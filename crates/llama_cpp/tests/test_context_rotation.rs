//! Verify scoped native rotation preferences without environment mutation.
//!
//! One test owns the global Rust default and log callback. Contexts are created
//! concurrently from one immutable model, each on its own inference thread.

mod support;

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use llama_cpp::context::params::LlamaContextParams;
use llama_cpp::llama_backend::LlamaBackend;
use llama_cpp::model::{params::LlamaModelParams, LlamaModel};
use llama_cpp::quantize::{attn_rot_disabled, set_attn_rot_disabled};

#[test]
fn rotation_defaults_and_context_opt_out_never_rewrite_environment() {
    let environment = std::env::var_os("LLAMA_ATTN_ROT_DISABLE");
    set_attn_rot_disabled(true);
    assert!(attn_rot_disabled());
    assert_eq!(std::env::var_os("LLAMA_ATTN_ROT_DISABLE"), environment);
    set_attn_rot_disabled(false);
    assert_eq!(std::env::var_os("LLAMA_ATTN_ROT_DISABLE"), environment);

    let params = LlamaContextParams::default().with_attn_rot_disabled(true);
    assert!(params.attn_rot_disabled());
    assert!(
        params.clone().attn_rot_disabled(),
        "cloning preserves the native context preference"
    );
    let Some(fixture) = support::model::find_test_model().filter(|f| !f.vocab_only) else {
        eprintln!("SKIP: native context rotation test needs a full model");
        return;
    };
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&records);
    llama_cpp::logging::set_log_callback(move |_, message| {
        if message.contains("attention rotation force disabled") {
            captured
                .lock()
                .expect("rotation records")
                .push((std::thread::current().id(), message.to_owned()));
        }
    });
    let backend = LlamaBackend::init().expect("initialize backend");
    let model = LlamaModel::load_from_file(
        &backend,
        &fixture.path,
        &LlamaModelParams::default().with_n_gpu_layers(0),
    )
    .expect("load rotation fixture");
    let ids = std::thread::scope(|scope| {
        let disabled = scope.spawn(|| {
            let params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(64))
                .with_attn_rot_disabled(true);
            let _context = model
                .new_context(&backend, params)
                .expect("disabled context");
            std::thread::current().id()
        });
        let enabled = scope.spawn(|| {
            let params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(64))
                .with_attn_rot_disabled(false);
            let _context = model
                .new_context(&backend, params)
                .expect("default context");
            std::thread::current().id()
        });
        (
            disabled.join().expect("disabled worker"),
            enabled.join().expect("default worker"),
        )
    });
    let records = records.lock().expect("read captured records");
    assert!(records
        .iter()
        .any(|(thread, message)| *thread == ids.0 && message.contains("context params")));
    assert!(
        !records
            .iter()
            .any(|(thread, message)| *thread == ids.1 && message.contains("context params")),
        "disabled preference must not leak across threads"
    );
    assert_eq!(std::env::var_os("LLAMA_ATTN_ROT_DISABLE"), environment);
    drop(records);
    // A later construction on this thread must also restore its scoped state.
    let _context = model
        .new_context(
            &backend,
            LlamaContextParams::default().with_n_ctx(NonZeroU32::new(64)),
        )
        .expect("subsequent default context");
    llama_cpp::logging::set_log_callback(|_, _| {});
}
