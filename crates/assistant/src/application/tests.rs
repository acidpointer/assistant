//! Deterministic application integration tests. Scripted evaluation runs through
//! the real chat runner; no model, GPU, network or inference timing is required.
use super::*;
use crate::{
    chat::{
        AssistantMessage, EventSink, ModelBackend, ModelFailure, ModelRequest, ModelTurn,
        TurnContext,
    },
    engine::{FinishReason, Thinking, Usage},
    pipeline::{BoxFuture, PluginRegistrar},
    runtime::RuntimeControl,
};
use std::sync::atomic::AtomicUsize;

#[derive(Debug)]
struct Controlled {
    calls: AtomicUsize,
    started: Semaphore,
    release: Semaphore,
}
impl Controlled {
    fn new(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            started: Semaphore::new(0),
            release: Semaphore::new(permits),
        })
    }
}
impl ModelBackend for Controlled {
    fn default_max_tokens(&self) -> usize {
        8
    }
    fn default_thinking(&self) -> Thinking {
        Thinking::Disabled
    }
    fn evaluate(
        &self,
        _: ModelRequest,
        turn: TurnContext,
        events: EventSink,
    ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            events
                .emit(RunEvent::TextDelta {
                    run_id: turn.run_id,
                    text: "Hello".into(),
                })
                .await
                .map_err(|source| ModelFailure {
                    metrics: None,
                    source,
                    partial: None,
                })?;
            self.started.add_permits(1);
            tokio::select! {
                () = turn.cancellation.cancelled() => return Err(ModelFailure {
                    metrics: None,
                    source: ChatError::Cancelled,
                    partial: None,
                }),
                permit = self.release.acquire() => { permit.expect("open gate").forget(); }
            }
            Ok(ModelTurn {
                limit_reason: None,
                metrics: None,
                message: AssistantMessage::text("Hello"),
                usage: Usage::default(),
                finish_reason: FinishReason::EndOfGeneration,
            })
        })
    }
}
fn application(backend: Arc<Controlled>) -> Application {
    application_with(backend, PluginRegistrar::default())
}
fn application_with(backend: Arc<dyn ModelBackend>, registrar: PluginRegistrar) -> Application {
    let chat = Chat::new(
        1,
        String::new(),
        backend,
        Arc::new(registrar.build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    Application::start(chat, RequestOptions::default()).expect("start coordinator")
}
fn input(key: &str) -> Submission {
    Submission {
        client_id: "client".into(),
        request_id: key.into(),
        text: "Hi".into(),
    }
}
async fn finished(handle: &AssistantHandle, id: &str) -> RunView {
    let mut subscription = handle.subscribe().await.expect("subscribe");
    if let Some(run) = subscription
        .snapshot
        .recent_runs
        .iter()
        .find(|r| r.id == id && r.status.is_terminal())
    {
        return run.clone();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = subscription.events.recv().await.expect("event");
            if let Update::Run { run } = &event.update
                && run.id == id
                && run.status.is_terminal()
            {
                return run.as_ref().clone();
            }
        }
    })
    .await
    .expect("terminal event")
}

#[tokio::test]
async fn shared_admission_deduplication_and_nonblocking_snapshots() {
    let backend = Controlled::new(0);
    let mut app = application(backend.clone());
    let handle = &app.handle;
    let mut oversized = input("oversized");
    oversized.text = "x".repeat(INPUT_BYTES + 1);
    assert_eq!(
        handle
            .submit("web", oversized)
            .await
            .expect_err("bounded ingress"),
        ApplicationError::InvalidInput
    );
    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    let run = handle.submit("web", input("one")).await.expect("admit");
    backend.started.acquire().await.expect("started").forget();
    assert_eq!(
        handle
            .submit("web", input("one"))
            .await
            .expect("duplicate")
            .id,
        run.id
    );
    assert_eq!(
        handle
            .submit("other_transport", input("one"))
            .await
            .expect_err("shared admission"),
        ApplicationError::Busy
    );
    let mut conflict = input("one");
    conflict.text = "different".into();
    assert_eq!(
        handle
            .submit("web", conflict)
            .await
            .expect_err("key conflict"),
        ApplicationError::Conflict
    );
    let snapshot = tokio::time::timeout(Duration::from_secs(1), handle.snapshot())
        .await
        .expect("does not wait for generation")
        .expect("snapshot");
    assert!(snapshot.active_run.is_some());
    backend.release.add_permits(1);
    assert_eq!(finished(handle, &run.id).await.status, Status::Completed);
    assert_eq!(backend.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        handle
            .submit("web", input("one"))
            .await
            .expect("completed duplicate")
            .status,
        Status::Completed
    );
    app.shutdown().await.expect("shutdown");
    assert_eq!(
        app.handle.snapshot().await.expect_err("closed handle"),
        ApplicationError::Closed
    );
}

#[tokio::test]
async fn cancellation_is_idempotent_and_preserves_partial_output() {
    let backend = Controlled::new(0);
    let mut app = application(backend.clone());
    let run = app
        .handle
        .submit("web", input("stop"))
        .await
        .expect("admit");
    backend.started.acquire().await.expect("started").forget();
    app.handle.cancel(&run.id).await.expect("cancel");
    assert_eq!(
        finished(&app.handle, &run.id).await.status,
        Status::Cancelled
    );
    assert_eq!(
        app.handle
            .cancel(&run.id)
            .await
            .expect("repeat cancel")
            .status,
        Status::Cancelled
    );
    let snapshot = app.handle.snapshot().await.expect("snapshot");
    assert!(
        snapshot
            .messages
            .iter()
            .any(|m| m.role == "assistant" && m.text == "Hello" && m.status == Status::Cancelled)
    );
    app.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn dropped_observers_do_not_cancel_and_subscriptions_have_an_atomic_boundary() {
    let backend = Controlled::new(0);
    let mut app = application(backend.clone());
    let mut observer = app.handle.subscribe().await.expect("subscribe");
    assert!(observer.snapshot.messages.is_empty());
    let run = app
        .handle
        .submit("web", input("observe"))
        .await
        .expect("admit");
    let event = observer.events.recv().await.expect("accepted update");
    assert_ne!(event.id, observer.snapshot.revision);
    drop(observer);
    backend.started.acquire().await.expect("started").forget();
    assert!(
        !app.handle
            .run(&run.id)
            .await
            .expect("run")
            .status
            .is_terminal()
    );
    backend.release.add_permits(1);
    assert_eq!(
        finished(&app.handle, &run.id).await.status,
        Status::Completed
    );
    app.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn lagged_observers_recover_and_history_remains_bounded() {
    let backend = Controlled::new(REQUEST_LIMIT + 1);
    let mut app = application(backend);
    let mut observer = app.handle.subscribe().await.expect("subscribe");
    for index in 0..REQUEST_LIMIT {
        let run = app
            .handle
            .submit("test", input(&format!("r{index}")))
            .await
            .expect("admit");
        finished(&app.handle, &run.id).await;
    }
    assert!(matches!(
        observer.events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    let recovered = app.handle.subscribe().await.expect("resnapshot");
    assert!(recovered.snapshot.truncated);
    assert_eq!(
        app.handle
            .submit("test", input("overflow"))
            .await
            .expect_err("retention capacity"),
        ApplicationError::Capacity
    );
    assert_eq!(
        app.handle
            .submit("test", input("r0"))
            .await
            .expect("deduplication survives display eviction")
            .status,
        Status::Completed
    );
    assert!(recovered.snapshot.messages.len() <= MESSAGE_LIMIT);
    assert!(
        recovered
            .snapshot
            .messages
            .iter()
            .map(|m| m.text.len())
            .sum::<usize>()
            <= TRANSCRIPT_BYTES
    );
    app.shutdown().await.expect("shutdown");
}

#[derive(Debug)]
struct Continuation {
    calls: AtomicUsize,
    partial_started: Semaphore,
}
impl ModelBackend for Continuation {
    fn default_max_tokens(&self) -> usize {
        32
    }
    fn default_thinking(&self) -> Thinking {
        Thinking::Disabled
    }
    fn evaluate(
        &self,
        _: ModelRequest,
        turn: TurnContext,
        events: EventSink,
    ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                let mut message = AssistantMessage::text("A longer explanation before the tool.");
                message.tool_calls.push(crate::chat::ToolCall {
                    id: "call".into(),
                    name: "calculate".into(),
                    arguments: serde_json::json!({"operation":"add","a":1,"b":2}),
                });
                return Ok(ModelTurn {
                    limit_reason: None,
                    metrics: None,
                    message,
                    usage: Usage::default(),
                    finish_reason: FinishReason::EndOfGeneration,
                });
            }
            events
                .emit(RunEvent::TextDelta {
                    run_id: turn.run_id,
                    text: "A".into(),
                })
                .await
                .map_err(|source| ModelFailure {
                    metrics: None,
                    source,
                    partial: None,
                })?;
            self.partial_started.add_permits(1);
            turn.cancellation.cancelled().await;
            Err(ModelFailure {
                metrics: None,
                source: ChatError::Cancelled,
                partial: None,
            })
        })
    }
}

#[tokio::test]
async fn interrupted_tool_continuation_keeps_its_own_partial_message() {
    let backend = Arc::new(Continuation {
        calls: AtomicUsize::new(0),
        partial_started: Semaphore::new(0),
    });
    let mut registrar = PluginRegistrar::default();
    registrar
        .add_tool(crate::tools::builtin::Calculator)
        .expect("calculator");
    let mut app = application_with(backend.clone(), registrar);
    let run = app
        .handle
        .submit("test", input("continuation"))
        .await
        .expect("admit");
    backend
        .partial_started
        .acquire()
        .await
        .expect("partial turn started")
        .forget();
    app.handle.cancel(&run.id).await.expect("cancel");
    finished(&app.handle, &run.id).await;
    let snapshot = app.handle.snapshot().await.expect("snapshot");
    let messages = snapshot
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "A longer explanation before the tool.");
    assert_eq!(messages[1].text, "A");
    assert_ne!(messages[0].id, messages[1].id);
    app.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn shutdown_finalizes_an_active_run_and_instances_are_distinct() {
    let backend = Controlled::new(0);
    let mut app = application(backend.clone());
    let mut observer = app.handle.subscribe().await.expect("subscribe");
    let run = app
        .handle
        .submit("test", input("shutdown"))
        .await
        .expect("admit");
    backend.started.acquire().await.expect("started").forget();
    app.shutdown().await.expect("shutdown");
    let mut terminal_seen = false;
    while let Ok(event) = observer.events.try_recv() {
        if let Update::Run { run: update } = &event.update
            && update.id == run.id
            && update.status == Status::Cancelled
        {
            terminal_seen = true;
        }
    }
    assert!(terminal_seen);
    let mut other = application(Controlled::new(0));
    assert_ne!(
        observer.snapshot.instance_id,
        other.handle.snapshot().await.expect("other").instance_id
    );
    other.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn visible_exchange_keeps_its_measurements_after_minimum_retention() {
    let chat = Chat::new(
        1,
        String::new(),
        Controlled::new(0),
        Arc::new(PluginRegistrar::default().build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut actor = Coordinator::new(
        chat,
        RequestOptions::default(),
        "instance".into(),
        Arc::new(AtomicBool::new(false)),
        Cancellation::default(),
    );
    let (old, _) = actor
        .admit("test".into(), input("old"))
        .await
        .expect("admit old");
    actor.active = None;
    let record = actor.records.front_mut().expect("retained record");
    record.view.status = Status::Completed;
    record.created = Instant::now()
        .checked_sub(REQUEST_RETENTION + Duration::from_secs(1))
        .expect("test process clock supports retention interval");
    actor
        .admit("test".into(), input("second"))
        .await
        .expect("admit new");
    assert!(
        actor.record(&old.id).is_ok(),
        "visible message keeps its run measurements"
    );
    actor.active = None;
    actor.messages.retain(|message| message.run_id != old.id);
    actor
        .admit("test".into(), input("third"))
        .await
        .expect("admit after eviction");
    assert!(matches!(
        actor.record(&old.id),
        Err(ApplicationError::NotFound)
    ));
}

#[derive(Debug)]
struct SilentFailure;
impl ModelBackend for SilentFailure {
    fn default_max_tokens(&self) -> usize {
        8
    }
    fn default_thinking(&self) -> Thinking {
        Thinking::Disabled
    }
    fn evaluate(
        &self,
        _: ModelRequest,
        _: TurnContext,
        _: EventSink,
    ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>> {
        Box::pin(async {
            Err(ModelFailure {
                source: ChatError::Protocol("private diagnostic".into()),
                partial: None,
                metrics: None,
            })
        })
    }
}
#[tokio::test]
async fn failure_before_visible_text_has_a_terminal_display_entry_without_fabricated_output() {
    let mut app = application_with(Arc::new(SilentFailure), PluginRegistrar::default());
    let run = app
        .handle
        .submit("test", input("silent-failure"))
        .await
        .expect("admit");
    let outcome = finished(&app.handle, &run.id).await;
    assert_eq!(outcome.status, Status::Failed);
    assert_eq!(outcome.error, Some("generation_failed"));
    let snapshot = app.handle.snapshot().await.expect("snapshot");
    let answer = snapshot
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("terminal row");
    assert!(
        answer.text.is_empty(),
        "host failures are not model content"
    );
    assert_eq!(answer.status, Status::Failed);
    app.shutdown().await.expect("shutdown");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn required_memory_acceptance_terminal_commit_and_restart_retry_are_durable() {
    use crate::memory::{
        ClientRequestKey, MemoryConfig, MemoryId, MemoryStore, OperationState,
        service::MemoryAccess,
    };
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::env::temp_dir().join(format!(
        "assistant-product-test-{}",
        MemoryId::new().expect("identity")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private directory");
    let config = MemoryConfig::new(&directory);
    let store = MemoryStore::open(config.clone()).await.expect("store");
    let memory = MemoryAccess::for_test(&store);
    let backend = Controlled::new(1);
    let chat = Chat::new(
        1,
        String::new(),
        backend.clone(),
        Arc::new(PluginRegistrar::default().build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut app =
        Application::start_with_memory(chat, RequestOptions::default(), Some(memory.clone()))
            .expect("app");
    let accepted = app
        .handle
        .submit("test", input("durable"))
        .await
        .expect("accepted after commit");
    let key = ClientRequestKey::new("test", "client", "durable").expect("key");
    let record = memory
        .store
        .operation_by_key(&memory.owner, key.clone())
        .await
        .expect("durable before acceptance");
    assert_eq!(accepted.id, record.id.to_string());
    backend
        .started
        .acquire()
        .await
        .expect("initial model call")
        .forget();
    let terminal = finished(&app.handle, &accepted.id).await;
    assert_eq!(terminal.status, Status::Completed);
    let record = memory
        .store
        .operation_by_key(&memory.owner, key.clone())
        .await
        .expect("committed before terminal event");
    assert_eq!(record.state, OperationState::Completed);
    let assistant = memory
        .source(record.assistant.expect("visible assistant source").id)
        .await
        .expect("assistant evidence");
    assert_eq!(assistant.source.text.as_str(), "Hello");
    app.shutdown().await.expect("join app");
    store.shutdown().await.expect("join store");
    let store = MemoryStore::open(config).await.expect("restart");
    let memory = MemoryAccess::for_test(&store);
    let chat = Chat::new(
        2,
        String::new(),
        backend.clone(),
        Arc::new(PluginRegistrar::default().build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut app = Application::start_with_memory(chat, RequestOptions::default(), Some(memory))
        .expect("restart app");
    let owner = app.handle.owner_access("test").expect("owner grant");
    let recovered = owner
        .lookup("client", "durable")
        .await
        .expect("lookup without resubmission");
    assert_eq!(recovered.id, accepted.id);
    assert_eq!(recovered.status, Status::Completed);
    let history = owner.history(None, 1).await.expect("durable history");
    assert_eq!(history.exchanges.len(), 1);
    assert_eq!(history.exchanges[0].input.source.text.as_str(), "Hi");
    assert_eq!(
        history.exchanges[0]
            .assistant
            .as_ref()
            .expect("visible output")
            .source
            .text
            .as_str(),
        "Hello"
    );
    assert!(
        owner
            .snapshot()
            .await
            .expect("recovered snapshot")
            .messages
            .iter()
            .any(|m| m.text == "Hello")
    );
    let replay = app
        .handle
        .submit("test", input("durable"))
        .await
        .expect("retry across restart");
    assert_eq!(replay.id, accepted.id);
    assert_eq!(replay.status, Status::Completed);
    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        1,
        "a retry never generates again"
    );
    assert!(
        app.handle
            .snapshot()
            .await
            .expect("snapshot")
            .messages
            .iter()
            .any(|message| message.text == "Hello")
    );
    let another_grant = app.handle.owner_access("test").expect("independent grant");
    let clone = owner.clone();
    owner.revoke();
    assert_eq!(
        clone
            .snapshot()
            .await
            .expect_err("revocation shared by clones"),
        ApplicationError::Unauthorized
    );
    assert!(
        another_grant.lookup("client", "durable").await.is_ok(),
        "revocation does not erase operations or revoke other sessions"
    );
    let active = another_grant
        .submit(input("new-active"))
        .await
        .expect("new accepted input");
    backend
        .started
        .acquire()
        .await
        .expect("second model call")
        .forget();
    let replay = another_grant
        .submit(input("durable"))
        .await
        .expect("historical retry while busy");
    assert_eq!(replay.id, accepted.id);
    assert_eq!(replay.status, Status::Completed);
    assert_eq!(
        app.handle
            .snapshot()
            .await
            .expect("snapshot")
            .active_run
            .expect("new run stays active")
            .id,
        active.id
    );
    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        2,
        "only genuinely new input executes"
    );
    app.shutdown().await.expect("join app");
    store.shutdown().await.expect("join store");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn durable_history_keeps_visible_text_before_and_after_a_tool() {
    use crate::memory::{MemoryConfig, MemoryId, MemoryStore, service::MemoryAccess};
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::env::temp_dir().join(format!(
        "assistant-visible-turns-{}",
        MemoryId::new().expect("identity")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private directory");
    let store = MemoryStore::open(MemoryConfig::new(&directory))
        .await
        .expect("store");
    let memory = MemoryAccess::for_test(&store);
    let backend = Arc::new(Continuation {
        calls: AtomicUsize::new(0),
        partial_started: Semaphore::new(0),
    });
    let mut registrar = PluginRegistrar::default();
    registrar
        .add_tool(crate::tools::builtin::Calculator)
        .expect("calculator");
    let chat = Chat::new(
        1,
        String::new(),
        backend.clone(),
        Arc::new(registrar.build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut app =
        Application::start_with_memory(chat, RequestOptions::default(), Some(memory)).expect("app");
    let owner = app.handle.owner_access("test").expect("owner");
    let run = owner.submit(input("tool-history")).await.expect("admit");
    backend
        .partial_started
        .acquire()
        .await
        .expect("continuation started")
        .forget();
    owner.cancel(&run.id).await.expect("cancel");
    finished(&app.handle, &run.id).await;
    let history = owner
        .history(None, 1)
        .await
        .expect("read committed history");
    assert_eq!(
        history.exchanges[0]
            .assistant
            .as_ref()
            .expect("visible source")
            .source
            .text
            .as_str(),
        "A longer explanation before the tool.\n\nA"
    );
    app.shutdown().await.expect("join application");
    store.shutdown().await.expect("join storage");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pending_storage_admission_keeps_snapshots_responsive_and_survives_lost_waiter() {
    use crate::memory::{MemoryConfig, MemoryId, MemoryStore, service::MemoryAccess};
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::env::temp_dir().join(format!(
        "assistant-pending-{}",
        MemoryId::new().expect("identity")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private directory");
    let store = MemoryStore::open(MemoryConfig::new(&directory))
        .await
        .expect("store");
    let memory = MemoryAccess::for_test(&store);
    let backend = Controlled::new(0);
    let chat = Chat::new(
        1,
        String::new(),
        backend.clone(),
        Arc::new(PluginRegistrar::default().build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut app =
        Application::start_with_memory(chat, RequestOptions::default(), Some(memory.clone()))
            .expect("application");
    let mut subscription = app.handle.subscribe().await.expect("startup snapshot");
    let (started, ready) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let writer = memory.store.clone();
    let blocker = tokio::spawn(async move { writer.test_writer_barrier(started, gate).await });
    ready.await.expect("writer held");
    let handle = app.handle.clone();
    let waiter = tokio::spawn(async move { handle.submit("test", input("lost-waiter")).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while memory.store.test_writer_pending() <= 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admission queued");
    let snapshot = tokio::time::timeout(Duration::from_millis(200), app.handle.snapshot())
        .await
        .expect("actor is responsive during SQLite wait")
        .expect("snapshot");
    assert!(
        snapshot.active_run.is_none(),
        "uncommitted input is not accepted"
    );
    waiter.abort();
    release.send(()).expect("release writer");
    blocker
        .await
        .expect("join blocker")
        .expect("barrier completed");
    let accepted = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = subscription.events.recv().await.expect("event");
            if let Update::Run { run } = &event.update {
                break run.clone();
            }
        }
    })
    .await
    .expect("application owns admission despite lost waiter");
    let owner = app.handle.owner_access("test").expect("grant");
    assert_eq!(
        owner
            .lookup("client", "lost-waiter")
            .await
            .expect("reconcile key")
            .id,
        accepted.id
    );
    backend
        .started
        .acquire()
        .await
        .expect("model started once")
        .forget();
    assert_eq!(backend.calls.load(Ordering::Relaxed), 1);
    app.shutdown().await.expect("settle and join");
    store.shutdown().await.expect("store shutdown");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct UncertainTool {
    started: Arc<Semaphore>,
    effects: Arc<AtomicUsize>,
}
#[cfg(target_os = "linux")]
impl crate::tools::Tool for UncertainTool {
    const NAME: &'static str = "uncertain_effect";
    type Args = serde_json::Value;
    type Output = serde_json::Value;
    type Error = std::io::Error;
    fn description(&self) -> &str {
        "Effect test"
    }
    async fn call(
        &self,
        _: crate::tools::ToolContext,
        _: Self::Args,
    ) -> std::io::Result<Self::Output> {
        self.effects.fetch_add(1, Ordering::Relaxed);
        self.started.add_permits(1);
        std::future::pending().await
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropped_tool_waiter_settles_unknown_before_terminal_commit() {
    use crate::memory::{
        MemoryConfig, MemoryId, MemoryStore, OperationState, ToolOutcome, service::MemoryAccess,
        session::MemoryOperation,
    };
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::env::temp_dir().join(format!(
        "assistant-tool-settlement-{}",
        MemoryId::new().expect("identity")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private directory");
    let store = MemoryStore::open(MemoryConfig::new(&directory))
        .await
        .expect("store");
    let memory = MemoryAccess::for_test(&store);
    let (record, _) = memory
        .admit_product(
            "test",
            "client",
            "tool",
            "Perform the authorized test effect.",
        )
        .await
        .expect("admit");
    let operation = MemoryOperation::new(record, memory.clone());
    let recalled = operation
        .prepare("Perform the authorized test effect.")
        .await
        .expect("begin");
    assert!(
        !recalled.contains("Perform the authorized test effect"),
        "a question is not its own historical evidence"
    );
    let started = Arc::new(Semaphore::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut tools = crate::tools::ToolRegistry::new();
    tools
        .register(UncertainTool {
            started: started.clone(),
            effects: effects.clone(),
        })
        .expect("register");
    let cancellation = Cancellation::default();
    let tool_cancellation = cancellation.clone();
    let task_operation = operation.clone();
    let waiter = tokio::spawn(async move {
        task_operation
            .execute_tool(
                tools,
                crate::chat::ToolCall {
                    id: "call1".into(),
                    name: "uncertain_effect".into(),
                    arguments: serde_json::json!({}),
                },
                vec!["uncertain_effect".into()],
                crate::tools::ToolContext {
                    #[cfg(target_os = "linux")]
                    memory: None,
                    invocation_id: "call1".into(),
                    cancellation: tool_cancellation,
                },
                Instant::now() + Duration::from_secs(5),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.acquire())
        .await
        .expect("intent committed and handler started")
        .expect("gate")
        .forget();
    waiter.abort();
    cancellation.cancel();
    let response = ChatResponse {
        performance: vec![],
        limit_reason: None,
        run_id: 1,
        message: AssistantMessage::default(),
        tools: vec![],
        usage: Usage::default(),
        finish_reason: None,
        status: RunStatus::Cancelled,
        elapsed: Duration::ZERO,
        warnings: vec![],
        messages: vec![],
    };
    tokio::time::timeout(Duration::from_secs(2), operation.settle(&Ok(response)))
        .await
        .expect("settlement drains")
        .expect("terminal commit");
    let durable = memory
        .store
        .operation(&memory.owner, operation.record.id)
        .await
        .expect("operation");
    assert_eq!(durable.state, OperationState::Interrupted);
    assert!(durable.unknown_tool_outcome);
    let invocation = memory
        .store
        .tool_invocation(&memory.owner, operation.record.id, 1)
        .await
        .expect("receipt");
    assert_eq!(invocation.outcome, Some(ToolOutcome::Unknown));
    assert!(
        invocation.result.is_none(),
        "ephemeral raw results were never persisted"
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1, "never replay effects");
    store.shutdown().await.expect("join store");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn memory_tools_and_personality_use_the_shared_product_loop() {
    use crate::{
        chat::{Message, ToolCall},
        memory::{
            MemoryConfig, MemoryId, MemoryStore, MemoryText, OperationKey, service::MemoryAccess,
        },
    };
    use std::os::unix::fs::DirBuilderExt;
    #[derive(Debug)]
    struct Journey {
        step: AtomicUsize,
        source: MemoryId,
    }
    impl ModelBackend for Journey {
        fn default_max_tokens(&self) -> usize {
            128
        }
        fn default_thinking(&self) -> Thinking {
            Thinking::Disabled
        }
        fn evaluate(
            &self,
            request: ModelRequest,
            _: TurnContext,
            _: EventSink,
        ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>> {
            Box::pin(async move {
                let step = self.step.fetch_add(1, Ordering::SeqCst);
                assert!(
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "memory.search")
                );
                let call = match step {
                    0 => Some((
                        "assistant.configure",
                        serde_json::json!({"instructions":"Be a warm anime companion. Speak plainly about Rust."}),
                    )),
                    1 => Some((
                        "memory.search",
                        serde_json::json!({"query":"garden bench project"}),
                    )),
                    2 => Some((
                        "memory.follow",
                        serde_json::json!({"source_id":self.source.to_string()}),
                    )),
                    3 => Some((
                        "memory.remember",
                        serde_json::json!({"note":"The owner wants a warm companion voice and enjoys practical garden projects.","related_source_ids":[self.source.to_string()]}),
                    )),
                    _ => None,
                };
                if step > 0 && step < 5 {
                    assert!(request.messages.iter().any(|message|matches!(message,Message::Tool{result,..} if result.get("ok").and_then(serde_json::Value::as_bool)==Some(true))),"tool receipts reach continuation");
                }
                if step == 5 {
                    assert!(
                        request.system.contains("warm anime companion"),
                        "updated personality is pinned on the next user run"
                    );
                }
                let message = call.map_or_else(
                    || AssistantMessage::text("I'll keep that in mind."),
                    |(name, arguments)| AssistantMessage {
                        tool_calls: vec![ToolCall {
                            id: format!("journey-{step}"),
                            name: name.into(),
                            arguments,
                        }],
                        ..AssistantMessage::default()
                    },
                );
                Ok(ModelTurn {
                    message,
                    usage: Usage {
                        generated_tokens: 1,
                        ..Usage::default()
                    },
                    finish_reason: FinishReason::EndOfGeneration,
                    metrics: None,
                    limit_reason: None,
                })
            })
        }
    }
    let directory = std::env::temp_dir().join(format!(
        "assistant-tool-journey-{}",
        MemoryId::new().expect("id")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private dir");
    let store = MemoryStore::open(MemoryConfig::new(&directory))
        .await
        .expect("store");
    let memory = MemoryAccess::for_test(&store);
    let source = memory
        .remember(
            OperationKey::new("test", "project").expect("key"),
            MemoryText::new("I'm making a garden bench from cedar.").expect("experience"),
        )
        .await
        .expect("saved project")
        .value;
    let backend = Arc::new(Journey {
        step: AtomicUsize::new(0),
        source: source.id,
    });
    let mut registrar = PluginRegistrar::default();
    crate::memory::tools::register(&mut registrar).expect("typed tools");
    let chat = Chat::new(
        1,
        String::new(),
        backend.clone(),
        Arc::new(registrar.build().expect("pipeline")),
        crate::chat::RunLimits::default(),
        RuntimeControl::new(4),
    );
    let mut app =
        Application::start_with_memory(chat, RequestOptions::default(), Some(memory.clone()))
            .expect("app");
    app.ready().await.expect("hydrate");
    let owner = app
        .handle
        .owner_access("socket-adapter")
        .expect("host-issued transport-independent owner");
    let note = owner
        .remember(
            "explicit-note",
            MemoryText::new("For my garden bench I want a satin finish.").expect("note"),
        )
        .await
        .expect("save without chat inference");
    assert_eq!(
        owner
            .remember("explicit-note", note.value.text.clone())
            .await
            .expect("retry same note")
            .value
            .id,
        note.value.id
    );
    assert!(
        owner
            .history(None, 4)
            .await
            .expect("independent note")
            .exchanges
            .is_empty()
    );
    assert_eq!(
        owner
            .evidence(note.value.id)
            .await
            .expect("owner attribution")
            .role,
        crate::memory::SourceRole::User
    );
    assert!(
        owner
            .evidence(note.value.id)
            .await
            .expect("live note")
            .observed_at
            .is_some(),
        "live capture has a host observation date; historical imports remain unknown"
    );
    let run=owner.submit(Submission{client_id:"client".into(),request_id:"persona-project".into(),text:"Please use a warm anime companion voice, remember our garden project, and connect it with this conversation.".into()}).await.expect("durable admission");
    assert_eq!(
        finished(&app.handle, &run.id).await.status,
        Status::Completed
    );
    assert_eq!(backend.step.load(Ordering::SeqCst), 5);
    let graph = owner
        .follow_sources(source.id, 2)
        .await
        .expect("same graph for any transport");
    assert!(
        graph
            .sources
            .iter()
            .any(|record| record.role == crate::memory::SourceRole::Assistant
                && record.source.text.as_str().contains("garden projects"))
    );
    let next = owner
        .submit(Submission {
            client_id: "client".into(),
            request_id: "next-topic".into(),
            text: "Let's talk about Rust now.".into(),
        })
        .await
        .expect("next topic");
    assert_eq!(
        finished(&app.handle, &next.id).await.status,
        Status::Completed
    );
    owner.revoke();
    assert!(owner.assistant_profile().await.is_err());
    app.shutdown().await.expect("join application");
    store.shutdown().await.expect("close storage");
    std::fs::remove_dir_all(directory).expect("cleanup");
}
