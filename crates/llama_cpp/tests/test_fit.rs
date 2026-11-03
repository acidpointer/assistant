//! Tests for [`llama_cpp::fit`].

use std::path::Path;
use std::sync::OnceLock;

use llama_cpp::context::params::LlamaContextParams;
use llama_cpp::fit::{fit_params, get_device_memory_data, FitParams, FitParamsError};
use llama_cpp::llama_backend::LlamaBackend;
use llama_cpp::model::params::LlamaModelParams;

static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

fn backend() -> &'static LlamaBackend {
    BACKEND.get_or_init(|| LlamaBackend::init().expect("initialize the process test backend"))
}

#[test]
fn fit_params_invalid_path() {
    let err = fit_params(
        backend(),
        Path::new("model\0bad.gguf"),
        FitParams::default(),
    )
    .unwrap_err();
    assert_eq!(err, FitParamsError::InvalidPath);
}

#[test]
fn fit_params_missing_model() {
    let err = fit_params(
        backend(),
        Path::new("/no/such/model.gguf"),
        FitParams::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, FitParamsError::Failed | FitParamsError::CouldNotFit),
        "expected fit failure for missing model, got {err:?}",
    );
}

#[test]
fn get_device_memory_data_missing_model() {
    let err = get_device_memory_data(
        Path::new("/no/such/model.gguf"),
        &LlamaModelParams::default(),
        &LlamaContextParams::default(),
        llama_cpp_sys::GGML_LOG_LEVEL_ERROR,
    )
    .unwrap_err();
    assert_eq!(err, llama_cpp::fit::DeviceMemoryError::QueryFailed);
}

#[test]
fn fit_params_default_margins_length() {
    let params = FitParams::default();
    assert!(params.margins.len() >= llama_cpp::max_devices());
    assert_eq!(params.n_ctx_min, 4096);
    assert!(params.context_params.n_ctx().is_none());
}
