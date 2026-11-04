//! Opt-in LFM3 cache validation using identical teacher-forced tokens. This is
//! separate from model tool-choice quality. No downloads or silent CPU fallback
//! are allowed when `ASSISTANT_CACHE_REQUIRE_VULKAN` is present.
use super::*;
use crate::engine::{AttentionRotation, KvCacheType, LLMConfig, SwaCacheMode, test_engine};
use llama_cpp::context::cache::CacheLayerKind;

fn prefill<'m>(context: &mut LLMContext<'m>, input: &PreparedInput<'m>) -> GenerationState<'m> {
    let mut state = context
        .begin(
            input,
            GenerationOptions {
                max_new_tokens: Some(8),
                ..Default::default()
            },
        )
        .expect("begin fixed input");
    while state.cursor < input.token_count() {
        context.advance(&mut state).expect("evaluate fixed tokens");
    }
    state
}

#[track_caller]
fn compare(reference: &[f32], actual: &[f32], tolerance: f32) -> (f64, f32) {
    assert_eq!(reference.len(), actual.len());
    let mut squares = 0.0;
    let mut maximum = 0.0_f32;
    for (a, b) in reference.iter().zip(actual) {
        assert!(a.is_finite() && b.is_finite(), "finite logits");
        let delta = (a - b).abs();
        squares += f64::from(delta).powi(2);
        maximum = maximum.max(delta);
    }
    assert!(
        maximum <= tolerance,
        "maximum logit difference {maximum} exceeds {tolerance}"
    );
    (
        (squares / f64::from(u32::try_from(reference.len()).expect("native vocabulary fits u32")))
            .sqrt(),
        maximum,
    )
}

// Probability metrics are characterization, not a task-quality approval gate.
fn distribution(reference: &[f32], actual: &[f32], target: usize) -> serde_json::Value {
    let log_z = |values: &[f32]| {
        let maximum = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        f64::from(maximum)
            + values
                .iter()
                .map(|v| f64::from(*v - maximum).exp())
                .sum::<f64>()
                .ln()
    };
    let (zr, za) = (log_z(reference), log_z(actual));
    let kl = reference
        .iter()
        .zip(actual)
        .map(|(r, a)| {
            let log_p = f64::from(*r) - zr;
            log_p.exp() * (log_p - (f64::from(*a) - za))
        })
        .sum::<f64>();
    let top = |values: &[f32]| {
        values
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|v| v.0)
    };
    serde_json::json!({"kl_reference_to_profile":kl,"top1_agrees":top(reference)==top(actual),"target_nll_reference":zr-f64::from(reference[target]),"target_nll_profile":za-f64::from(actual[target])})
}

fn fixed_input<'m>(model: &'m LLMModel<'m>) -> PreparedInput<'m> {
    let tokens = model.model_ref().str_to_token(&"The local assistant keeps careful notes. A red box contains seven stones. A blue box contains three stones. ".repeat(160), AddBos::Always).expect("tokenize fixed corpus");
    PreparedInput {
        model,
        tokens,
        chat: None,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Ordered reference/candidate comparisons over fixed tokens.
fn required_representative_text_quality() {
    let Some(path) = std::env::var_os("ASSISTANT_QUALITY_MODEL") else {
        assert!(
            std::env::var_os("ASSISTANT_QUALITY_REQUIRE_MODEL").is_none()
                && std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_none(),
            "required text evaluation needs ASSISTANT_QUALITY_MODEL"
        );
        eprintln!("SKIP: set ASSISTANT_QUALITY_MODEL for representative text characterization");
        return;
    };
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/cache-quality-v1.json"
    ))
    .expect("fixed numerical corpus");
    let step = usize::try_from(
        fixture["acceptance"]["numerical_token_stride"]
            .as_u64()
            .expect("sampling stride"),
    )
    .expect("bounded stride");
    let engine = test_engine();
    let mut references: Vec<Vec<Vec<f32>>> = Vec::new();
    #[allow(unused_mut)]
    let mut profiles = vec![
        (KvCacheType::F16, SwaCacheMode::Full, 0),
        (KvCacheType::F16, SwaCacheMode::Full, 1),
        (KvCacheType::F16, SwaCacheMode::Compact, 0),
        (KvCacheType::Q8_0, SwaCacheMode::Compact, 0),
        (KvCacheType::Q4_0, SwaCacheMode::Compact, 0),
    ];
    if std::env::var_os("ASSISTANT_QUALITY_INCLUDE_PQ3").is_some() {
        #[cfg(feature = "turboquant")]
        profiles.extend([
            (KvCacheType::PQ3_0, SwaCacheMode::Full, 0),
            (KvCacheType::PQ3_0, SwaCacheMode::Compact, 0),
        ]);
        #[cfg(not(feature = "turboquant"))]
        panic!("PQ3 numerical evaluation requires the turboquant feature");
    }
    for (profile, swa, repeat) in profiles {
        let mut config = LLMConfig::new(path.clone());
        config.max_new_tokens = 8;
        config.cache.key = profile;
        config.cache.value = profile;
        config.cache.swa = swa;
        #[cfg(feature = "turboquant")]
        if profile == KvCacheType::PQ3_0
            && std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_some()
        {
            config.execution = crate::engine::ExecutionMode::RequireVulkan;
        }
        let model = engine.load_llm(config).expect("text profile model");
        let mut context = model.new_context().expect("text profile context");
        if std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_some() {
            assert!(
                context
                    .cache_diagnostics()
                    .expect("text placement")
                    .weight_devices
                    .iter()
                    .any(|d| d.starts_with("Vulkan"))
            );
        }
        for (text_index, item) in fixture["texts"]
            .as_array()
            .expect("authored texts")
            .iter()
            .enumerate()
        {
            context.reset();
            let text = item["text"].as_str().expect("corpus text");
            let tokens = model
                .model_ref()
                .str_to_token(text, AddBos::Always)
                .expect("corpus tokens");
            assert!(tokens.len() > step && tokens.len() + 8 < context.capacity());
            let mut checkpoints: Vec<_> = (step..tokens.len()).step_by(step).collect();
            checkpoints.push(tokens.len() - 1);
            checkpoints.sort_unstable();
            checkpoints.dedup();
            let token_hash = tokens
                .iter()
                .flat_map(|t| t.0.to_le_bytes())
                .fold(0xcbf2_9ce4_8422_2325_u64, |h, v| {
                    (h ^ u64::from(v)).wrapping_mul(0x100_0000_01b3)
                });
            let mut rows = Vec::new();
            for (point, count) in checkpoints.into_iter().enumerate() {
                let input = PreparedInput {
                    model: &model,
                    tokens: tokens[..count].to_vec(),
                    chat: None,
                };
                prefill(&mut context, &input);
                let logits = context.native.get_logits().to_vec();
                assert!(logits.iter().all(|v| v.is_finite()));
                if profile == KvCacheType::F16 && swa == SwaCacheMode::Full && repeat == 0 {
                    rows.push(logits.clone());
                }
                let reference = if references.is_empty()
                    || repeat == 0 && profile == KvCacheType::F16 && swa == SwaCacheMode::Full
                {
                    &logits
                } else {
                    &references[text_index][point]
                };
                let metrics = distribution(
                    reference,
                    &logits,
                    usize::try_from(tokens[count].0).expect("target token"),
                );
                #[allow(unused_mut)]
                let mut pq3_observation = serde_json::Value::Null;
                #[cfg(feature = "turboquant")]
                if profile == KvCacheType::PQ3_0 {
                    let observed = context
                        .native
                        .cache_info()
                        .expect("PQ3 numerical snapshot")
                        .pq3
                        .expect("PQ3 identity");
                    if std::env::var_os("ASSISTANT_QUALITY_REQUIRE_VULKAN").is_some() {
                        assert!(
                            observed.execution
                                == llama_cpp::context::cache::Pq3Execution::VulkanRequired
                                && observed.indexed_writes > 0
                                && observed.attention_operations > 0
                                && observed.observed_paths().next().is_some(),
                            "required numerical PQ3 GPU execution"
                        );
                    }
                    pq3_observation = serde_json::json!({"execution":format!("{:?}",observed.execution),"completed_indexed_writes":observed.indexed_writes,"completed_attention_operations":observed.attention_operations,"observed_paths":observed.observed_paths().map(|p|format!("{p:?}")).collect::<Vec<_>>()});
                }
                eprintln!(
                    "CACHE_TEXT_QUALITY {}",
                    serde_json::json!({"stage":if profile.name()=="pq3_0" {"candidate"} else {"baseline"},"profile":profile.name(),"swa":format!("{swa:?}"),"repeat":repeat,"corpus":item["id"],"tokens":count,"token_stream_fnv1a64":format!("{token_hash:016x}"),"metrics":metrics,"pq3_observation":pq3_observation})
                );
            }
            if profile == KvCacheType::F16 && swa == SwaCacheMode::Full && repeat == 0 {
                references.push(rows);
            }
        }
    }
}

#[test]
fn required_context_rotation_isolation() {
    let Some(path) = std::env::var_os("ASSISTANT_CACHE_TEST_MODEL") else {
        assert!(
            std::env::var_os("ASSISTANT_CACHE_REQUIRE_MODEL").is_none()
                && std::env::var_os("ASSISTANT_CACHE_REQUIRE_VULKAN").is_none(),
            "required rotation coverage needs ASSISTANT_CACHE_TEST_MODEL"
        );
        eprintln!("SKIP: set ASSISTANT_CACHE_TEST_MODEL for rotation isolation");
        return;
    };
    let mut config = LLMConfig::new(path);
    config.context_size = 512;
    config.batch_size = 32;
    config.max_new_tokens = 8;
    config.cache.key = KvCacheType::Q8_0;
    config.cache.value = KvCacheType::Q8_0;
    config.cache.swa = SwaCacheMode::Compact;
    let engine = test_engine();
    let model = engine.load_llm(config.clone()).expect("rotation model");
    let automatic = model.new_context().expect("automatic rotation context");
    let before = automatic.cache_diagnostics().expect("automatic snapshot");
    if std::env::var_os("ASSISTANT_CACHE_REQUIRE_VULKAN").is_some() {
        assert!(
            before
                .weight_devices
                .iter()
                .any(|d| d.starts_with("Vulkan"))
        );
    }
    let mut disabled = config.cache;
    disabled.rotation = AttentionRotation::Disabled;
    let params = disabled
        .apply(
            llama_cpp::context::params::LlamaContextParams::default()
                .with_n_ctx(std::num::NonZeroU32::new(512))
                .with_n_batch(32)
                .with_n_ubatch(32),
        )
        .expect("disabled policy");
    let other = model
        .model_ref()
        .new_context(engine.backend_ref(), params)
        .expect("simultaneous disabled context");
    let info = other.cache_info().expect("disabled snapshot");
    assert!(
        info.arenas
            .iter()
            .all(|a| a.key_rotation == 0 && a.value_rotation == 0)
    );
    assert_eq!(
        automatic
            .cache_diagnostics()
            .expect("retained automatic context")
            .effective,
        before.effective
    );
    let later = model.new_context().expect("later automatic context");
    assert_eq!(
        later
            .cache_diagnostics()
            .expect("later automatic snapshot")
            .effective,
        before.effective
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered lifecycle scenario per cache profile.
fn required_lfm3_cache_profiles_wrap_reuse_and_recover() {
    let Some(path) = std::env::var_os("ASSISTANT_CACHE_TEST_MODEL") else {
        assert!(
            std::env::var_os("ASSISTANT_CACHE_REQUIRE_MODEL").is_none()
                && std::env::var_os("ASSISTANT_CACHE_REQUIRE_VULKAN").is_none(),
            "required cache coverage needs ASSISTANT_CACHE_TEST_MODEL"
        );
        eprintln!("SKIP: set ASSISTANT_CACHE_TEST_MODEL for LFM3 cache validation");
        return;
    };
    let mut config = LLMConfig::new(path);
    config.context_size = 2048;
    let microbatch: u32 = std::env::var("ASSISTANT_CACHE_UBATCH")
        .map_or(128, |s| s.parse().expect("numeric cache microbatch"));
    config.batch_size = 256_u32.max(microbatch);
    config.microbatch_size =
        Some(std::num::NonZeroU32::new(microbatch).expect("positive cache microbatch"));
    config.max_new_tokens = 8;
    let engine = test_engine();
    let model = engine
        .load_llm(config.clone())
        .expect("load required cache model");
    let topology = model.model_ref().cache_topology().expect("topology");
    assert!(
        topology
            .layers
            .iter()
            .any(|l| l.kind == CacheLayerKind::Sliding),
        "fixture must exercise SWA"
    );
    assert!(
        topology
            .layers
            .iter()
            .any(|l| l.kind == CacheLayerKind::Recurrent),
        "fixture must exercise hybrid state"
    );
    if std::env::var_os("ASSISTANT_CACHE_REQUIRE_VULKAN").is_some() {
        assert!(
            topology
                .layers
                .iter()
                .filter(|l| l.kind != CacheLayerKind::None)
                .any(|l| l.device_name.starts_with("Vulkan")),
            "required Vulkan model offload unavailable"
        );
    }
    let mut reference = model.new_context().expect("FP16 full reference");
    let mut input = fixed_input(&model);
    input.tokens.truncate(2033);
    assert_eq!(
        input.tokens.len(),
        2033,
        "corpus covers several wraps and near capacity"
    );
    let checkpoints = [63, 255, 257, 767, 769, 1537, 2032];
    let mut expected = Vec::new();
    let mut expected_replay = Vec::new();
    let mut replay_reference = model.new_context().expect("fresh full reference");
    for count in checkpoints {
        let mut prompt = input.clone();
        prompt.tokens.truncate(count);
        prefill(&mut reference, &prompt);
        expected.push(reference.native.get_logits().to_vec());
        replay_reference.reset();
        prefill(&mut replay_reference, &prompt);
        expected_replay.push(replay_reference.native.get_logits().to_vec());
    }
    let full_report = reference.cache_diagnostics().expect("full accounting");
    drop(replay_reference);
    drop(reference);
    drop(input);
    drop(model);
    for (key, value, rotation) in [
        (KvCacheType::F16, KvCacheType::F16, AttentionRotation::Auto),
        (
            KvCacheType::Q8_0,
            KvCacheType::Q8_0,
            AttentionRotation::Auto,
        ),
        (
            KvCacheType::Q8_0,
            KvCacheType::Q4_0,
            AttentionRotation::Auto,
        ),
        (
            KvCacheType::Q4_0,
            KvCacheType::Q4_0,
            AttentionRotation::Auto,
        ),
        (
            KvCacheType::Q8_0,
            KvCacheType::Q8_0,
            AttentionRotation::Disabled,
        ),
    ] {
        for swa in [SwaCacheMode::Full, SwaCacheMode::Compact] {
            config.cache.key = key;
            config.cache.value = value;
            config.cache.swa = swa;
            config.cache.rotation = rotation;
            let model = engine.load_llm(config.clone()).expect("load cache profile");
            let mut context = model.new_context().expect("allocate compact cache");
            let mut replay_context = model.new_context().expect("compact replay context");
            let report = context.cache_diagnostics().expect("compact report");
            assert!(report.effective.inspection_available);
            assert_eq!(report.effective.microbatch_size, microbatch);
            let sliding = report
                .effective
                .arenas
                .iter()
                .find(|a| a.kind == CacheLayerKind::Sliding)
                .expect("SWA arena");
            if swa == SwaCacheMode::Compact {
                assert!(sliding.cells < 2048, "compact allocation must be bounded");
            } else {
                assert_eq!(sliding.cells, 2048);
            }
            if swa == SwaCacheMode::Compact || key != KvCacheType::F16 {
                assert!(
                    report.kv_payload_estimate_bytes.expect("known payload")
                        < full_report.kv_payload_estimate_bytes.expect("full payload")
                );
            }
            assert!(report.context_compute_bytes().expect("checked memory") > 0);
            if value != KvCacheType::F16 {
                assert!(report.effective.flash_attention);
            }
            if rotation == AttentionRotation::Disabled {
                assert!(
                    report
                        .effective
                        .arenas
                        .iter()
                        .all(|a| a.key_rotation == 0 && a.value_rotation == 0)
                );
            }
            let mut input = fixed_input(&model);
            input.tokens.truncate(2033);
            let mut previous = 0;
            for ((count, baseline), replay_baseline) in
                checkpoints.into_iter().zip(&expected).zip(&expected_replay)
            {
                let mut prompt = input.clone();
                prompt.tokens.truncate(count);
                let state = prefill(&mut context, &prompt);
                assert_eq!(
                    state.usage.cached_tokens, previous,
                    "exact append reuses evaluated prefix"
                );
                let logits = context.native.get_logits().to_vec();
                let (rms, max) = compare(
                    baseline,
                    &logits,
                    if key == KvCacheType::F16 {
                        0.20
                    } else {
                        f32::MAX
                    },
                );
                if key == KvCacheType::F16 {
                    assert!(rms < 0.03, "compact FP16 numerical regression: RMS {rms}");
                }
                eprintln!(
                    "CACHE_QUALITY {}",
                    serde_json::json!({"key":key.name(),"value":value.name(),"rotation":format!("{rotation:?}"),"swa":format!("{swa:?}"),"microbatch":microbatch,"tokens":count,"logit_rms":rms,"logit_max":max,"distribution":distribution(baseline,&logits,usize::try_from(input.tokens[count].0).expect("nonnegative fixture token"))})
                );
                replay_context.reset();
                let replay = prefill(&mut replay_context, &prompt);
                assert_eq!(replay.usage.cached_tokens, 0);
                compare(
                    replay_baseline,
                    replay_context.native.get_logits(),
                    if key == KvCacheType::F16 {
                        0.20
                    } else {
                        f32::MAX
                    },
                );
                previous = count;
            }
            drop(replay_context);
            // A divergent/trimmed history rebuilds rather than rewinding hybrid state.
            let short = context
                .prepare_raw("A different local task.")
                .expect("divergent prompt");
            let state = prefill(&mut context, &short);
            assert_eq!(state.usage.cached_tokens, 0);
            let cancelled = Cancellation::default();
            cancelled.cancel();
            let cached = context.cached_tokens();
            let mut cancelled_state = context
                .begin(
                    &input,
                    GenerationOptions {
                        cancellation: cancelled,
                        ..Default::default()
                    },
                )
                .expect("pre-cancelled begin");
            context
                .advance(&mut cancelled_state)
                .expect("pre-cancelled step");
            assert_eq!(context.cached_tokens(), cached);
            context.reset();
            let mut superseded = prefill(&mut context, &short);
            let mut current = context
                .begin(&input, GenerationOptions::default())
                .expect("new work");
            assert!(matches!(
                context.advance(&mut superseded),
                Err(EngineError::State(_))
            ));
            context.advance(&mut current).expect("first prefill batch");
            current.options.cancellation.cancel();
            context
                .advance(&mut current)
                .expect("cancel during prefill");
            let replay = prefill(&mut context, &short);
            assert_eq!(replay.usage.cached_tokens, 0);
            let mut failure = context
                .begin(
                    &short,
                    GenerationOptions {
                        max_output_bytes: 0,
                        ..Default::default()
                    },
                )
                .expect("delivery failure setup");
            for _ in 0..16 {
                if context.advance(&mut failure).is_err() {
                    break;
                }
                assert!(
                    !failure.is_finished(),
                    "fixture must generate a non-EOG output for failure test"
                );
            }
            assert_eq!(context.cached_tokens(), 0, "failure poisons cache reuse");
            let mut generated = String::new();
            context
                .generate(&short, GenerationOptions::default(), &mut |text: &str| {
                    generated.push_str(text);
                    Ok(StreamCmd::Continue)
                })
                .expect("generation after recovery");
            assert!(!generated.is_empty(), "real generation");
            let mut decode_cancel = prefill(&mut context, &short);
            context
                .advance(&mut decode_cancel)
                .expect("first decode token");
            decode_cancel.options.cancellation.cancel();
            context.advance(&mut decode_cancel).expect("cancel decode");
            assert_eq!(decode_cancel.reason, Some(FinishReason::Cancelled));
            context
                .generate(&short, GenerationOptions::default(), &mut |_: &str| {
                    Err(EngineError::Delivery("test stream closed".into()))
                })
                .expect_err("stream delivery failure");
            context
                .generate(&short, GenerationOptions::default(), &mut |_: &str| {
                    Ok(StreamCmd::Continue)
                })
                .expect("recover from stream delivery failure");
            drop(context);
            let mut a = model.new_context().expect("reallocate context");
            let mut b = model.new_context().expect("independent context");
            let prompt = a
                .prepare_raw("Separate contexts have independent state.")
                .expect("interleave input");
            let mut sa = a
                .begin(&prompt, GenerationOptions::default())
                .expect("begin A");
            let mut sb = b
                .begin(&prompt, GenerationOptions::default())
                .expect("begin B");
            while sa.cursor < prompt.token_count() || sb.cursor < prompt.token_count() {
                if sa.cursor < prompt.token_count() {
                    a.advance(&mut sa).expect("A prefill");
                }
                if sb.cursor < prompt.token_count() {
                    b.advance(&mut sb).expect("B prefill");
                }
            }
            compare(a.native.get_logits(), b.native.get_logits(), 0.005);
        }
    }
}
