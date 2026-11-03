//! Explicit process-wide backend ownership. The singleton intentionally lives
//! until process exit. Models borrow it; duplicate initialization fails closed.
use super::{EngineError, EngineResult, LLMConfig, LLMModel};
use llama_cpp::{LLamaCppError, LogLevel, llama_backend::LlamaBackend};
use std::sync::{Once, OnceLock};

static ENGINE: OnceLock<ModelEngine> = OnceLock::new();

/// Process-wide native backend and model factory.
#[derive(Debug)]
pub struct ModelEngine {
    backend: LlamaBackend,
}

impl ModelEngine {
    /// Initialize the singleton, installing logging before native operations.
    /// # Errors
    /// Returns `AlreadyInitialized` for competing or repeated initialization,
    /// or the underlying backend failure. Failed initialization is retryable.
    pub fn init() -> EngineResult<()> {
        static LOGGING: Once = Once::new();
        LOGGING.call_once(|| {
            llama_cpp::logging::set_log_callback(|level, message| match level {
                LogLevel::Error => tracing::error!(target: "llama_cpp::native", "{message}"),
                LogLevel::Warn => tracing::warn!(target: "llama_cpp::native", "{message}"),
                _ => tracing::debug!(target: "llama_cpp::native", "{message}"),
            });
        });
        let backend = LlamaBackend::init().map_err(|error| match error {
            LLamaCppError::BackendAlreadyInitialized => EngineError::AlreadyInitialized,
            error => EngineError::Backend(error),
        })?;
        ENGINE
            .set(Self { backend })
            .map_err(|_| EngineError::AlreadyInitialized)
    }
    /// Borrow the initialized singleton.
    /// # Errors
    /// Returns `NotInitialized` until explicit initialization completes.
    pub fn get_instance() -> EngineResult<&'static Self> {
        ENGINE.get().ok_or(EngineError::NotInitialized)
    }
    /// Load weights using this initialization epoch.
    /// # Errors
    /// Returns configuration or native loading failures.
    pub fn load_llm(&self, config: LLMConfig) -> EngineResult<LLMModel<'_>> {
        LLMModel::load(self, config)
    }
    /// Borrow proof that the backend remains initialized.
    #[must_use]
    pub fn backend_ref(&self) -> &LlamaBackend {
        &self.backend
    }
}
