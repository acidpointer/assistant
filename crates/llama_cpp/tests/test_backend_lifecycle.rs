//! Exercise process-global backend epochs in an isolated integration binary.
//!
//! One test owns initialization; no shared fixture backend is initialized here.
//! Models must keep native initialization alive even after the public handle
//! is dropped, and competing/reentrant initialization must fail without waiting.

mod support;

use llama_cpp::llama_backend::{LlamaBackend, NumaStrategy};
use llama_cpp::model::{params::LlamaModelParams, LlamaModel};
use llama_cpp::LLamaCppError;

#[test]
fn initialization_numa_and_model_leases_preserve_one_live_epoch() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LlamaBackend>();
    assert_send_sync::<LlamaModel>();
    assert_eq!(
        LlamaBackend::init_numa(NumaStrategy::COUNT),
        Err(LLamaCppError::InvalidNumaStrategy)
    );

    // NUMA must load the CPU backend first even when it is the first operation
    // in this process. This aborted natively before the initialization fix.
    let backend =
        LlamaBackend::init_numa(NumaStrategy::MIRROR).expect("initialize CPU before NUMA");
    assert_eq!(
        LlamaBackend::init(),
        Err(LLamaCppError::BackendAlreadyInitialized)
    );
    let cloned = backend.clone();
    drop(backend);
    assert_eq!(
        LlamaBackend::init(),
        Err(LLamaCppError::BackendAlreadyInitialized)
    );
    let backend = cloned;
    if let Some(fixture) = support::model::find_test_model() {
        let params = LlamaModelParams::default()
            .with_vocab_only(fixture.vocab_only)
            .with_n_gpu_layers(0);
        let model = LlamaModel::load_from_file(&backend, &fixture.path, &params)
            .expect("load lifecycle fixture");
        drop(backend);
        assert_eq!(
            LlamaBackend::init(),
            Err(LLamaCppError::BackendAlreadyInitialized)
        );
        assert!(
            model.get_vocab().n_tokens() > 0,
            "model remains usable without public backend handle"
        );
        drop(model);
    } else {
        eprintln!("SKIP: model lease check needs a model or tokenizer fixture");
        drop(backend);
    }
    drop(LlamaBackend::init().expect("last lease completed shutdown"));

    let backend = LlamaBackend::init().expect("synthetic model epoch");
    let synthetic = llama_cpp::quantize::QuantModelDesc::llama(64, 128, 1, 1)
        .build(&backend)
        .expect("synthetic model retains backend too");
    drop(backend);
    assert_eq!(
        LlamaBackend::init(),
        Err(LLamaCppError::BackendAlreadyInitialized)
    );
    assert_eq!(synthetic.n_embd(), 64);
    drop(synthetic);

    // The old Drop reset the gate before freeing native state. Competing
    // epochs must never overlap initialization and final free.
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..32 {
                    match LlamaBackend::init() {
                        Ok(backend) => drop(backend),
                        Err(LLamaCppError::BackendAlreadyInitialized) => std::thread::yield_now(),
                        Err(error) => panic!("unexpected initialization failure: {error}"),
                    }
                }
            });
        }
    });
    drop(LlamaBackend::init().expect("gate reopened after competing epochs"));
}
