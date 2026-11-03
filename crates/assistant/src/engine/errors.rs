use llama_cpp::{context, llama_batch::BatchInitError, prelude::*};

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum EngineError {
    #[error("llama.cpp error: {0}")]
    LlamaCppError(#[from] LLamaCppError),

    #[error("llama.cpp error loading model: {0}")]
    LlamaModelLoadError(#[from] LlamaModelLoadError),

    #[error("llama.cpp context load error: {0}")]
    LlamaContextLoadError(#[from] LlamaContextLoadError),

    #[error("llama.cpp failed to add tokens to batch: {0}")]
    LlamaBatchAddError(#[from] BatchAddError),

    #[error("llama.cpp decode error: {0}")]
    LlamaDecodeError(#[from] DecodeError),

    #[error("Shim error: {0}")]
    ShimError(#[from] ShimError),

    #[error("Detokenizer error: {0}")]
    DetokenizeError(#[from] DetokenizeError),

    #[error("llama.cpp batch init error: {0}")]
    BatchInitError(#[from] BatchInitError),

    #[error("Engine already initialized")]
    EngineAlreadyInitializedError,

    #[error("Engine not initialized")]
    EngineNotInitializedError,

    #[error("The prompt produced no tokens")]
    NoTokensError,

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Backend of llama.cpp was not initialized! Call init() first!")]
    BackendNotInitializedError,

    #[error("Token budget overflow")]
    TokenBudgetOverflowError,
}
