//! Admission, lifecycle rollback, real worker ownership, and idle lifetime tests.
use super::*;
use crate::{
    engine::test_engine,
    pipeline::{BoxFuture, Service},
};

#[derive(Debug)]
struct Probe {
    name: &'static str,
    events: Arc<std::sync::Mutex<Vec<String>>>,
    fail_start: bool,
}
impl Service for Probe {
    fn start(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async {
            self.events
                .lock()
                .expect("events")
                .push(format!("start {}", self.name));
            if self.fail_start {
                Err(ChatError::Configuration(
                    "deliberate service failure".into(),
                ))
            } else {
                Ok(())
            }
        })
    }
    fn shutdown(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async {
            self.events
                .lock()
                .expect("events")
                .push(format!("stop {}", self.name));
            Ok(())
        })
    }
}
#[derive(Debug)]
struct Probes(Vec<Arc<Probe>>);
impl Plugin for Probes {
    fn configure(&self, registrar: &mut PluginRegistrar) -> ChatResult<()> {
        for probe in &self.0 {
            registrar.lifecycle(probe.clone());
        }
        Ok(())
    }
}
fn config() -> LLMConfig {
    let mut config = LLMConfig::new(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-models/stories260K.gguf"),
    );
    config.gpu_layers = 0;
    config.context_size = 256;
    config.batch_size = 16;
    config.max_new_tokens = 8;
    config.threads = 1;
    config
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Complete worker admission/inspection/release scenario.
async fn required_cache_worker_snapshots_and_context_release() {
    let Some(path) = std::env::var_os("ASSISTANT_CACHE_TEST_MODEL") else {
        assert!(
            std::env::var_os("ASSISTANT_CACHE_REQUIRE_MODEL").is_none(),
            "required worker cache coverage needs ASSISTANT_CACHE_TEST_MODEL"
        );
        eprintln!("SKIP: set ASSISTANT_CACHE_TEST_MODEL for worker cache validation");
        return;
    };
    for (key, value) in [
        (
            crate::engine::KvCacheType::F16,
            crate::engine::KvCacheType::F16,
        ),
        (
            crate::engine::KvCacheType::Q8_0,
            crate::engine::KvCacheType::Q8_0,
        ),
        (
            crate::engine::KvCacheType::Q8_0,
            crate::engine::KvCacheType::Q4_0,
        ),
        (
            crate::engine::KvCacheType::Q4_0,
            crate::engine::KvCacheType::Q4_0,
        ),
    ] {
        let mut config = LLMConfig::new(path.clone());
        config.context_size = 2048;
        config.max_new_tokens = 8;
        config.cache.swa = crate::engine::SwaCacheMode::Compact;
        config.cache.key = key;
        config.cache.value = value;
        let cache = config.cache;
        let runtime = AssistantRuntime::builder(test_engine())
            .model("cache", config)
            .max_contexts(2)
            .build()
            .expect("worker configuration")
            .start()
            .await
            .expect("start cache worker");
        assert!(
            runtime
                .cache_diagnostics()
                .await
                .expect("empty worker diagnostics")["cache"]
                .contexts
                .is_empty()
        );
        let first = runtime.chat();
        let second = runtime.create_chat("cache").expect("second conversation");
        for chat in [&first, &second] {
            chat.request("A local assistant writes careful notes. ".repeat(100))
                .mode(crate::chat::InputMode::Raw)
                .run()
                .await
                .expect("native worker generation");
        }
        let reports = runtime
            .cache_diagnostics()
            .await
            .expect("worker diagnostics");
        let report = &reports["cache"];
        assert_eq!(report.contexts.len(), 2);
        assert!(report.contexts.iter().all(|c| c.requested == cache));
        let one_weights: usize = report.contexts[0].memory.iter().map(|m| m.model).sum();
        assert_eq!(
            one_weights,
            report.memory.iter().map(|m| m.model).sum::<usize>()
        );
        if std::env::var_os("ASSISTANT_CACHE_REQUIRE_VULKAN").is_some() {
            assert!(
                report
                    .memory
                    .iter()
                    .any(|m| m.buft_name.starts_with("Vulkan") && m.context > 0)
            );
        }
        drop(second);
        assert_eq!(
            runtime.cache_diagnostics().await.expect("released context")["cache"]
                .contexts
                .len(),
            1
        );
        let replacement = runtime.create_chat("cache").expect("replacement chat");
        replacement
            .request("New independent state.")
            .mode(crate::chat::InputMode::Raw)
            .run()
            .await
            .expect("reallocate released slot");
        assert_eq!(
            runtime.cache_diagnostics().await.expect("new context")["cache"]
                .contexts
                .len(),
            2
        );
        runtime.shutdown().await.expect("cache worker shutdown");
    }
}

#[tokio::test]
async fn admission_is_bounded_and_shutdown_waits_for_guards() {
    let control = RuntimeControl::new(1);
    let token = Cancellation::default();
    let guard = control
        .admit(token.clone())
        .await
        .expect("first run admitted");
    assert!(matches!(
        control.admit(Cancellation::default()).await,
        Err(ChatError::Limit(_))
    ));
    let closing = control.clone();
    let task = tokio::spawn(async move { closing.cancel_and_wait(Duration::from_secs(1)).await });
    token.cancelled().await;
    assert!(control.is_closed());
    drop(guard);
    task.await.expect("join shutdown").expect("guard drained");
    assert!(matches!(
        control.admit(Cancellation::default()).await,
        Err(ChatError::Closed)
    ));
}

#[tokio::test]
async fn failed_start_rolls_back_successful_services_in_reverse_order() {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let probes = ["a", "b", "fail"]
        .into_iter()
        .map(|name| {
            Arc::new(Probe {
                name,
                events: events.clone(),
                fail_start: name == "fail",
            })
        })
        .collect();
    let error = AssistantRuntime::builder(test_engine())
        .model("main", config())
        .plugin(Probes(probes))
        .build()
        .expect("valid runtime")
        .start()
        .await
        .expect_err("service fails before any model load");
    assert!(matches!(error, ChatError::Startup { .. }));
    assert_eq!(
        *events.lock().expect("events"),
        ["start a", "start b", "start fail", "stop b", "stop a"]
    );
}

#[tokio::test]
async fn missing_model_rolls_back_services_and_reports_native_failure() {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let probe = Arc::new(Probe {
        name: "a",
        events: events.clone(),
        fail_start: false,
    });
    let mut missing = config();
    missing.model_path = "/missing-assistant-test-model.gguf".into();
    let error = AssistantRuntime::builder(test_engine())
        .model("main", missing)
        .plugin(Probes(vec![probe]))
        .build()
        .expect("valid config")
        .start()
        .await
        .expect_err("missing model");
    assert!(matches!(error, ChatError::Startup { .. }));
    assert_eq!(*events.lock().expect("events"), ["start a", "stop a"]);
}

#[tokio::test]
async fn resident_worker_supports_multiple_runs_context_limits_and_shutdown() {
    let config = config();
    if !config.model_path.is_file() {
        eprintln!("SKIP: native runtime needs stories260K.gguf");
        return;
    }
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let probe = Arc::new(Probe {
        name: "a",
        events: events.clone(),
        fail_start: false,
    });
    let runtime = AssistantRuntime::builder(test_engine())
        .model("main", config)
        .max_contexts(1)
        .event_capacity(1)
        .plugin(Probes(vec![probe]))
        .build()
        .expect("valid runtime")
        .start()
        .await
        .expect("load resident worker");
    let chat = runtime.chat();
    for _ in 0..2 {
        chat.clear_working_history().await;
        let response = chat
            .request("Once upon a time")
            .mode(crate::chat::InputMode::Raw)
            .tools(Vec::<String>::new())
            .run()
            .await
            .expect("native run");
        assert_eq!(response.usage.generated_tokens, 8);
        assert!(!response.text().is_empty());
        assert_eq!(
            *events.lock().expect("events"),
            ["start a"],
            "responding does not stop service"
        );
    }
    let other = runtime.create_chat("main").expect("independent chat");
    let error = other
        .request("Once upon a time")
        .mode(crate::chat::InputMode::Raw)
        .run()
        .await
        .expect_err("live context capacity");
    assert!(matches!(error.source, ChatError::Limit(_)));
    runtime.shutdown().await.expect("orderly shutdown");
    assert_eq!(*events.lock().expect("events"), ["start a", "stop a"]);
    assert!(matches!(
        chat.generate("after shutdown")
            .await
            .expect_err("closed")
            .source,
        ChatError::Closed
    ));
}

#[tokio::test]
async fn native_token_window_evicts_old_exchanges_across_many_interactions() {
    let mut config = config();
    if !config.model_path.is_file() {
        eprintln!("SKIP: token window needs stories260K.gguf");
        return;
    }
    config.context_size = 128;
    let actual_capacity = {
        let model = test_engine()
            .load_llm(config.clone())
            .expect("fixture model");
        model.new_context().expect("fixture context").capacity()
    };
    let runtime = AssistantRuntime::builder(test_engine())
        .model("main", config)
        .system("Be brief.")
        .build()
        .expect("config")
        .start()
        .await
        .expect("worker");
    let chat = runtime.chat();
    let mut previous = 0;
    let mut evicted = false;
    for _ in 0..32 {
        let response = chat
            .generate("Once upon a time")
            .await
            .expect("chat remains usable beyond one context window");
        let input = response.usage.input_tokens;
        assert!(
            input + 8 <= actual_capacity,
            "reserved output always fits actual native capacity"
        );
        evicted |= input <= previous;
        previous = input;
    }
    assert!(evicted, "fixture must exercise token-based eviction");
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backpressured_chat_does_not_block_another_native_context() {
    let config = config();
    if !config.model_path.is_file() {
        eprintln!("SKIP: backpressure needs stories260K.gguf");
        return;
    }
    let runtime = AssistantRuntime::builder(test_engine())
        .model("main", config)
        .event_capacity(1)
        .max_contexts(2)
        .build()
        .expect("config")
        .start()
        .await
        .expect("worker");
    let first = runtime.chat();
    let second = runtime.create_chat("main").expect("second chat");
    let entered = Arc::new(Notify::new());
    let notify = entered.clone();
    let (release, blocked) = std::sync::mpsc::channel();
    let blocked = std::sync::Mutex::new(blocked);
    let first_chunk = AtomicBool::new(true);
    let waiting = tokio::spawn(async move {
        first
            .request("Once upon a time")
            .mode(crate::chat::InputMode::Raw)
            .max_new_tokens(128)
            .on_event(move |event| {
                if matches!(event, crate::chat::RunEvent::TextDelta { .. })
                    && first_chunk.swap(false, Ordering::SeqCst)
                {
                    notify.notify_one();
                    // Intentionally stall one consumer, with a bounded fallback so
                    // an assertion failure cannot hang executor shutdown.
                    let _released = blocked
                        .lock()
                        .expect("consumer gate")
                        .recv_timeout(Duration::from_secs(5));
                }
                Ok(())
            })
            .run()
            .await
    });
    entered.notified().await;
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        second
            .request("Once upon a time")
            .mode(crate::chat::InputMode::Raw)
            .run(),
    )
    .await;
    release.send(()).expect("release first consumer");
    waiting
        .await
        .expect("join first request")
        .expect("first response");
    runtime.shutdown().await.expect("shutdown");
    response
        .expect("second chat progresses while first is backpressured")
        .expect("second response");
}
