//! Bounded application coordinator shared by all product transports. One Tokio
//! task serializes admission, snapshots and subscriptions; inference runs in an
//! owned task and retains the core's worker confinement and receipt finalization.
//! Slow subscribers resnapshot; they never backpressure inference. This view is
//! volatile presentation state, distinct from model context and durable history.

mod types;
pub use types::*;
#[cfg(target_os = "linux")]
mod owner;
#[cfg(target_os = "linux")]
pub use owner::OwnerHandle;

use crate::{
    chat::{
        Chat, ChatError, ChatResponse, ChatResult, RequestOptions, RunEvent, RunFailure, RunStatus,
    },
    engine::{Cancellation, EngineError},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};

/// Maximum UTF-8 bytes in one new user input.
pub const INPUT_BYTES: usize = 16 * 1024;
/// Maximum retained visible message count.
pub const MESSAGE_LIMIT: usize = 128;
/// Maximum visible text bytes in one assistant message.
pub const MESSAGE_BYTES: usize = 128 * 1024;
/// Maximum retained visible text bytes.
pub const TRANSCRIPT_BYTES: usize = 1024 * 1024;
/// Maximum remembered submission keys, including active work.
pub const REQUEST_LIMIT: usize = 128;
/// Minimum deduplication retention; capacity is rejected instead of early eviction.
pub const REQUEST_RETENTION: Duration = Duration::from_secs(600);
const EVENT_CAPACITY: usize = 64;

/// A snapshot and its subsequent updates, registered in one coordinator step.
#[derive(Debug)]
pub struct Subscription {
    /// State immediately before the first queued event.
    pub snapshot: Snapshot,
    /// Lagged receivers must discard their view and subscribe again.
    pub events: broadcast::Receiver<Arc<ApplicationEvent>>,
}

type Reply<T> = oneshot::Sender<Result<T, ApplicationError>>;
#[derive(Debug)]
enum Command {
    Submit(String, Submission, Reply<RunView>),
    Snapshot(Reply<Snapshot>),
    ChatId(Reply<String>),
    Subscribe(Reply<Subscription>),
    Run(String, Reply<RunView>),
    Cancel(String, Reply<RunView>),
    #[cfg(target_os = "linux")]
    Pending(String, String, String, Reply<bool>),
}

/// Restricted, cloneable entry point for transports. It owns no runtime shutdown
/// authority. Handles surviving shutdown return [`ApplicationError::Closed`].
#[derive(Debug, Clone)]
pub struct AssistantHandle {
    #[cfg(target_os = "linux")]
    memory: Option<crate::memory::service::MemoryAccess>,
    commands: mpsc::Sender<Command>,
    closed: Arc<AtomicBool>,
}
impl AssistantHandle {
    /// Host-issued owner access, present only when durable memory is required.
    /// Transport adapters must authenticate before exposing this handle.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn memory(&self) -> Option<crate::memory::service::MemoryAccess> {
        self.memory.clone()
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, ApplicationError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ApplicationError::Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .try_send(make(tx))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ApplicationError::Capacity,
                mpsc::error::TrySendError::Closed(_) => ApplicationError::Closed,
            })?;
        rx.await.map_err(|_| ApplicationError::Closed)?
    }
    /// Submit literal input. Scope is a trusted host-assigned transport identity.
    /// # Errors
    /// Rejects invalid input, conflicting keys, busy chats, capacity or shutdown.
    pub async fn submit(
        &self,
        scope: &str,
        input: Submission,
    ) -> Result<RunView, ApplicationError> {
        // Validate before queueing, so the command bound also bounds owned bytes.
        if !Coordinator::identifier(scope)
            || !Coordinator::identifier(&input.client_id)
            || !Coordinator::identifier(&input.request_id)
            || input.text.len() > INPUT_BYTES
            || input.text.trim().is_empty()
        {
            return Err(ApplicationError::InvalidInput);
        }
        self.call(|reply| Command::Submit(scope.into(), input, reply))
            .await
    }
    /// Inspect the bounded presentation view without waiting for inference.
    /// # Errors
    /// Reports coordinator capacity or shutdown.
    pub async fn snapshot(&self) -> Result<Snapshot, ApplicationError> {
        self.call(Command::Snapshot).await
    }
    /// Read the primary identity without copying the retained transcript.
    /// # Errors
    /// Reports coordinator capacity or shutdown.
    pub async fn chat_id(&self) -> Result<String, ApplicationError> {
        self.call(Command::ChatId).await
    }
    /// Atomically obtain a snapshot and its subsequent updates.
    /// # Errors
    /// Reports subscription/command capacity or shutdown.
    pub async fn subscribe(&self) -> Result<Subscription, ApplicationError> {
        self.call(Command::Subscribe).await
    }
    /// Inspect an active or recently completed interaction.
    /// # Errors
    /// Reports expired/unknown identity, capacity or shutdown.
    pub async fn run(&self, id: &str) -> Result<RunView, ApplicationError> {
        if id.len() > 128 {
            return Err(ApplicationError::NotFound);
        }
        let result = self.call(|reply| Command::Run(id.into(), reply)).await;
        #[cfg(target_os = "linux")]
        if matches!(result, Err(ApplicationError::NotFound))
            && let Some(memory) = &self.memory
        {
            let id = crate::memory::MemoryId::parse(id).map_err(|_| ApplicationError::NotFound)?;
            let exchange = memory
                .store
                .exchange(&memory.owner, id)
                .await
                .map_err(memory_application_error)?;
            let (_, input) = exchange_input(&exchange);
            return Ok(restored_run(
                &exchange.operation,
                &input,
                &format!("{}-primary", memory.owner.principal()),
            ));
        }
        result
    }
    /// Request cooperative cancellation. A terminal outcome remains unchanged.
    /// # Errors
    /// Reports expired/unknown identity, capacity or shutdown.
    pub async fn cancel(&self, id: &str) -> Result<RunView, ApplicationError> {
        if id.len() > 128 {
            return Err(ApplicationError::NotFound);
        }
        self.call(|reply| Command::Cancel(id.into(), reply)).await
    }
}

/// Runtime-owned coordinator. Explicit shutdown joins accepted interactions.
#[derive(Debug)]
pub(crate) struct Application {
    pub(crate) handle: AssistantHandle,
    stop: Cancellation,
    task: JoinHandle<ChatResult<()>>,
    finished: watch::Receiver<bool>,
    ready: Option<oneshot::Receiver<()>>,
}
impl Application {
    #[cfg(any(test, not(target_os = "linux")))]
    pub(crate) fn start(chat: Chat, options: RequestOptions) -> ChatResult<Self> {
        Self::start_inner(
            chat,
            options,
            #[cfg(target_os = "linux")]
            None,
        )
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn start_with_memory(
        chat: Chat,
        options: RequestOptions,
        memory: Option<crate::memory::service::MemoryAccess>,
    ) -> ChatResult<Self> {
        Self::start_inner(chat, options, memory)
    }
    fn start_inner(
        chat: Chat,
        options: RequestOptions,
        #[cfg(target_os = "linux")] memory: Option<crate::memory::service::MemoryAccess>,
    ) -> ChatResult<Self> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| ChatError::Configuration("instance identity unavailable".into()))?;
        let instance = random
            .iter()
            .flat_map(|byte| {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]
            })
            .collect::<String>();
        let (commands, incoming) = mpsc::channel(32);
        let closed = Arc::new(AtomicBool::new(false));
        let handle = AssistantHandle {
            #[cfg(target_os = "linux")]
            memory: memory.clone(),
            commands,
            closed: closed.clone(),
        };
        let stop = Cancellation::default();
        let mut actor = Coordinator::new(chat, options, instance, closed, stop.clone());
        #[cfg(target_os = "linux")]
        {
            actor.memory = memory;
        }
        let (completion, finished) = watch::channel(false);
        let (ready, readiness) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = actor.run(incoming, ready).await;
            completion.send_replace(true);
            result
        });
        Ok(Self {
            handle,
            stop,
            task,
            finished,
            ready: Some(readiness),
        })
    }
    pub(crate) async fn ready(&mut self) -> ChatResult<()> {
        if let Some(ready) = self.ready.take() {
            ready.await.map_err(|_| ChatError::Closed)?;
        }
        Ok(())
    }
    pub(crate) async fn terminated(&self) -> ChatResult<()> {
        let mut finished = self.finished.clone();
        loop {
            if *finished.borrow_and_update() {
                return Err(ChatError::Closed);
            }
            finished.changed().await.map_err(|_| ChatError::Closed)?;
        }
    }
    pub(crate) fn close(&self) {
        self.handle.closed.store(true, Ordering::Release);
    }
    pub(crate) async fn shutdown(&mut self) -> ChatResult<()> {
        self.close();
        self.stop.cancel();
        (&mut self.task)
            .await
            .map_err(|_| ChatError::Configuration("application task terminated".into()))?
    }
}
impl Drop for Application {
    fn drop(&mut self) {
        self.close();
        self.stop.cancel();
    }
}

#[derive(Debug)]
struct Record {
    scope: String,
    input: Submission,
    view: RunView,
    created: Instant,
}
#[derive(Debug)]
enum Progress {
    Metrics(usize, usize, crate::engine::GenerationMetrics),
    Started,
    Turn(crate::telemetry::ResourceBoundary),
    TurnFinished(usize, crate::telemetry::ResourceBoundary),
    Text(String),
    Activity(&'static str),
}
#[derive(Debug)]
struct QueuedProgress {
    progress: Progress,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Debug)]
struct Coordinator {
    #[cfg(target_os = "linux")]
    memory: Option<crate::memory::service::MemoryAccess>,
    #[cfg(target_os = "linux")]
    active_operation: Option<Arc<crate::memory::session::MemoryOperation>>,
    resource_sample: Option<crate::telemetry::ResourceSample>,
    inference: InferenceTotals,
    chat: Chat,
    options: RequestOptions,
    instance: String,
    chat_id: String,
    closed: Arc<AtomicBool>,
    stop: Cancellation,
    messages: VecDeque<DisplayMessage>,
    records: VecDeque<Record>,
    active: Option<(String, Cancellation)>,
    turn: usize,
    sequence: u64,
    run_sequence: u64,
    truncated: bool,
    projection_failed: bool,
    /// Deltas are coalesced for at most 20ms; snapshots and semantic boundaries flush.
    pending_delta: Option<(String, String)>,
    flush_at: tokio::time::Instant,
    outgoing: broadcast::Sender<Arc<ApplicationEvent>>,
}
impl Coordinator {
    fn new(
        chat: Chat,
        options: RequestOptions,
        instance: String,
        closed: Arc<AtomicBool>,
        stop: Cancellation,
    ) -> Self {
        let (outgoing, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            #[cfg(target_os = "linux")]
            memory: None,
            #[cfg(target_os = "linux")]
            active_operation: None,
            resource_sample: None,
            inference: InferenceTotals::default(),
            chat,
            options,
            chat_id: format!("{instance}-primary"),
            instance,
            closed,
            stop,
            messages: VecDeque::new(),
            records: VecDeque::new(),
            active: None,
            turn: 0,
            sequence: 0,
            run_sequence: 0,
            truncated: false,
            projection_failed: false,
            pending_delta: None,
            flush_at: tokio::time::Instant::now(),
            outgoing,
        }
    }
    fn cursor(&self) -> String {
        format!("{}:{}", self.instance, self.sequence)
    }
    fn publish(&mut self, update: Update) {
        self.flush_pending();
        self.emit(update);
    }
    fn emit(&mut self, update: Update) {
        self.sequence += 1;
        // No receivers is ordinary idle operation. State remains authoritative.
        let _ = self.outgoing.send(Arc::new(ApplicationEvent {
            id: self.cursor(),
            update,
        }));
    }
    fn flush_pending(&mut self) {
        if let Some((message_id, text)) = self.pending_delta.take() {
            self.emit(Update::Delta { message_id, text });
        }
    }
    fn queue_delta(&mut self, message_id: String, text: &str) {
        if self.pending_delta.as_ref().is_some_and(|(id, pending)| {
            id != &message_id || pending.len().saturating_add(text.len()) > 16 * 1024
        }) {
            self.flush_pending();
        }
        if self.pending_delta.is_none() {
            self.flush_at = tokio::time::Instant::now() + Duration::from_millis(20);
            self.pending_delta = Some((message_id, String::new()));
        }
        if let Some((_, pending)) = &mut self.pending_delta {
            pending.push_str(text);
        }
        if self
            .pending_delta
            .as_ref()
            .is_some_and(|(_, pending)| pending.len() >= 16 * 1024)
        {
            self.flush_pending();
        }
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            resources: self.resource_sample.clone(),
            inference: self.inference.clone(),
            instance_id: self.instance.clone(),
            chat_id: self.chat_id.clone(),
            revision: self.cursor(),
            messages: self.messages.iter().cloned().collect(),
            recent_runs: self.records.iter().map(|r| r.view.clone()).collect(),
            active_run: self
                .active
                .as_ref()
                .and_then(|(id, _)| self.record(id).ok()),
            truncated: self.truncated,
        }
    }
    fn record(&self, id: &str) -> Result<RunView, ApplicationError> {
        self.records
            .iter()
            .find(|record| record.view.id == id)
            .map(|r| r.view.clone())
            .ok_or(ApplicationError::NotFound)
    }
    fn identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    }
    fn check_admission(
        &mut self,
        scope: &str,
        input: &Submission,
    ) -> Result<Option<RunView>, ApplicationError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ApplicationError::Closed);
        }
        if !Self::identifier(scope)
            || !Self::identifier(&input.client_id)
            || !Self::identifier(&input.request_id)
            || input.text.len() > INPUT_BYTES
            || input.text.trim().is_empty()
        {
            return Err(ApplicationError::InvalidInput);
        }
        let visible = self
            .messages
            .iter()
            .map(|message| message.run_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        self.records.retain(|record| {
            !record.view.status.is_terminal()
                || record.created.elapsed() < REQUEST_RETENTION
                || visible.contains(record.view.id.as_str())
        });
        if let Some(record) = self.records.iter().find(|r| {
            r.scope == scope
                && r.input.client_id == input.client_id
                && r.input.request_id == input.request_id
        }) {
            return if record.input.text == input.text {
                Ok(Some(record.view.clone()))
            } else {
                Err(ApplicationError::Conflict)
            };
        }
        if self.active.is_some() {
            return Err(ApplicationError::Busy);
        }
        if self.records.len() >= REQUEST_LIMIT {
            return Err(ApplicationError::Capacity);
        }
        Ok(None)
    }
    async fn admit(
        &mut self,
        scope: String,
        input: Submission,
    ) -> Result<(RunView, bool), ApplicationError> {
        if let Some(view) = self.check_admission(&scope, &input)? {
            return Ok((view, false));
        }
        self.run_sequence += 1;
        let id = format!("{}-r{}", self.instance, self.run_sequence);
        self.install_admission(scope, input, id)
    }
    fn install_admission(
        &mut self,
        scope: String,
        input: Submission,
        id: String,
    ) -> Result<(RunView, bool), ApplicationError> {
        let mut resources = crate::telemetry::ResourceWindow::default();
        if let Some(sample) = &self.resource_sample {
            resources.observe(sample);
        }
        resources.started = Some(crate::telemetry::boundary());
        let view = RunView {
            client_identity_available: true,
            usage_available: false,
            outcome_details_available: false,
            performance: Vec::new(),
            resources,
            limit_reason: None,
            id: id.clone(),
            chat_id: self.chat_id.clone(),
            client_id: input.client_id.clone(),
            client_request_id: input.request_id.clone(),
            status: Status::Accepted,
            error: None,
            input_tokens: 0,
            generated_tokens: 0,
            warning_count: 0,
            unknown_tool_outcome: false,
        };
        self.active = Some((id.clone(), Cancellation::default()));
        self.turn = 0;
        self.projection_failed = false;
        self.records.push_back(Record {
            scope,
            input: input.clone(),
            view: view.clone(),
            created: Instant::now(),
        });
        self.publish(Update::Run {
            run: Box::new(view.clone()),
        });
        self.add_message(DisplayMessage {
            id: format!("{id}-user"),
            run_id: id,
            role: "user",
            text: input.text,
            status: Status::Accepted,
        });
        Ok((view, true))
    }
    fn trim(&mut self) {
        self.trim_for(0);
    }
    fn trim_for(&mut self, additional_bytes: usize) {
        let mut removed = Vec::new();
        while self.messages.len() > MESSAGE_LIMIT
            || self
                .messages
                .iter()
                .map(|m| m.text.len())
                .sum::<usize>()
                .saturating_add(additional_bytes)
                > TRANSCRIPT_BYTES
        {
            let Some(first) = self.messages.front() else {
                break;
            };
            if self
                .active
                .as_ref()
                .is_some_and(|(id, _)| id == &first.run_id)
            {
                break;
            }
            let old_run = first.run_id.clone();
            while self.messages.front().is_some_and(|m| m.run_id == old_run) {
                if let Some(message) = self.messages.pop_front() {
                    removed.push(message.id);
                }
            }
        }
        if !removed.is_empty() {
            self.truncated = true;
            self.publish(Update::Evicted {
                message_ids: removed,
            });
        }
    }
    fn add_message(&mut self, message: DisplayMessage) {
        self.messages.push_back(message.clone());
        self.publish(Update::Message { message });
        self.trim();
    }
    fn assistant_id(&self, id: &str) -> String {
        format!("{id}-assistant-{}", self.turn)
    }
    fn progress(&mut self, progress: Progress) {
        let Some((id, cancellation)) = self.active.clone() else {
            return;
        };
        match progress {
            Progress::Metrics(turn, generated_tokens, timing) => {
                if let Some(record) = self.records.back_mut()
                    && let Some(performance) =
                        record.view.performance.iter_mut().find(|p| p.turn == turn)
                {
                    performance.generated_tokens = generated_tokens;
                    performance.timing = Some(timing);
                }
            }
            Progress::Started => {
                if let Some(record) = self.records.back_mut() {
                    if record.view.status != Status::Stopping {
                        record.view.status = Status::Running;
                    }
                    let run = record.view.clone();
                    self.publish(Update::Run { run: Box::new(run) });
                }
            }
            Progress::TurnFinished(turn, boundary) => {
                if let Some(record) = self.records.back_mut()
                    && let Some(view) = record.view.performance.iter_mut().find(|p| p.turn == turn)
                {
                    view.resources.ended = Some(boundary);
                    view.measurement_complete = true;
                }
            }
            Progress::Turn(boundary) => {
                self.turn += 1;
                if let Some(record) = self.records.back_mut() {
                    let mut resources = crate::telemetry::ResourceWindow::default();
                    if let Some(sample) = &self.resource_sample {
                        resources.observe(sample);
                    }
                    resources.started = Some(boundary);
                    record.view.performance.push(PerformanceView {
                        measurement_complete: false,
                        turn: self.turn,
                        generated_tokens: 0,
                        timing: None,
                        resources,
                    });
                }
            }
            Progress::Activity(label) => self.publish(Update::Activity { run_id: id, label }),
            Progress::Text(text) => {
                let message_id = self.assistant_id(&id);
                if !self.messages.iter().any(|m| m.id == message_id) {
                    self.add_message(DisplayMessage {
                        id: message_id.clone(),
                        run_id: id,
                        role: "assistant",
                        text: String::new(),
                        status: Status::Running,
                    });
                }
                // Keep even malicious extension output within the projection budget.
                let message_bytes = self
                    .messages
                    .iter()
                    .find(|m| m.id == message_id)
                    .map_or(0, |m| m.text.len());
                self.trim_for(text.len());
                let retained = self.messages.iter().map(|m| m.text.len()).sum::<usize>();
                if retained.saturating_add(text.len()) > TRANSCRIPT_BYTES
                    || message_bytes.saturating_add(text.len()) > MESSAGE_BYTES
                {
                    self.projection_failed = true;
                    cancellation.cancel();
                    return;
                }
                if let Some(message) = self.messages.iter_mut().find(|m| m.id == message_id) {
                    message.text.push_str(&text);
                }
                self.queue_delta(message_id, &text);
                self.trim();
            }
        }
    }
    fn cancel(&mut self, id: &str) -> Result<RunView, ApplicationError> {
        let mut view = self.record(id)?;
        if let Some((active, cancellation)) = &self.active
            && active == id
        {
            cancellation.cancel();
            view.status = Status::Stopping;
            if let Some(record) = self.records.iter_mut().find(|r| r.view.id == id) {
                record.view = view.clone();
            }
            self.publish(Update::Run {
                run: Box::new(view.clone()),
            });
        }
        Ok(view)
    }
    /// Successful final text is authoritative. On interruption the core response
    /// can refer to an earlier complete tool turn, so preserve delivered per-turn
    /// text instead of guessing which message owns buffered partial progress.
    fn reconcile_final_message(&mut self, id: &str, response: &ChatResponse) {
        if !matches!(response.status, RunStatus::Completed | RunStatus::Truncated) {
            return;
        }
        if response.message.content.len() > MESSAGE_BYTES {
            self.projection_failed = true;
            return;
        }
        let message_id = self.assistant_id(id);
        let current = self.messages.iter().find(|m| m.id == message_id);
        let previous = current.map_or(0, |m| m.text.len());
        let final_bytes = response.message.content.len();
        self.trim_for(final_bytes.saturating_sub(previous));
        let retained = self
            .messages
            .iter()
            .map(|m| m.text.len())
            .sum::<usize>()
            .saturating_sub(previous);
        if retained.saturating_add(final_bytes) > TRANSCRIPT_BYTES {
            self.projection_failed = true;
        }
        if !self.projection_failed && !response.message.content.is_empty() {
            if let Some(message) = self.messages.iter_mut().find(|m| m.id == message_id) {
                message.text.clone_from(&response.message.content);
            } else {
                self.messages.push_back(DisplayMessage {
                    id: message_id,
                    run_id: id.into(),
                    role: "assistant",
                    text: response.message.content.clone(),
                    status: Status::Running,
                });
            }
        }
    }
    fn finish(&mut self, result: Result<ChatResponse, RunFailure>) {
        let Some((id, _)) = self.active.clone() else {
            return;
        };
        let (response, mut error) = match result {
            Ok(response) => (response, None),
            Err(failure) => {
                let error = match &failure.source {
                    ChatError::Cancelled => "cancelled",
                    ChatError::Deadline => "deadline",
                    ChatError::Closed => "closed",
                    ChatError::Engine(EngineError::Budget { .. }) => "context_budget_exceeded",
                    ChatError::Limit(_) => "limit_exceeded",
                    _ => "generation_failed",
                };
                (*failure.partial, Some(error))
            }
        };
        let mut status = match response.status {
            RunStatus::Completed => Status::Completed,
            RunStatus::Truncated => Status::Truncated,
            RunStatus::Cancelled => Status::Cancelled,
            RunStatus::Deadline => Status::Deadline,
            RunStatus::Failed => Status::Failed,
        };
        self.reconcile_final_message(&id, &response);
        if self.projection_failed {
            status = Status::Failed;
            error = Some("limit_exceeded");
        }
        // Reasoning-only truncation and failures before the first visible token
        // still need a terminal display entry. Its text stays empty: host status
        // must never be fabricated as model output or enter working context.
        let message_id = self.assistant_id(&id);
        if status != Status::Completed
            && !self.messages.iter().any(|message| message.id == message_id)
        {
            self.add_message(DisplayMessage {
                id: message_id,
                run_id: id.clone(),
                role: "assistant",
                text: String::new(),
                status,
            });
        }
        let changed = self
            .messages
            .iter_mut()
            .filter(|m| m.run_id == id)
            .map(|message| {
                message.status = status;
                message.clone()
            })
            .collect::<Vec<_>>();
        for message in changed {
            self.publish(Update::Message { message });
        }
        if let Some(record) = self.records.iter_mut().find(|r| r.view.id == id) {
            complete_measurements(
                &mut record.view,
                &response,
                &mut self.inference,
                self.resource_sample.as_ref(),
            );
            record.view.usage_available = true;
            record.view.outcome_details_available = true;
            record.view.limit_reason = response.limit_reason;
            record.view.status = status;
            record.view.error = error;
            record.view.input_tokens = response.usage.input_tokens;
            record.view.generated_tokens = response.usage.generated_tokens;
            record.view.warning_count = response.warnings.len();
            record.view.unknown_tool_outcome = response.tools.iter().any(|receipt| {
                receipt
                    .result
                    .get("error")
                    .and_then(|error| error.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("interrupted")
            });
            let run = record.view.clone();
            self.publish(Update::Run { run: Box::new(run) });
        }
        self.active = None;
        #[cfg(target_os = "linux")]
        {
            self.active_operation = None;
        }
        if let Some(sample) = self.resource_sample.clone() {
            self.publish(Update::Telemetry {
                sample,
                inference: self.inference.clone(),
            });
        }
        self.trim();
    }
    /// Enqueue only presentation progress; byte permits include queued text and
    /// callbacks never await socket consumers or coordinator capacity.
    fn request(
        &self,
        text: String,
        cancellation: Cancellation,
        tx: mpsc::Sender<QueuedProgress>,
    ) -> crate::chat::Request {
        let mut options = self.options.clone();
        options.cancellation = cancellation;

        let bytes = Arc::new(Semaphore::new(256 * 1024));
        let request = self.chat.request(text);
        #[cfg(target_os = "linux")]
        let request = request.memory_operation(self.active_operation.clone());
        request.options(options).on_event(move |event| {
            let progress = match event {
                RunEvent::ModelProgress {
                    turn,
                    usage,
                    metrics,
                    ..
                } => Progress::Metrics(*turn, usage.generated_tokens, metrics.clone()),
                RunEvent::Started { .. } => Progress::Started,
                RunEvent::ModelTurnStarted { .. } => Progress::Turn(crate::telemetry::boundary()),
                RunEvent::ModelTurnFinished { turn, .. } => {
                    Progress::TurnFinished(*turn, crate::telemetry::boundary())
                }
                RunEvent::TextDelta { text, .. } => Progress::Text(text.clone()),
                RunEvent::ToolStarted { .. } => Progress::Activity("Using a tool"),
                RunEvent::ToolFinished { .. } => Progress::Activity("Thinking"),
                _ => return Ok(()),
            };
            let cost = match &progress {
                Progress::Text(text) => text.len().max(1),
                _ => 1,
            };
            let cost =
                u32::try_from(cost).map_err(|_| ChatError::Limit("application event bytes"))?;
            let permit = bytes
                .clone()
                .try_acquire_many_owned(cost)
                .map_err(|_| ChatError::Limit("application event bytes"))?;
            tx.try_send(QueuedProgress {
                progress,
                _bytes: permit,
            })
            .map_err(|_| ChatError::Limit("application event queue"))
        })
    }
    fn observe(&mut self, sample: crate::telemetry::ResourceSample) {
        self.resource_sample = Some(sample.clone());
        if self.active.is_some()
            && let Some(record) = self.records.back_mut()
        {
            record.view.resources.observe(&sample);
            if let Some(turn) = record.view.performance.last_mut() {
                // Completed native turns keep their exact CPU terminal boundary.
                if !turn.measurement_complete {
                    turn.resources.observe(&sample);
                }
            }
            let run = record.view.clone();
            self.publish(Update::Run { run: Box::new(run) });
        }
        self.publish(Update::Telemetry {
            sample,
            inference: self.inference.clone(),
        });
    }
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        ready: oneshot::Sender<()>,
    ) -> ChatResult<()> {
        #[cfg(target_os = "linux")]
        if let Some(memory) = self.memory.clone() {
            self.chat_id = format!("{}-primary", memory.owner.principal());
            let page = memory.store.history(&memory.owner, None, 32).await?;
            self.truncated = page.next_before.is_some();
            for exchange in page.exchanges.into_iter().rev() {
                let (scope, input) = exchange_input(&exchange);
                let view = restored_run(&exchange.operation, &input, &self.chat_id);
                let id = view.id.clone();
                self.add_message(DisplayMessage {
                    id: format!("{id}-user"),
                    run_id: id.clone(),
                    role: "user",
                    text: input.text.clone(),
                    status: view.status,
                });
                if let Some(source) = exchange.assistant {
                    self.add_message(DisplayMessage {
                        id: format!("{id}-assistant-0"),
                        run_id: id,
                        role: "assistant",
                        text: source.source.text.as_str().to_owned(),
                        status: view.status,
                    });
                }
                self.records.push_back(Record {
                    scope,
                    input,
                    view,
                    created: Instant::now(),
                });
            }
        }
        let _receiver_gone = ready.send(());
        let (progress_tx, mut progress_rx) = mpsc::channel::<QueuedProgress>(256);
        let mut runs = JoinSet::new();
        #[cfg(target_os = "linux")]
        let mut admissions = JoinSet::new();
        #[cfg(target_os = "linux")]
        let mut pending: Option<(String, Submission, Vec<Reply<RunView>>)> = None;
        let mut stopping = false;
        let mut sampler = Some(crate::telemetry::Sampler::default());
        let mut observations = JoinSet::new();
        let mut sample_at = tokio::time::Instant::now();
        loop {
            #[cfg(target_os = "linux")]
            let admission_pending = !admissions.is_empty();
            #[cfg(not(target_os = "linux"))]
            let admission_pending = false;
            tokio::select! {
                () = tokio::time::sleep_until(sample_at), if sampler.is_some() && (self.active.is_some() || self.outgoing.receiver_count() > 0) && !stopping => {
                    if let Some(mut provider) = sampler.take() {
                        let devices = self.chat.model_gpu_scope();
                        observations.spawn_blocking(move || { let sample = provider.sample(devices.as_deref()); (provider, sample) });
                    }
                }
                Some(result) = observations.join_next(), if !observations.is_empty() => {
                    let (provider, sample) = result.map_err(|_| ChatError::Configuration("telemetry provider task failed".into()))?;
                    sampler = Some(provider);
                    self.observe(sample);
                    sample_at = tokio::time::Instant::now() + if self.active.is_some() { Duration::from_millis(250) } else { Duration::from_secs(1) };
                }
                () = tokio::time::sleep_until(self.flush_at), if self.pending_delta.is_some() => self.flush_pending(),
                () = self.stop.cancelled(), if !stopping => {
                    stopping = true; self.closed.store(true, Ordering::Release);
                    if let Some((_, cancellation)) = &self.active { cancellation.cancel(); }
                }
                command = commands.recv(), if !stopping => {
                    let Some(command) = command else { self.stop.cancel(); continue };
                    match command {
                        Command::Snapshot(reply) => { self.flush_pending(); let _ = reply.send(Ok(self.snapshot())); }
                        Command::ChatId(reply) => { let _ = reply.send(Ok(self.chat_id.clone())); }
                        Command::Subscribe(reply) => {
                            self.flush_pending();
                            let result = if self.outgoing.receiver_count() >= 32 { Err(ApplicationError::Capacity) }
                                else { Ok(Subscription { snapshot: self.snapshot(), events: self.outgoing.subscribe() }) };
                            let _ = reply.send(result);
                        }
                        Command::Run(id, reply) => { let _ = reply.send(self.record(&id)); }
                        Command::Cancel(id, reply) => { let result = self.cancel(&id); let _ = reply.send(result); }
                        #[cfg(target_os = "linux")]
                        Command::Pending(scope, client, request, reply) => {
                            let committing = pending.as_ref().is_some_and(|(s, input, _)| *s == scope && input.client_id == client && input.request_id == request);
                            let _ = reply.send(Ok(committing));
                        }
                        Command::Submit(scope, input, reply) => {
                            #[cfg(target_os = "linux")]
                            if let Some(memory) = self.memory.clone() {
                                if let Some((pending_scope, pending_input, replies)) = &mut pending {
                                    let same_key = *pending_scope == scope && pending_input.client_id == input.client_id && pending_input.request_id == input.request_id;
                                    if same_key && pending_input.text == input.text && replies.len() < 32 { replies.push(reply); }
                                    else { let _ = reply.send(Err(if same_key && pending_input.text == input.text { ApplicationError::Capacity } else if same_key { ApplicationError::Conflict } else { ApplicationError::Busy })); }
                                    continue;
                                }
                                let admission = self.check_admission(&scope, &input);
                                let task_scope = scope.clone();
                                let task_input = input.clone();
                                // Own the whole durable lookup/admission path before
                                // its first database await, independently of waiters.
                                // Existing keys remain reconcilable while a new run is busy.
                                admissions.spawn(async move {
                                    if let Err(rejection) = admission {
                                        let key=crate::memory::ClientRequestKey::new(&task_scope,&task_input.client_id,&task_input.request_id).map_err(memory_application_error)?;
                                        let record=match memory.store.operation_by_key(&memory.owner,key).await {
                                            Ok(record)=>record,
                                            Err(crate::memory::MemoryError::NotFound)=>return Err(rejection),
                                            Err(error)=>return Err(memory_application_error(error)),
                                        };
                                        let source=memory.source(record.input.id).await.map_err(memory_application_error)?;
                                        if source.source.text.as_str()!=task_input.text {return Err(ApplicationError::Conflict);}
                                        return Ok((record,false));
                                    }
                                    memory.admit_product(&task_scope, &task_input.client_id, &task_input.request_id, &task_input.text).await.map_err(memory_application_error)
                                });
                                pending = Some((scope, input, vec![reply]));
                                continue;
                            }
                            let text = input.text.clone();
                            let admitted = self.admit(scope, input).await;
                            if let Ok((_, true)) = &admitted
                                && let Some((_, cancellation)) = &self.active {
                                    let request = self.request(text, cancellation.clone(), progress_tx.clone());
                                    runs.spawn(request.run());
                            }
                            let _ = reply.send(admitted.map(|(view, _)| view));
                        }
                    }
                }
                result = async {
                    #[cfg(target_os = "linux")]
                    { admissions.join_next().await }
                    #[cfg(not(target_os = "linux"))]
                    { std::future::pending::<Option<Result<(), tokio::task::JoinError>>>().await }
                }, if admission_pending => {
                    #[cfg(target_os = "linux")]
                    {
                    let Some(result) = result else { continue };
                    let (scope, input, replies) = pending.take().ok_or_else(|| ChatError::Configuration("admission ownership missing".into()))?;
                    let record = result.map_err(|_| ChatError::Configuration("admission task terminated".into()))?;
                    let admitted = match record {
                        Err(error) => Err(error),
                        Ok((record, false)) => Ok((self.record(&record.id.to_string()).unwrap_or_else(|_|restored_run(&record, &input, &self.chat_id)), false)),
                        Ok((record, true)) => {
                            let memory = self.memory.clone().ok_or_else(|| ChatError::Configuration("admission memory missing".into()))?;
                            let id = record.id.to_string();
                            self.active_operation = Some(crate::memory::session::MemoryOperation::new(record, memory));
                            let text = input.text.clone();
                            let accepted = self.install_admission(scope, input, id);
                            if let Some((_, cancellation)) = &self.active {
                                if stopping { cancellation.cancel(); }
                                runs.spawn(self.request(text, cancellation.clone(), progress_tx.clone()).run());
                            }
                            accepted
                        }
                    };
                    for reply in replies { let _ = reply.send(admitted.as_ref().map(|(view, _)| view.clone()).map_err(|error| *error)); }
                    }
                }
                Some(queued) = progress_rx.recv(), if self.active.is_some() => { self.progress(queued.progress); }
                Some(result) = runs.join_next(), if !runs.is_empty() => {
                    while let Ok(queued) = progress_rx.try_recv() { self.progress(queued.progress); }
                    match result {
                        Ok(result) => {
                            // Finish resources at the execution boundary, rather than
                            // attributing the next idle interval to this message.
                            if let Some(sampled) = observations.join_next().await {
                                let (provider, sample) = sampled.map_err(|_| ChatError::Configuration("telemetry provider task failed".into()))?;
                                sampler = Some(provider); self.observe(sample);
                            }
                            if let Some(mut provider) = sampler.take() {
                                let devices = self.chat.model_gpu_scope();
                                let (provider, sample) = tokio::task::spawn_blocking(move || { let sample = provider.sample(devices.as_deref()); (provider, sample) }).await.map_err(|_| ChatError::Configuration("telemetry provider task failed".into()))?;
                                sampler = Some(provider); self.observe(sample);
                            }
                            self.finish(result);
                        },
                        Err(_) => return Err(ChatError::Configuration("application run task terminated".into())),
                    }
                }
            }
            #[cfg(target_os = "linux")]
            let admission_done = admissions.is_empty();
            #[cfg(not(target_os = "linux"))]
            let admission_done = true;
            if stopping && runs.is_empty() && admission_done {
                break;
            }
        }
        while observations.join_next().await.is_some() {}
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn exchange_input(exchange: &crate::memory::OperationExchange) -> (String, Submission) {
    let [scope, client_id, request_id] = exchange
        .client_key
        .clone()
        .map_or_else(|| [String::new(), String::new(), String::new()], Into::into);
    (
        scope,
        Submission {
            client_id,
            request_id,
            text: exchange.input.source.text.as_str().to_owned(),
        },
    )
}

/// Close observed process windows and fold exact measured native counters. Keeping
/// this apart from status projection prevents terminal errors from losing metrics.
fn complete_measurements(
    view: &mut RunView,
    response: &ChatResponse,
    inference: &mut InferenceTotals,
    sample: Option<&crate::telemetry::ResourceSample>,
) {
    if let Some(sample) = sample
        && let Some(turn) = view.performance.last_mut()
    {
        let ended = turn.resources.ended.clone();
        turn.resources.observe(sample);
        if turn.measurement_complete {
            turn.resources.ended = ended;
        }
    }
    view.resources.ended = Some(crate::telemetry::boundary());
    if let Some(turn) = view.performance.last_mut()
        && !turn.measurement_complete
    {
        turn.resources.ended = Some(crate::telemetry::boundary());
    }
    for measured in &response.performance {
        if let Some(view) = view
            .performance
            .iter_mut()
            .find(|p| p.turn == measured.turn)
        {
            view.measurement_complete = measured.measurement_complete;
            view.generated_tokens = measured.generated_tokens;
            view.timing.clone_from(&measured.timing);
        }
        if let Some(timing) = &measured.timing {
            inference.generated_tokens = inference
                .generated_tokens
                .saturating_add(measured.generated_tokens as u64);
            inference.generation_ns = inference.generation_ns.saturating_add(timing.generation_ns);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
fn memory_application_error(error: crate::memory::MemoryError) -> ApplicationError {
    use crate::memory::MemoryError;
    match error {
        MemoryError::InvalidInput(_) => ApplicationError::InvalidInput,
        MemoryError::Forbidden => ApplicationError::Unauthorized,
        MemoryError::Conflict => ApplicationError::Conflict,
        MemoryError::Busy => ApplicationError::Busy,
        MemoryError::Capacity | MemoryError::StorageFull => ApplicationError::Capacity,
        MemoryError::NotFound => ApplicationError::NotFound,
        MemoryError::Gone => ApplicationError::Gone,
        _ => ApplicationError::StorageUnavailable,
    }
}
#[cfg(target_os = "linux")]
fn restored_run(
    record: &crate::memory::OperationRecord,
    input: &Submission,
    chat: &str,
) -> RunView {
    use crate::memory::OperationState;
    let status = if let Some(details) = &record.details {
        match details.kind {
            crate::memory::CompletionKind::Completed => Status::Completed,
            crate::memory::CompletionKind::Truncated => Status::Truncated,
            crate::memory::CompletionKind::Cancelled => Status::Cancelled,
            crate::memory::CompletionKind::Deadline => Status::Deadline,
            crate::memory::CompletionKind::Failed | crate::memory::CompletionKind::Interrupted => {
                Status::Failed
            }
        }
    } else {
        match record.state {
            OperationState::Accepted => Status::Accepted,
            OperationState::Running => Status::Running,
            OperationState::Completed => Status::Completed,
            OperationState::Cancelled => Status::Cancelled,
            OperationState::Failed | OperationState::Interrupted => Status::Failed,
        }
    };
    let input_tokens = record
        .details
        .as_ref()
        .and_then(|details| details.input_tokens)
        .and_then(|v| usize::try_from(v).ok());
    let generated_tokens = record
        .details
        .as_ref()
        .and_then(|details| details.generated_tokens)
        .and_then(|v| usize::try_from(v).ok());
    let limit_reason = record
        .details
        .as_ref()
        .and_then(|details| details.limit)
        .map(|limit| match limit {
            crate::memory::CompletionLimit::OutputTokens => "output_tokens",
            crate::memory::CompletionLimit::ContextCapacity => "context_capacity",
            crate::memory::CompletionLimit::RunTokens => "run_tokens",
        });
    RunView {
        client_identity_available: !input.client_id.is_empty(),
        usage_available: input_tokens.is_some() && generated_tokens.is_some(),
        outcome_details_available: record.details.is_some(),
        performance: Vec::new(),
        resources: crate::telemetry::ResourceWindow::default(),
        limit_reason,
        id: record.id.to_string(),
        chat_id: chat.into(),
        client_id: input.client_id.clone(),
        client_request_id: input.request_id.clone(),
        status,
        error: if record.state == OperationState::Interrupted {
            Some("interrupted")
        } else {
            None
        },
        input_tokens: input_tokens.unwrap_or(0),
        generated_tokens: generated_tokens.unwrap_or(0),
        warning_count: 1,
        unknown_tool_outcome: record.unknown_tool_outcome,
    }
}
