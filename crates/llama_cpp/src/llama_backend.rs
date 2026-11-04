//! Process-wide native backend initialization and retained ownership.
//!
//! An atomic gate stays closed through initialization and final shutdown. Models
//! retain the same lease, so dropping the public handle cannot free native state
//! while a model is live. Only one initialization epoch may exist at a time. Handles are
//! Send + Sync; contexts retain their separate thread-confinement rules.

// Every `unsafe` block below is justified in a `SAFETY:` comment; keep it that
// way. See the note on the lint in the workspace `Cargo.toml`.
#![deny(clippy::undocumented_unsafe_blocks)]

use crate::LLamaCppError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Representation of an initialized llama backend
/// This is required as a parameter for most llama functions as the backend must be initialized
/// before any llama functions are called. This type is proof of initialization.
///
/// Construction is restricted to checked initialization. Cloning shares the
/// existing initialization lease; it never initializes another native backend.
///
/// ```compile_fail
/// use llama_cpp::LlamaBackend;
/// let backend = LlamaBackend {};
/// ```
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LlamaBackend {
    pub(crate) state: Arc<BackendState>,
}

/// Retained by models until after their native model has been freed.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BackendState;

/// Remains true from before initialization until native shutdown completes.
/// Reentrant initialization from a log callback returns an error rather than
/// blocking on a lock held by the native operation that emitted that log.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

impl LlamaBackend {
    /// Close the gate before initialization and keep it closed through shutdown.
    fn initialize(strategy: Option<NumaStrategy>) -> crate::Result<Self> {
        if strategy == Some(NumaStrategy::COUNT) {
            return Err(LLamaCppError::InvalidNumaStrategy);
        }
        INITIALIZED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| LLamaCppError::BackendAlreadyInitialized)?;
        // SAFETY: the gate excludes initialization and final shutdown. It is
        // reopened only after native free completes; no live lease can coexist
        // with a successful compare-exchange.
        unsafe {
            let _ = llama_cpp_sys::llama_cpp_rs_runtime_safety_abi_v1();
            #[cfg(feature = "turboquant")]
            let _ = llama_cpp_sys::ggml_pq3_reference_abi_v1();
            #[cfg(feature = "turboquant")]
            let _ = llama_cpp_sys::ggml_pq3_cpu_reference_abi_v1();
            #[cfg(feature = "turboquant")]
            let _ = llama_cpp_sys::llama_pq3_graph_abi_v1();
            llama_cpp_sys::llama_backend_init();
        }
        let backend = Self {
            state: Arc::new(BackendState),
        };
        if let Some(strategy) = strategy {
            if strategy != NumaStrategy::DISABLED {
                // SAFETY: initialization above loaded the registered backends.
                // The device is borrowed only for this null check.
                let cpu = unsafe {
                    llama_cpp_sys::ggml_backend_dev_by_type(
                        llama_cpp_sys::GGML_BACKEND_DEVICE_TYPE_CPU,
                    )
                };
                if cpu.is_null() {
                    return Err(LLamaCppError::NumaBackendUnavailable);
                }
            }
            // SAFETY: a non-disabled strategy has a loaded CPU backend; COUNT
            // was rejected. DISABLED is a native no-op and needs no CPU device.
            unsafe { llama_cpp_sys::llama_numa_init(strategy.into()) };
        }
        Ok(backend)
    }

    /// Initialize the llama backend (without numa).
    ///
    /// # Examples
    ///
    /// ```
    ///# use llama_cpp::llama_backend::LlamaBackend;
    ///# use llama_cpp::LLamaCppError;
    ///# use std::error::Error;
    ///
    ///# fn main() -> Result<(), Box<dyn Error>> {
    ///
    ///
    /// let backend = LlamaBackend::init()?;
    /// // the llama backend can only be initialized once
    /// assert_eq!(Err(LLamaCppError::BackendAlreadyInitialized), LlamaBackend::init());
    ///
    ///# Ok(())
    ///# }
    /// ```
    /// # Errors
    ///
    /// Returns [`LLamaCppError::BackendAlreadyInitialized`] if the backend has already been initialized.
    #[tracing::instrument(skip_all)]
    pub fn init() -> crate::Result<LlamaBackend> {
        Self::initialize(None)
    }

    /// Initialize the llama backend (with numa).
    /// ```
    ///# use llama_cpp::llama_backend::LlamaBackend;
    ///# use std::error::Error;
    ///# use llama_cpp::llama_backend::NumaStrategy;
    ///
    ///# fn main() -> Result<(), Box<dyn Error>> {
    ///
    /// let llama_backend = LlamaBackend::init_numa(NumaStrategy::MIRROR)?;
    ///
    ///# Ok(())
    ///# }
    /// ```
    /// # Errors
    ///
    /// Returns [`LLamaCppError::BackendAlreadyInitialized`] for a live backend
    /// or model, [`LLamaCppError::InvalidNumaStrategy`] for `COUNT`, or
    /// [`LLamaCppError::NumaBackendUnavailable`] when no CPU backend is loaded.
    #[tracing::instrument(skip_all)]
    pub fn init_numa(strategy: NumaStrategy) -> crate::Result<LlamaBackend> {
        Self::initialize(Some(strategy))
    }

    /// Silence native llama.cpp/ggml logs for the rest of the process.
    ///
    /// This is a compatibility wrapper around
    /// [`crate::logging::set_log_callback`]. It installs a persistent no-op
    /// callback, matching the old behaviour.
    ///
    /// Prefer the explicit form when you want to control logging yourself:
    ///
    /// ```no_run
    /// llama_cpp::logging::set_log_callback(|level, message| {
    ///     eprintln!("{level:?}: {message}");
    /// });
    /// ```
    ///
    /// Note that this covers the llama/ggml core logger only. The `common/`
    /// logger and the clip/mtmd logger have separate configuration paths.
    #[deprecated(
        note = "use llama_cpp::logging::set_log_callback(|_, _| {}) or a real callback instead"
    )]
    pub fn void_logs(&mut self) {
        crate::logging::set_log_callback(|_level, _message| {});
    }
}

/// A rusty wrapper around `numa_strategy`.
///
/// ## Description
/// Represents different NUMA (Non-Uniform Memory Access) strategies for memory management
/// in multi-core or multi-processor systems.
///
/// ## See more
/// <https://github.com/ggerganov/llama.cpp/blob/master/ggml/include/ggml-cpu.h#L25-L32>
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum NumaStrategy {
    /// The NUMA strategy is disabled. No NUMA-aware optimizations are applied.
    /// Memory allocation will not consider NUMA node locality.
    DISABLED,

    /// Distribute memory across NUMA nodes. This strategy aims to balance memory usage
    /// across all available NUMA nodes, potentially improving load balancing and preventing
    /// memory hotspots on a single node. It may use round-robin or another method to
    /// distribute allocations.
    DISTRIBUTE,

    /// Isolate memory to specific NUMA nodes. Memory allocations will be restricted to
    /// specific NUMA nodes, potentially reducing contention and improving locality for
    /// processes or threads bound to a particular node.
    ISOLATE,

    /// Use `numactl` to manage memory and processor affinities. This strategy utilizes
    /// the `numactl` command or library to bind processes or memory allocations to specific
    /// NUMA nodes or CPUs, providing fine-grained control over memory placement.
    NUMACTL,

    /// Mirror memory across NUMA nodes. This strategy creates duplicate memory copies
    /// on multiple NUMA nodes, which can help with fault tolerance and redundancy,
    /// ensuring that each NUMA node has access to a copy of the memory.
    MIRROR,

    /// A placeholder representing the total number of strategies available.
    /// Typically used for iteration or determining the number of strategies in the enum.
    COUNT,
}

/// An invalid numa strategy was provided.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct InvalidNumaStrategy(
    /// The invalid numa strategy that was provided.
    pub llama_cpp_sys::ggml_numa_strategy,
);

impl TryFrom<llama_cpp_sys::ggml_numa_strategy> for NumaStrategy {
    type Error = InvalidNumaStrategy;

    fn try_from(value: llama_cpp_sys::ggml_numa_strategy) -> Result<Self, Self::Error> {
        match value {
            llama_cpp_sys::GGML_NUMA_STRATEGY_DISABLED => Ok(Self::DISABLED),
            llama_cpp_sys::GGML_NUMA_STRATEGY_DISTRIBUTE => Ok(Self::DISTRIBUTE),
            llama_cpp_sys::GGML_NUMA_STRATEGY_ISOLATE => Ok(Self::ISOLATE),
            llama_cpp_sys::GGML_NUMA_STRATEGY_NUMACTL => Ok(Self::NUMACTL),
            llama_cpp_sys::GGML_NUMA_STRATEGY_MIRROR => Ok(Self::MIRROR),
            llama_cpp_sys::GGML_NUMA_STRATEGY_COUNT => Ok(Self::COUNT),
            value => Err(InvalidNumaStrategy(value)),
        }
    }
}

impl From<NumaStrategy> for llama_cpp_sys::ggml_numa_strategy {
    fn from(value: NumaStrategy) -> Self {
        match value {
            NumaStrategy::DISABLED => llama_cpp_sys::GGML_NUMA_STRATEGY_DISABLED,
            NumaStrategy::DISTRIBUTE => llama_cpp_sys::GGML_NUMA_STRATEGY_DISTRIBUTE,
            NumaStrategy::ISOLATE => llama_cpp_sys::GGML_NUMA_STRATEGY_ISOLATE,
            NumaStrategy::NUMACTL => llama_cpp_sys::GGML_NUMA_STRATEGY_NUMACTL,
            NumaStrategy::MIRROR => llama_cpp_sys::GGML_NUMA_STRATEGY_MIRROR,
            NumaStrategy::COUNT => llama_cpp_sys::GGML_NUMA_STRATEGY_COUNT,
        }
    }
}

/// Drops the llama backend.
/// ```
///
///# use llama_cpp::llama_backend::LlamaBackend;
///# use std::error::Error;
///
///# fn main() -> Result<(), Box<dyn Error>> {
/// let backend = LlamaBackend::init()?;
/// drop(backend);
/// // can be initialized again after being dropped
/// let backend = LlamaBackend::init()?;
///# Ok(())
///# }
///
/// ```
impl Drop for BackendState {
    fn drop(&mut self) {
        // SAFETY: the last lease owns shutdown. Models release their lease only
        // after llama_model_free. The gate excludes a new initialization until
        // native shutdown has finished and the flag is reset.
        unsafe { llama_cpp_sys::llama_backend_free() };
        INITIALIZED.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numa_from_and_to() {
        let numas = [
            NumaStrategy::DISABLED,
            NumaStrategy::DISTRIBUTE,
            NumaStrategy::ISOLATE,
            NumaStrategy::NUMACTL,
            NumaStrategy::MIRROR,
            NumaStrategy::COUNT,
        ];

        for numa in &numas {
            let from = llama_cpp_sys::ggml_numa_strategy::from(*numa);
            let to = NumaStrategy::try_from(from).expect("Failed to convert from and to");
            assert_eq!(*numa, to);
        }
    }

    #[test]
    fn check_invalid_numa() {
        let invalid = 800;
        let invalid = NumaStrategy::try_from(invalid);
        assert_eq!(invalid, Err(InvalidNumaStrategy(invalid.unwrap_err().0)));
    }
}
