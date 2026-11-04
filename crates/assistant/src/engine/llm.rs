//! Loaded weights and application defaults. The engine outlives the model;
//! contexts borrow the model. Native loading parameters are temporary, so the
//! loaded wrapper can be shared while each context stays thread-confined.
use super::{EngineError, EngineResult, LLMContext, ModelEngine};
use llama_cpp::{
    context::{cache::KvCacheConfig, params::LlamaContextParams},
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
    /// Physical prefill microbatch; unset resolves to `min(batch_size, 512)`.
    pub microbatch_size: Option<NonZeroU32>,
    /// Immutable attention-cache policy for every context using these weights.
    pub cache: KvCacheConfig,
    /// Disable KQV and operator offloading as well as GPU weights for a CPU reference.
    pub execution: ExecutionMode,
    /// CPU threads for both prefill and decoding.
    pub threads: u32,
    /// Default generated-token ceiling, including protocol and reasoning. The
    /// worker reduces this to actual available space; it is not a reservation.
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
            microbatch_size: None,
            cache: KvCacheConfig::default(),
            execution: ExecutionMode::Auto,
            threads: 4,
            max_new_tokens: 512,
            thinking: Thinking::Disabled,
        }
    }
    /// Check bounds before any native state changes.
    /// # Errors
    /// Returns a description of an invalid field or combination.
    pub fn validate(&self) -> EngineResult<()> {
        self.cache.validate()?;
        if self.execution == ExecutionMode::RequireVulkan {
            #[cfg(not(all(feature = "vulkan", feature = "turboquant")))]
            return Err(EngineError::Configuration(
                "required Vulkan codec validation needs vulkan and turboquant features",
            ));
            #[cfg(all(feature = "vulkan", feature = "turboquant"))]
            if self.gpu_layers == 0 || self.cache.key != super::KvCacheType::PQ3_0 {
                return Err(EngineError::Configuration(
                    "required Vulkan mode currently validates PQ3/PQ3 with GPU attention placement",
                ));
            }
        }
        if self
            .microbatch_size
            .is_some_and(|size| size.get() > self.batch_size)
        {
            return Err(EngineError::Configuration(
                "microbatch_size exceeds batch_size",
            ));
        }
        if self.execution == ExecutionMode::Cpu && self.gpu_layers != 0 {
            return Err(EngineError::Configuration(
                "CPU execution requires gpu_layers = 0",
            ));
        }
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
        if self.max_new_tokens == 0 {
            return Err(EngineError::Configuration(
                "max_new_tokens must be positive",
            ));
        }
        Ok(())
    }
}

/// Operator placement policy; CPU mode is useful for verified reference runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecutionMode {
    /// Allow native operator/KQV offloading independently of weight placement.
    #[default]
    Auto,
    /// Require the experimental PQ3 Vulkan path, including native device preflight.
    RequireVulkan,
    /// Disable operator and KQV offloading; requires zero GPU weight layers.
    Cpu,
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
        config.cache.validate_for(&model.cache_topology()?)?;
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
            .with_n_ubatch(
                self.config
                    .microbatch_size
                    .map_or(self.config.batch_size.min(512), NonZeroU32::get),
            )
            .with_offload_kqv(self.config.execution != ExecutionMode::Cpu)
            .with_op_offload(self.config.execution != ExecutionMode::Cpu)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let params = self.config.cache.apply(params)?;
        let native = self.model.new_context(self.engine.backend_ref(), params)?;
        LLMContext::new(native, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{KvCacheType, LlamaFlashAttnType};
    #[test]
    fn desired_output_ceiling_is_independent_of_context_capacity() {
        let mut config = LLMConfig::new("not-a-model.gguf");
        config.context_size = 256;
        config.batch_size = 128;
        config.max_new_tokens = 4096;
        config
            .validate()
            .expect("worker fits a ceiling to actual input/capacity");
        config.max_new_tokens = 0;
        assert!(matches!(
            config.validate(),
            Err(EngineError::Configuration(_))
        ));
    }
    #[test]
    fn invalid_cache_microbatch_and_cpu_policies_fail_before_loading() {
        let mut config = LLMConfig::new("not-a-model.gguf");
        config.batch_size = 8;
        config.microbatch_size = NonZeroU32::new(9);
        assert!(matches!(
            config.validate(),
            Err(EngineError::Configuration(_))
        ));
        config.microbatch_size = None;
        config
            .validate()
            .expect("unset microbatch supports small batches");
        config.cache.value = KvCacheType::Q4_0;
        config.cache.flash_attention = LlamaFlashAttnType::Disabled;
        assert!(matches!(
            config.validate(),
            Err(EngineError::CacheConfig(_))
        ));
        config.cache = KvCacheConfig::default();
        config.execution = ExecutionMode::Cpu;
        assert!(matches!(
            config.validate(),
            Err(EngineError::Configuration(_))
        ));
        config.gpu_layers = 0;
        config.validate().expect("explicit CPU placement policy");
    }
}
