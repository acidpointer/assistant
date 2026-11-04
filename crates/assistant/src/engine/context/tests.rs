//! Cache, stop, budget, cancellation, and failure recovery contracts.
use super::*;
use crate::engine::{LLMConfig, test_engine};
use std::path::PathBuf;

#[test]
fn stop_buffer_handles_overlap_unicode_and_unmatched_tail() {
    let mut buffer = StopBuffer::new(vec![String::new(), "🙂🙂!".into(), "aaaa".into()]);
    assert_eq!(buffer.push("hello 🙂"), ("hello ".into(), false));
    assert_eq!(buffer.push("🙂"), (String::new(), false));
    assert_eq!(buffer.push("! hidden"), (String::new(), true));
    assert_eq!(buffer.text, "hello ");
    let mut buffer = StopBuffer::new(vec!["aaaa".into()]);
    assert_eq!(buffer.push("aaa"), (String::new(), false));
    assert_eq!(buffer.flush(), "aaa");
}

#[tokio::test]
async fn cancellation_wakes_all_waiters_and_late_waiters() {
    let cancellation = Cancellation::default();
    let first = cancellation.clone();
    let second = cancellation.clone();
    let a = tokio::spawn(async move { first.cancelled().await });
    let b = tokio::spawn(async move { second.cancelled().await });
    tokio::task::yield_now().await;
    cancellation.cancel();
    a.await.expect("first waiter");
    b.await.expect("second waiter");
    tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
        .await
        .expect("late waiter observes cancellation");
}

#[test]
fn native_generation_cache_stops_and_failure_recovery() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-models/stories260K.gguf");
    if !path.is_file() {
        eprintln!("SKIP: native generation needs stories260K.gguf");
        return;
    }
    let mut config = LLMConfig::new(path);
    config.gpu_layers = 0;
    config.context_size = 256;
    config.batch_size = 8;
    config.threads = 1;
    config.max_new_tokens = 32;
    let model = test_engine().load_llm(config).expect("load tiny model");
    let mut context = model.new_context().expect("allocate native context");
    let input = context
        .prepare_raw("Once upon a time")
        .expect("prepare prompt");
    let mut chunks = Vec::new();
    let baseline = context
        .generate(&input, GenerationOptions::default(), &mut |text: &str| {
            chunks.push(text.to_owned());
            Ok(StreamCmd::Continue)
        })
        .expect("baseline generation");
    assert_eq!(chunks.concat(), baseline.text);
    assert!(chunks.len() >= 3, "fixture spans multiple chunks");
    assert_eq!(baseline.usage.prefill_tokens, input.token_count());
    assert!(baseline.metrics.generation_ns > 0);
    assert_eq!(
        baseline.generation_time.as_nanos(),
        u128::from(baseline.metrics.generation_ns)
    );
    assert!(baseline.metrics.first_token_ns.is_some());
    // Deliberate consumer delay must affect wall time, not active native time.
    let mut delayed = context
        .begin(
            &input,
            GenerationOptions {
                max_new_tokens: Some(2),
                ..GenerationOptions::default()
            },
        )
        .expect("timed cursor");
    context.advance(&mut delayed).expect("first native step");
    let active_before = delayed.metrics.prefill_ns + delayed.metrics.generation_ns;
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        delayed.metrics.prefill_ns + delayed.metrics.generation_ns,
        active_before
    );
    while !delayed.is_finished() {
        context.advance(&mut delayed).expect("finish cursor");
    }
    let delayed = delayed.result().expect("timed result");
    assert!(
        delayed.metrics.elapsed_ns
            >= delayed.metrics.prefill_ns + delayed.metrics.generation_ns + 15_000_000
    );
    let cached = context.cached_tokens();
    let cancelled = Cancellation::default();
    cancelled.cancel();
    let result = context
        .generate(
            &input,
            GenerationOptions {
                cancellation: cancelled,
                ..GenerationOptions::default()
            },
            &mut |_: &str| Ok(StreamCmd::Continue),
        )
        .expect("pre-cancelled result");
    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert_eq!(context.cached_tokens(), cached);
    let oversized = GenerationOptions {
        max_new_tokens: Some(256),
        ..GenerationOptions::default()
    };
    assert!(matches!(
        context
            .generate(&input, oversized, &mut |_: &str| Ok(StreamCmd::Continue))
            .expect_err("reject overflow")
            .source,
        EngineError::Budget { .. }
    ));
    assert_eq!(context.cached_tokens(), cached);
    for stops in [
        vec![chunks[..2].concat()],
        vec![String::new(), chunks[1].clone()],
        vec![format!("{} unmatched", baseline.text)],
        vec![String::new()],
    ] {
        let mut streamed = String::new();
        let result = context
            .generate(
                &input,
                GenerationOptions {
                    stop_sequences: stops.clone(),
                    ..GenerationOptions::default()
                },
                &mut |text: &str| {
                    streamed.push_str(text);
                    Ok(StreamCmd::Continue)
                },
            )
            .expect("generation with stops");
        let stop = find_stop(&baseline.text, &stops);
        assert_eq!(
            result.text,
            baseline.text[..stop.unwrap_or(baseline.text.len())]
        );
        assert_eq!(streamed, result.text);
        if stop.is_some() {
            assert_eq!(result.finish_reason, FinishReason::Stop);
        }
    }
    // An exact append reuses every successfully evaluated prefix token.
    let mut state = context
        .begin(&input, GenerationOptions::default())
        .expect("begin prompt");
    while state.cursor < input.token_count() {
        context.advance(&mut state).expect("prefill batch");
    }
    let append = context
        .prepare_raw("Once upon a time there")
        .expect("append prompt");
    assert!(
        append.tokens.starts_with(&input.tokens),
        "fixture is a token append"
    );
    let reuse = context
        .begin(&append, GenerationOptions::default())
        .expect("begin append");
    assert_eq!(reuse.usage.cached_tokens, input.token_count());
    assert!(
        matches!(context.advance(&mut state), Err(EngineError::State(_))),
        "stale cursors cannot mutate newer work"
    );
    context.reset();
    let mut failed = context
        .begin(
            &input,
            GenerationOptions {
                max_output_bytes: 0,
                ..GenerationOptions::default()
            },
        )
        .expect("begin byte-bound failure");
    while context.advance(&mut failed).is_ok() {
        assert!(!failed.is_finished(), "fixture emits output");
    }
    assert_eq!(
        context.cached_tokens(),
        0,
        "failed decode bookkeeping invalidates reuse"
    );
    let recovered = context
        .generate(&input, GenerationOptions::default(), &mut |_: &str| {
            Ok(StreamCmd::Continue)
        })
        .expect("recover from failed generation");
    assert_eq!(recovered.text, baseline.text);
    let other = model.new_context().expect("second context");
    let mut cursor = context
        .begin(&input, GenerationOptions::default())
        .expect("begin cursor");
    let mut other = other;
    assert!(
        matches!(other.advance(&mut cursor), Err(EngineError::State(_))),
        "cursor belongs to its original context"
    );
}
