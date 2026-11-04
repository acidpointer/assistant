//! Deterministic orchestration tests without model weights or inference timing.
use super::*;
use crate::{
    engine::{FinishReason, Thinking},
    pipeline::{ModelLayer, NextModel, PluginRegistrar},
    tools::builtin::Calculator,
};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

#[derive(Debug, Default)]
struct Scripted {
    turns: Mutex<VecDeque<ModelTurn>>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Scripted {
    fn new(messages: Vec<AssistantMessage>) -> Arc<Self> {
        Arc::new(Self {
            turns: Mutex::new(
                messages
                    .into_iter()
                    .map(|message| ModelTurn {
                        limit_reason: None,
                        metrics: None,
                        message,
                        usage: Usage {
                            input_tokens: 10,
                            prefill_tokens: 10,
                            generated_tokens: 1,
                            ..Usage::default()
                        },
                        finish_reason: FinishReason::EndOfGeneration,
                    })
                    .collect(),
            ),
            requests: Mutex::default(),
        })
    }
}
impl ModelBackend for Scripted {
    fn default_max_tokens(&self) -> usize {
        8
    }
    fn default_thinking(&self) -> Thinking {
        Thinking::Disabled
    }
    fn evaluate(
        &self,
        request: ModelRequest,
        _turn: TurnContext,
        _events: EventSink,
    ) -> BoxFuture<'_, Result<ModelTurn, ModelFailure>> {
        Box::pin(async move {
            self.requests.lock().await.push(request);
            self.turns.lock().await.pop_front().ok_or(ModelFailure {
                metrics: None,
                source: ChatError::Closed,
                partial: None,
            })
        })
    }
}
fn chat(backend: Arc<Scripted>, registrar: PluginRegistrar, limits: RunLimits) -> Chat {
    Chat::new(
        1,
        "system".into(),
        backend,
        Arc::new(registrar.build().expect("valid pipeline")),
        limits,
        RuntimeControl::new(4),
    )
}
fn calculator_call() -> AssistantMessage {
    AssistantMessage {
        tool_calls: vec![ToolCall {
            id: "call-1".into(),
            name: "calculate".into(),
            arguments: json!({"operation":"multiply","a":19,"b":23}),
        }],
        ..AssistantMessage::default()
    }
}

#[tokio::test]
async fn tool_loop_keeps_receipts_and_context_for_continuations_only() {
    let backend = Scripted::new(vec![
        calculator_call(),
        AssistantMessage::text("437"),
        AssistantMessage::text("next"),
    ]);
    let mut registrar = PluginRegistrar::default();
    registrar.add_tool(Calculator).expect("register calculator");
    let chat = chat(backend.clone(), registrar, RunLimits::default());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = events.clone();
    let response = chat
        .request("19 times 23")
        .context("retrieved knowledge")
        .on_event(move |event| {
            recorded.lock().expect("event log").push(event.clone());
            Ok(())
        })
        .run()
        .await
        .expect("tool conversation");
    assert_eq!(response.text(), "437");
    assert_eq!(response.tools[0].result["result"]["value"], 437);
    assert_eq!(response.usage.generated_tokens, 2);
    assert_eq!(response.messages.len(), 4);
    assert_eq!(chat.messages().await.len(), 4);
    chat.generate("next question")
        .await
        .expect("second interaction");
    let requests = backend.requests.lock().await;
    assert_eq!(requests[0].context, ["retrieved knowledge"]);
    assert_eq!(requests[1].context, requests[0].context);
    assert!(requests[2].context.is_empty());
    let events = events.lock().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, RunEvent::Finished { .. }))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunEvent::TextDelta { text, .. } if text == "437"))
    );
}

#[tokio::test]
async fn complete_old_exchanges_are_evicted_and_bad_input_preserves_history() {
    let backend = Scripted::new(vec![
        AssistantMessage::text("a"),
        AssistantMessage::text("b"),
        AssistantMessage::text("c"),
    ]);
    let limits = RunLimits {
        working_messages: 4,
        input_bytes: 1024,
        ..RunLimits::default()
    };
    let chat = chat(backend.clone(), PluginRegistrar::default(), limits);
    for question in ["first", "second", "third"] {
        chat.generate(question).await.expect("short response");
    }
    let messages = chat.messages().await;
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0], Message::user("second"));
    assert_eq!(backend.requests.lock().await[2].messages.len(), 3);
    assert!(chat.generate("x".repeat(2048)).await.is_err());
    assert_eq!(
        chat.messages().await,
        messages,
        "rejected requests leave prior working state intact"
    );
}

#[derive(Debug)]
struct Oversize;
impl ModelLayer for Oversize {
    fn call<'a>(
        &'a self,
        context: &'a mut RunContext,
        mut request: ModelRequest,
        next: NextModel<'a>,
    ) -> BoxFuture<'a, ChatResult<ModelTurn>> {
        request.options.max_new_tokens = Some(usize::MAX);
        next.run(context, request)
    }
}
#[tokio::test]
async fn model_boundary_checks_limits_after_middleware() {
    let backend = Scripted::new(vec![AssistantMessage::text("unused")]);
    let mut registrar = PluginRegistrar::default();
    registrar.layer_model(Oversize);
    let chat = chat(backend.clone(), registrar, RunLimits::default());
    chat.generate("hello")
        .await
        .expect("oversized preference is clamped");
    let requests = backend.requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].options.max_new_tokens,
        Some(RunLimits::default().generated_tokens)
    );
}

#[tokio::test]
async fn cancellation_drops_pending_hooks_and_finishes_once() {
    let backend = Scripted::new(Vec::new());
    let mut registrar = PluginRegistrar::default();
    let entered = Arc::new(tokio::sync::Notify::new());
    let hook_entered = entered.clone();
    registrar.before_run("wait", move |_| {
        let entered = hook_entered.clone();
        Box::pin(async move {
            entered.notify_one();
            std::future::pending().await
        })
    });
    let finished = Arc::new(AtomicUsize::new(0));
    let hook_finished = finished.clone();
    registrar.on_run_finished("finish", move |response, _| {
        let finished = hook_finished.clone();
        Box::pin(async move {
            assert_eq!(response.status, RunStatus::Cancelled);
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let chat = chat(backend.clone(), registrar, RunLimits::default());
    let cancellation = Cancellation::default();
    let token = cancellation.clone();
    let task = tokio::spawn(async move { chat.request("hello").cancellation(token).run().await });
    entered.notified().await;
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("cancel promptly")
        .expect("join request")
        .expect_err("cancelled");
    assert_eq!(error.partial.status, RunStatus::Cancelled);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(backend.requests.lock().await.is_empty());
}

#[tokio::test]
async fn delivery_failure_preserves_real_tool_receipt_and_closes_protocol() {
    let backend = Scripted::new(vec![calculator_call(), AssistantMessage::text("unused")]);
    let mut registrar = PluginRegistrar::default();
    registrar.add_tool(Calculator).expect("tool");
    let chat = chat(backend.clone(), registrar, RunLimits::default());
    let error = chat
        .request("multiply")
        .on_event(|event| {
            if matches!(event, RunEvent::ToolFinished { .. }) {
                Err(ChatError::Configuration("consumer disconnected".into()))
            } else {
                Ok(())
            }
        })
        .run()
        .await
        .expect_err("delivery failed after effect");
    assert_eq!(error.partial.tools.len(), 1);
    assert_eq!(error.partial.tools[0].result["result"]["value"], 437);
    assert!(matches!(
        chat.messages().await.last(),
        Some(Message::Tool { .. })
    ));
    assert_eq!(backend.requests.lock().await.len(), 1, "no implicit replay");
}

#[tokio::test]
async fn interrupted_tool_middleware_records_unknown_outcome() {
    #[derive(Debug)]
    struct Wait(Arc<tokio::sync::Notify>);
    impl crate::pipeline::ToolLayer for Wait {
        fn call<'a>(
            &'a self,
            _context: &'a mut RunContext,
            _call: ToolCall,
            _next: crate::pipeline::NextTool<'a>,
        ) -> BoxFuture<'a, ChatResult<ToolReceipt>> {
            Box::pin(async {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let backend = Scripted::new(vec![calculator_call()]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut registrar = PluginRegistrar::default();
    registrar.add_tool(Calculator).expect("tool");
    registrar.layer_tool(Wait(entered.clone()));
    let chat = chat(backend, registrar, RunLimits::default());
    let cancellation = Cancellation::default();
    let token = cancellation.clone();
    let task =
        tokio::spawn(async move { chat.request("multiply").cancellation(token).run().await });
    entered.notified().await;
    cancellation.cancel();
    let error = task.await.expect("join").expect_err("cancelled tool");
    assert_eq!(error.partial.tools.len(), 1);
    assert_eq!(
        error.partial.tools[0].result["error"]["kind"],
        "interrupted"
    );
    assert!(matches!(
        error.partial.messages.last(),
        Some(Message::Tool { .. })
    ));
}

#[tokio::test]
async fn runtime_close_interrupts_a_hook_without_native_work() {
    let backend = Scripted::new(Vec::new());
    let mut registrar = PluginRegistrar::default();
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    registrar.before_run("wait", move |_| {
        let notify = notify.clone();
        Box::pin(async move {
            notify.notify_one();
            std::future::pending().await
        })
    });
    let chat = chat(backend, registrar, RunLimits::default());
    let control = chat.0.control.clone();
    let task = tokio::spawn(async move { chat.generate("wait").await });
    entered.notified().await;
    control.shutdown.cancel();
    assert!(matches!(
        task.await.expect("join").expect_err("shutdown").source,
        ChatError::Closed
    ));
}

#[tokio::test]
async fn dropping_client_future_still_finalizes_interrupted_tool_protocol() {
    #[derive(Debug)]
    struct Wait(Arc<tokio::sync::Notify>);
    impl crate::pipeline::ToolLayer for Wait {
        fn call<'a>(
            &'a self,
            _context: &'a mut RunContext,
            _call: ToolCall,
            _next: crate::pipeline::NextTool<'a>,
        ) -> BoxFuture<'a, ChatResult<ToolReceipt>> {
            Box::pin(async {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let backend = Scripted::new(vec![calculator_call()]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(tokio::sync::Notify::new());
    let notify = finished.clone();
    let mut registrar = PluginRegistrar::default();
    registrar.add_tool(Calculator).expect("tool");
    registrar.layer_tool(Wait(entered.clone()));
    registrar.on_run_finished("finish", move |response, _| {
        let notify = notify.clone();
        Box::pin(async move {
            assert_eq!(response.tools.len(), 1);
            notify.notify_one();
            Ok(())
        })
    });
    let chat = chat(backend, registrar, RunLimits::default());
    let client = chat.clone();
    let task = tokio::spawn(async move { client.generate("multiply").await });
    entered.notified().await;
    task.abort();
    assert!(task.await.expect_err("aborted waiter").is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), finished.notified())
        .await
        .expect("runtime finalizes after client disconnect");
    let messages = chat.messages().await;
    assert!(
        matches!(messages.last(), Some(Message::Tool { result, .. }) if result["error"]["kind"] == "interrupted")
    );
}

#[tokio::test]
async fn middleware_can_inspect_run_state_after_awaiting_each_scope() {
    #[derive(Debug)]
    struct Around(Arc<std::sync::Mutex<Vec<&'static str>>>);
    impl crate::pipeline::RunLayer for Around {
        fn call<'a>(
            &'a self,
            context: &'a mut RunContext,
            next: crate::pipeline::NextRun<'a>,
        ) -> BoxFuture<'a, ChatResult<ChatResponse>> {
            Box::pin(async move {
                self.0.lock().expect("log").push("run before");
                let response = next.run(context).await?;
                assert_eq!(response.run_id, context.run_id());
                self.0.lock().expect("log").push("run after");
                Ok(response)
            })
        }
    }
    impl ModelLayer for Around {
        fn call<'a>(
            &'a self,
            context: &'a mut RunContext,
            request: ModelRequest,
            next: NextModel<'a>,
        ) -> BoxFuture<'a, ChatResult<ModelTurn>> {
            Box::pin(async move {
                self.0.lock().expect("log").push("model before");
                let response = next.run(context, request).await?;
                assert!(context.run_id() > 0);
                self.0.lock().expect("log").push("model after");
                Ok(response)
            })
        }
    }
    impl crate::pipeline::ToolLayer for Around {
        fn call<'a>(
            &'a self,
            context: &'a mut RunContext,
            call: ToolCall,
            next: crate::pipeline::NextTool<'a>,
        ) -> BoxFuture<'a, ChatResult<ToolReceipt>> {
            Box::pin(async move {
                self.0.lock().expect("log").push("tool before");
                let response = next.run(context, call).await?;
                assert!(context.run_id() > 0);
                self.0.lock().expect("log").push("tool after");
                Ok(response)
            })
        }
    }
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut registrar = PluginRegistrar::default();
    registrar.layer_run(Around(log.clone()));
    registrar.layer_model(Around(log.clone()));
    registrar.layer_tool(Around(log.clone()));
    registrar.add_tool(Calculator).expect("tool");
    let backend = Scripted::new(vec![calculator_call(), AssistantMessage::text("437")]);
    chat(backend, registrar, RunLimits::default())
        .generate("multiply")
        .await
        .expect("middleware conversation");
    assert_eq!(
        *log.lock().expect("log"),
        [
            "run before",
            "model before",
            "model after",
            "tool before",
            "tool after",
            "model before",
            "model after",
            "run after"
        ]
    );
}

#[tokio::test]
async fn retrieval_hook_uses_typed_service_without_persisting_its_context() {
    let backend = Scripted::new(vec![AssistantMessage::text("answer")]);
    let mut registrar = PluginRegistrar::default();
    registrar
        .provide(Arc::new(String::from("retrieved fact")))
        .expect("service");
    registrar.before_run("retrieve", |context| {
        Box::pin(async move {
            let fact = context.services().get::<String>()?;
            context.context.push((*fact).clone());
            Ok(())
        })
    });
    let chat = chat(backend.clone(), registrar, RunLimits::default());
    let response = chat
        .generate("question")
        .await
        .expect("retrieval integration");
    assert_eq!(backend.requests.lock().await[0].context, ["retrieved fact"]);
    assert_eq!(
        response.messages,
        [
            Message::user("question"),
            Message::Assistant(AssistantMessage::text("answer"))
        ]
    );
}

#[tokio::test]
async fn tool_continuations_use_the_remaining_run_allowance() {
    let backend = Scripted::new(vec![calculator_call(), AssistantMessage::text("437")]);
    let mut registrar = PluginRegistrar::default();
    registrar.add_tool(Calculator).expect("register calculator");
    let limits = RunLimits {
        generated_tokens: 2,
        ..RunLimits::default()
    };
    let chat = chat(backend.clone(), registrar, limits);
    let response = chat
        .generate("19 times 23")
        .await
        .expect("bounded tool continuation");
    assert_eq!(response.usage.generated_tokens, 2);
    let requests = backend.requests.lock().await;
    assert_eq!(requests[0].options.max_new_tokens, Some(2));
    assert_eq!(requests[1].options.max_new_tokens, Some(1));
}
