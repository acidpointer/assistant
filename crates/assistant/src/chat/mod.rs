//! Simple conversational API over a bounded model/tool runner. Working messages
//! are short-lived state, never permanent memory. Concurrent handles serialize
//! updates to the same chat; different chats keep independent native contexts.
//! Failed runs preserve actual tool receipts and never automatically replay them.
pub(crate) mod runner;
mod types;
use crate::{
    engine::{Cancellation, Usage},
    pipeline::{BoxFuture, NextRun, Pipeline},
    runtime::{RunGuard, RuntimeControl},
};
pub use runner::{RunContext, run_loop};
use std::{fmt, sync::Arc, time::Instant};
use tokio::sync::Mutex;
pub use types::*;

pub(crate) trait ModelBackend: fmt::Debug + Send + Sync {
    fn default_max_tokens(&self) -> usize;
    fn default_thinking(&self) -> crate::engine::Thinking;
    fn evaluate(
        &self,
        request: ModelRequest,
        turn: TurnContext,
        events: EventSink,
    ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>>;
    fn wake(&self) {}
}
#[derive(Debug, Clone)]
pub(crate) struct TurnContext {
    pub chat_id: u64,
    pub run_id: u64,
    pub turn: usize,
    pub lease: Arc<()>,
    pub limits: RunLimits,
    pub cancellation: Cancellation,
    pub deadline: Instant,
}
#[derive(Debug)]
pub(crate) struct ModelFailure {
    pub source: ChatError,
    pub partial: Option<(AssistantMessage, Usage)>,
}
#[derive(Debug, Default)]
struct Progress {
    message: AssistantMessage,
    usage: Usage,
}
type EventHandler = Arc<dyn Fn(&RunEvent) -> ChatResult<()> + Send + Sync>;
#[derive(Clone, Default)]
pub(crate) struct EventSink {
    handler: Option<EventHandler>,
    progress: Arc<Mutex<Progress>>,
}
impl fmt::Debug for EventSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSink")
            .field("has_handler", &self.handler.is_some())
            .finish_non_exhaustive()
    }
}
impl EventSink {
    pub(crate) async fn emit(&self, event: RunEvent) -> ChatResult<()> {
        {
            let mut progress = self.progress.lock().await;
            match &event {
                RunEvent::TextDelta { text, .. } => progress.message.content.push_str(text),
                RunEvent::ReasoningDelta { text, .. } => progress
                    .message
                    .reasoning
                    .get_or_insert_with(String::new)
                    .push_str(text),
                _ => {}
            }
        }
        if let Some(handler) = &self.handler {
            handler(&event)?;
        }
        Ok(())
    }
    pub(crate) async fn reset(&self) {
        *self.progress.lock().await = Progress::default();
    }
    pub(crate) async fn usage(&self, usage: Usage) {
        self.progress.lock().await.usage = usage;
    }
}
#[derive(Debug)]
struct WorkingState {
    system: String,
    messages: Vec<Message>,
}
#[derive(Debug)]
struct ChatInner {
    id: u64,
    lease: Arc<()>,
    state: Mutex<WorkingState>,
    backend: Arc<dyn ModelBackend>,
    pipeline: Arc<Pipeline>,
    limits: RunLimits,
    control: Arc<RuntimeControl>,
}
impl Drop for ChatInner {
    fn drop(&mut self) {
        self.backend.wake();
    }
}
/// Cheaply cloned handle to one working conversation. Calls on clones serialize;
/// cancellation remains independent and native state never crosses threads.
#[derive(Debug, Clone)]
pub struct Chat(Arc<ChatInner>);
impl Chat {
    pub(crate) fn new(
        id: u64,
        system: String,
        backend: Arc<dyn ModelBackend>,
        pipeline: Arc<Pipeline>,
        limits: RunLimits,
        control: Arc<RuntimeControl>,
    ) -> Self {
        Self(Arc::new(ChatInner {
            id,
            lease: Arc::new(()),
            state: Mutex::new(WorkingState {
                system,
                messages: Vec::new(),
            }),
            backend,
            pipeline,
            limits,
            control,
        }))
    }
    /// Generate a final response, automatically driving selected tools.
    /// # Errors
    /// Returns a typed failure with partial response and tool receipts.
    pub async fn generate(&self, input: impl Into<String>) -> Result<ChatResponse, RunFailure> {
        self.request(input).run().await
    }
    /// Configure a single operation. Dropping the builder has no effects.
    #[must_use]
    pub fn request(&self, input: impl Into<String>) -> Request {
        Request {
            chat: self.clone(),
            input: input.into(),
            options: RequestOptions::default(),
            events: EventSink::default(),
        }
    }
    /// Replace trusted host instructions for subsequent requests.
    /// # Errors
    /// Rejects oversized instructions or a closed runtime.
    pub async fn set_system(&self, text: impl Into<String>) -> ChatResult<()> {
        let text = text.into();
        if text.len() > self.0.limits.input_bytes {
            return Err(ChatError::Limit("system bytes"));
        }
        self.0.control.check_open()?;
        let mut state = self.0.state.lock().await;
        self.0.control.check_open()?;
        state.system = text;
        Ok(())
    }
    /// Inspect bounded working messages, including recorded tool results.
    pub async fn messages(&self) -> Vec<Message> {
        self.0.state.lock().await.messages.clone()
    }
    /// Explicitly clear recent interaction state. Permanent memory is untouched.
    pub async fn clear_working_history(&self) {
        self.0.state.lock().await.messages.clear();
    }
    /// Public chat identity, stable for this runtime's lifetime.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.0.id
    }
}
/// Fluent per-operation configuration, sharing the same path as [`Chat::generate`].
#[derive(Debug)]
pub struct Request {
    chat: Chat,
    input: String,
    options: RequestOptions,
    events: EventSink,
}
impl Request {
    /// Select raw diagnostic completion or structured chat.
    #[must_use]
    pub fn mode(mut self, mode: InputMode) -> Self {
        self.options.mode = mode;
        self
    }
    /// Configure whether the first turn must call a tool.
    #[must_use]
    pub fn tool_use(mut self, mode: ToolUse) -> Self {
        self.options.tool_use = mode;
        self
    }
    /// Override the model-turn output allowance.
    #[must_use]
    pub fn max_new_tokens(mut self, tokens: usize) -> Self {
        self.options.max_new_tokens = Some(tokens);
        self
    }
    /// Add supporting text for this operation and all its tool continuations.
    #[must_use]
    pub fn context(mut self, text: impl Into<String>) -> Self {
        self.options.context.push(text.into());
        self
    }
    /// Select tools by registered name. Empty disables tools.
    #[must_use]
    pub fn tools(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.tools = Some(names.into_iter().map(Into::into).collect());
        self
    }
    /// Enable or disable supported template thinking.
    #[must_use]
    pub fn thinking(mut self, thinking: crate::engine::Thinking) -> Self {
        self.options.thinking = Some(thinking);
        self
    }
    /// Override all operation options together.
    #[must_use]
    pub fn options(mut self, options: RequestOptions) -> Self {
        self.options = options;
        self
    }
    /// Request cancellation through a caller-owned handle.
    #[must_use]
    pub fn cancellation(mut self, cancellation: Cancellation) -> Self {
        self.options.cancellation = cancellation;
        self
    }
    /// Observe semantic events. Errors stop delivery and cancel ongoing work.
    #[must_use]
    pub fn on_event<F>(mut self, handler: F) -> Self
    where
        F: Fn(&RunEvent) -> ChatResult<()> + Send + Sync + 'static,
    {
        self.events.handler = Some(Arc::new(handler));
        self
    }
    /// Execute exactly one user interaction through the configured pipeline.
    /// # Errors
    /// Carries partial progress on validation, delivery, native, or tool failure.
    pub async fn run(self) -> Result<ChatResponse, RunFailure> {
        // Keep finalization owned by the runtime when a transport drops its
        // waiting future. Cancelling the waiter must not discard tool receipts.
        let mut waiting = CancelOnDrop {
            cancellation: self.options.cancellation.clone(),
            completed: false,
        };
        let control = self.chat.0.control.clone();
        let run_id = control.next_id();
        let handle = tokio::runtime::Handle::try_current().map_err(|_| RunFailure {
            source: ChatError::Configuration("generation requires a Tokio runtime".into()),
            partial: Box::new(empty_response(run_id)),
        })?;
        let result = handle.spawn(self.execute(run_id)).await;
        waiting.completed = true;
        match result {
            Ok(result) => result,
            Err(_) => Err(RunFailure {
                source: ChatError::Configuration("run task panicked or executor stopped".into()),
                partial: Box::new(empty_response(run_id)),
            }),
        }
    }
    async fn execute(self, run_id: u64) -> Result<ChatResponse, RunFailure> {
        let started = Instant::now();
        let inner = &self.chat.0;
        let maximum_deadline = started
            .checked_add(inner.limits.duration)
            .unwrap_or(started);
        let deadline = self
            .options
            .deadline
            .unwrap_or(maximum_deadline)
            .min(maximum_deadline);
        let empty = || empty_response(run_id);
        let mut guard = inner
            .control
            .admit(self.options.cancellation.clone())
            .await
            .map_err(|source| RunFailure {
                source,
                partial: Box::new(empty()),
            })?;
        let cancellation = self.options.cancellation.clone();
        let mut state = tokio::select! {
            () = inner.control.shutdown.cancelled() => return early_failure(guard, ChatError::Closed, empty()),
            () = cancellation.cancelled() => return early_failure(guard, ChatError::Cancelled, empty()),
            () = tokio::time::sleep_until(deadline.into()) => return early_failure(guard, ChatError::Deadline, empty()),
            state = inner.state.lock() => state,
        };
        let mut working = state.messages.clone();
        runner::trim_history(
            &mut working,
            &state.system,
            &self.input,
            &self.options.context,
            &inner.limits,
        );
        let mut context = RunContext::new(
            run_id,
            inner.id,
            inner.lease.clone(),
            state.system.clone(),
            working,
            self.input,
            self.options,
            self.events,
            inner.limits.clone(),
            inner.backend.clone(),
            inner.pipeline.clone(),
            started,
            deadline,
        );
        let operation = async {
            context.validate_input()?;
            context.emit(RunEvent::Started { run_id }).await?;
            NextRun {
                layers: &inner.pipeline.run_layers,
                pipeline: &inner.pipeline,
            }
            .run(&mut context)
            .await
        };
        let result = tokio::select! {
            () = inner.control.shutdown.cancelled() => Err(ChatError::Closed),
            () = cancellation.cancelled() => Err(ChatError::Cancelled),
            () = tokio::time::sleep_until(deadline.into()) => Err(ChatError::Deadline),
            result = operation => result,
        };
        let status = match &result {
            Ok(response) => response.status,
            Err(ChatError::Cancelled) => RunStatus::Cancelled,
            Err(ChatError::Deadline) => RunStatus::Deadline,
            Err(_) => RunStatus::Failed,
        };
        context.close_pending_tools();
        let mut response = match &result {
            Ok(response) => response.clone(),
            Err(_) => context.response(status).await,
        };
        // Close every outstanding protocol call with an explicit unknown result.
        // No automatic retry can assume that an interrupted side effect rolled back.
        if result.is_ok() {
            context
                .messages
                .push(Message::Assistant(response.message.clone()));
        }
        response.tools = context.receipts().to_vec();
        response.messages = context.run_messages().to_vec();
        response.run_id = run_id;
        response.status = status;
        if context.accepted {
            state.messages.clone_from(&context.messages);
        }
        drop(state);
        inner.pipeline.finished(&mut response).await;
        if let Err(error) = context.emit(RunEvent::Finished { run_id, status }).await {
            response
                .warnings
                .push(format!("terminal event delivery: {error}"));
        }
        response.elapsed = started.elapsed();
        guard.complete();
        match result {
            Ok(_) => Ok(response),
            Err(source) => Err(RunFailure {
                source,
                partial: Box::new(response),
            }),
        }
    }
}
struct CancelOnDrop {
    cancellation: Cancellation,
    completed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.completed {
            self.cancellation.cancel();
        }
    }
}
fn empty_response(run_id: u64) -> ChatResponse {
    ChatResponse {
        run_id,
        message: AssistantMessage::default(),
        tools: Vec::new(),
        usage: Usage::default(),
        finish_reason: None,
        status: RunStatus::Failed,
        elapsed: std::time::Duration::ZERO,
        warnings: Vec::new(),
        messages: Vec::new(),
    }
}
fn early_failure(
    mut guard: RunGuard,
    source: ChatError,
    mut partial: ChatResponse,
) -> Result<ChatResponse, RunFailure> {
    partial.status = match source {
        ChatError::Cancelled => RunStatus::Cancelled,
        ChatError::Deadline => RunStatus::Deadline,
        _ => RunStatus::Failed,
    };
    guard.complete();
    Err(RunFailure {
        source,
        partial: Box::new(partial),
    })
}

#[cfg(test)]
mod tests;
