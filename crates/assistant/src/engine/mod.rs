//! Native inference foundation. Models borrow the process singleton and contexts
//! borrow models. Contexts are worker-confined; prepared inputs bind rendering,
//! tokenization, grammar, and parsing to one model. Errors are typed and bounded
//! generation failures carry partial progress.
mod context;
mod diagnostics;
#[allow(clippy::module_inception)] // Preserve the existing engine source layout.
mod engine;
mod errors;
mod helpers;
mod llm;

pub(crate) use context::GenerationState;
pub use context::{
    Cancellation, FinishReason, Generation, GenerationFailure, GenerationOptions,
    GenerationProgress, LLMContext, ParseMode, PreparedInput, StreamCmd, TokenSink, Usage,
};
pub use diagnostics::{CacheDiagnostics, ContextRequest};
pub use engine::ModelEngine;
pub use errors::{EngineError, EngineResult};
pub use llama_cpp::context::{
    cache::{AttentionRotation, KvCacheConfig, KvCacheType, SwaCacheMode},
    params::LlamaFlashAttnType,
};
pub use llm::{ExecutionMode, LLMConfig, LLMModel, Thinking};

#[cfg(test)]
pub(crate) fn test_engine() -> &'static ModelEngine {
    static INITIALIZE: std::sync::Once = std::sync::Once::new();
    INITIALIZE.call_once(|| ModelEngine::init().expect("initialize test engine"));
    ModelEngine::get_instance().expect("test engine initialized")
}
