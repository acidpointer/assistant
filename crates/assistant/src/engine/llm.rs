//! Loaded weights and application defaults. The engine outlives the model;
//! contexts borrow the model. Native loading parameters are temporary, so the
//! loaded wrapper can be shared while each context stays thread-confined.
use super::{EngineError, EngineResult, LLMContext, ModelEngine};
use llama_cpp::{
    context::params::LlamaContextParams,
    model::{LlamaModel, params::LlamaModelParams},
};
use std::{num::NonZeroU32, path::PathBuf};

/// Thinking behavior for templates that support it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Thinking {
    /// Ask the model to answer without a separate thinking phase.
    #[default]
    Disabled,
    /// Enable the template's thinking mode.
    Enabled,
}

/// Loading settings and defaults for contexts and generations.
/// Values are validated before native loading or allocation.
#[derive(Debug, Clone)]
pub struct LLMConfig {
    /// GGUF to load; never included in default application logs.
    pub model_path: PathBuf,
    /// Number of layers to offload; zero selects CPU weights.
    pub gpu_layers: u32,
    /// Per-context token capacity.
    pub context_size: u32,
    /// Maximum tokens evaluated in one prefill step.
    pub batch_size: u32,
    /// CPU threads for both prefill and decoding.
    pub threads: u32,
    /// Default generated-token ceiling, including protocol and reasoning.
    pub max_new_tokens: usize,
    /// Default template thinking mode.
    pub thinking: Thinking,
}
impl LLMConfig {
    /// Defaults suitable for interactive experiments; GPU offload is enabled.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: path.into(),
            gpu_layers: 1000,
            context_size: 4096,
            batch_size: 512,
            threads: 4,
            max_new_tokens: 512,
            thinking: Thinking::Disabled,
        }
    }
    /// Check bounds before any native state changes.
    /// # Errors
    /// Returns a description of an invalid field or combination.
    pub fn validate(&self) -> EngineResult<()> {
        if self.context_size == 0 || self.context_size > i32::MAX as u32 {
            return Err(EngineError::Configuration(
                "context_size must fit a positive native position",
            ));
        }
        if self.batch_size == 0 || self.batch_size > self.context_size {
            return Err(EngineError::Configuration(
                "batch_size must be positive and no larger than context_size",
            ));
        }
        if self.threads == 0 || self.threads > i32::MAX as u32 {
            return Err(EngineError::Configuration(
                "threads must fit a positive native count",
            ));
        }
        if self.max_new_tokens == 0 || self.max_new_tokens >= self.context_size as usize {
            return Err(EngineError::Configuration(
                "max_new_tokens must leave room for input",
            ));
        }
        Ok(())
    }
}

/// Loaded model weights. Construct through [`ModelEngine::load_llm`].
#[derive(Debug)]
pub struct LLMModel<'engine> {
    engine: &'engine ModelEngine,
    model: LlamaModel,
    config: LLMConfig,
}
impl<'engine> LLMModel<'engine> {
    pub(super) fn load(engine: &'engine ModelEngine, config: LLMConfig) -> EngineResult<Self> {
        config.validate()?;
        let params = LlamaModelParams::default().with_n_gpu_layers(config.gpu_layers);
        let model = LlamaModel::load_from_file(engine.backend_ref(), &config.model_path, &params)?;
        Ok(Self {
            engine,
            model,
            config,
        })
    }
    /// Defaults used for new contexts and requests.
    #[must_use]
    pub fn config(&self) -> &LLMConfig {
        &self.config
    }
    /// Borrow the native model for advanced diagnostics.
    #[must_use]
    pub fn model_ref(&self) -> &LlamaModel {
        &self.model
    }
    /// Allocate independent inference state borrowing these weights.
    /// # Errors
    /// Returns native allocation or batch-allocation failures.
    pub fn new_context(&self) -> EngineResult<LLMContext<'_>> {
        let n_ctx = NonZeroU32::new(self.config.context_size)
            .ok_or(EngineError::Configuration("zero context size"))?;
        let threads = i32::try_from(self.config.threads)
            .map_err(|_| EngineError::Configuration("thread count overflow"))?;
        let params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(self.config.batch_size)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let native = self.model.new_context(self.engine.backend_ref(), params)?;
        LLMContext::new(native, self)
    }
}
