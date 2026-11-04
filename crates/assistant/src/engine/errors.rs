//! Typed failures at the native inference boundary. Errors preserve their causes;
//! generation adds partial progress separately rather than fabricating a result.
use llama_cpp::{
    DecodeError, LLamaCppError, LlamaContextLoadError, LlamaModelLoadError, ShimError,
    StringToTokenError,
    llama_batch::{BatchAddError, BatchInitError},
    sampling::SamplerInitError,
    token::detokenizer::DetokenizeError,
};

/// Native initialization, validation, preparation, or execution failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// A backend is already initializing or initialized.
    #[error("engine already initialized")]
    AlreadyInitialized,
    /// Explicit initialization has not completed.
    #[error("engine not initialized")]
    NotInitialized,
    /// Invalid application configuration.
    #[error("invalid configuration: {0}")]
    Configuration(&'static str),
    /// The requested attention cache is incompatible with the model or algorithm.
    #[error(transparent)]
    CacheConfig(#[from] llama_cpp::context::cache::CacheConfigError),
    /// Input and reserved output do not fit the allocated sequence.
    #[error("context budget exceeded: {input} input + {output} output > {capacity} capacity")]
    Budget {
        /// Tokens in the complete rendered input.
        input: usize,
        /// Reserved newly generated tokens.
        output: usize,
        /// Actual native sequence capacity.
        capacity: usize,
    },
    /// A request targets another model instance.
    #[error("prepared input belongs to another model")]
    ModelMismatch,
    /// A bounded payload exceeded its limit.
    #[error("resource limit exceeded: {0}")]
    Limit(&'static str),
    /// The operation is invalid for the current generation state.
    #[error("invalid generation state: {0}")]
    State(&'static str),
    /// Output delivery failed.
    #[error("output delivery failed: {0}")]
    Delivery(String),
    /// A process-wide native operation failed.
    #[error(transparent)]
    Backend(#[from] LLamaCppError),
    /// Model loading failed.
    #[error(transparent)]
    Model(#[from] LlamaModelLoadError),
    /// Context allocation failed.
    #[error(transparent)]
    Context(#[from] LlamaContextLoadError),
    /// Batch allocation failed.
    #[error(transparent)]
    Batch(#[from] BatchInitError),
    /// A batch entry was rejected.
    #[error(transparent)]
    BatchEntry(#[from] BatchAddError),
    /// Native evaluation failed.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// A guarded native helper failed.
    #[error(transparent)]
    Shim(#[from] ShimError),
    /// Input tokenization failed.
    #[error(transparent)]
    Tokenize(#[from] StringToTokenError),
    /// Output bytes could not be decoded.
    #[error(transparent)]
    Detokenize(#[from] DetokenizeError),
    /// Sampler allocation failed.
    #[error(transparent)]
    Sampler(#[from] SamplerInitError),
    /// Serialized chat metadata was invalid.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
/// Result of an engine operation.
pub type EngineResult<T> = Result<T, EngineError>;
