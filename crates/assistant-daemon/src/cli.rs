//! CLI-only configuration. Server mode is the default. Diagnostic input modes
//! require --no-web and are validated before any native or listener I/O.
use assistant::engine::{
    AttentionRotation, ExecutionMode, KvCacheType, LLMConfig, LlamaFlashAttnType, SwaCacheMode,
    Thinking,
};
use clap::{Parser, ValueEnum};
use std::{
    net::{IpAddr, Ipv4Addr},
    num::NonZeroU32,
    path::PathBuf,
};

// Independent CLI flags map to typed library options after parsing.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(about = "Local assistant daemon with an embedded web UI")]
pub(crate) struct Args {
    /// Local model weights.
    #[arg(long)]
    pub(crate) model: PathBuf,
    /// Enable durable memory in this private directory; requires --embedding-model.
    #[arg(
        long,
        value_name = "DIR",
        requires = "embedding_model",
        conflicts_with = "raw"
    )]
    pub(crate) memory_dir: Option<PathBuf>,
    /// Local E5 Q8 embedding weights; checksum-verified and never downloaded.
    #[arg(long, value_name = "GGUF", requires = "memory_dir")]
    pub(crate) embedding_model: Option<PathBuf>,
    /// Stable caller identity for durable headless requests.
    #[arg(long, default_value = "local-cli")]
    pub(crate) client_id: String,
    /// Caller key for one-shot retry/reconciliation with --memory-dir.
    #[arg(long,requires_all=["memory_dir","prompt"])]
    pub(crate) request_id: Option<String>,
    /// Diagnostic/testing one-shot, or durable interaction with --memory-dir; requires --no-web.
    #[arg(short, long, requires = "no_web", conflicts_with_all = ["interactive", "daemon"])]
    pub(crate) prompt: Option<String>,
    /// Compatibility alias for the default resident mode.
    #[arg(long, conflicts_with = "prompt")]
    pub(crate) daemon: bool,
    /// Disable the HTTP listener, UI, streams and web plugin entirely.
    #[arg(long)]
    pub(crate) no_web: bool,
    /// Explicit stdin interaction mode for manual testing; requires --no-web.
    #[arg(long, requires = "no_web", conflicts_with_all = ["prompt", "daemon"])]
    pub(crate) interactive: bool,
    /// HTTP bind address (IPv4 or IPv6 literal).
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST), conflicts_with = "no_web")]
    pub(crate) host: IpAddr,
    /// HTTP port; zero asks the OS to allocate a port.
    #[arg(long, default_value_t = 8080, conflicts_with = "no_web")]
    pub(crate) port: u16,
    /// Raw completion for decoder testing, without chat or tools.
    #[arg(long, requires = "no_web")]
    pub(crate) raw: bool,
    #[arg(long, default_value_t = 1000)]
    pub(crate) gpu_layers: u32,
    #[arg(long, default_value_t = 8192)]
    pub(crate) ctx_size: u32,
    #[arg(long, default_value_t = 512)]
    pub(crate) batch_size: u32,
    #[arg(long)]
    pub(crate) ubatch_size: Option<NonZeroU32>,
    #[arg(long, value_enum, default_value = "f16")]
    pub(crate) cache_type_k: CacheType,
    #[arg(long, value_enum, default_value = "f16")]
    pub(crate) cache_type_v: CacheType,
    #[arg(long, value_enum, default_value = "full")]
    pub(crate) swa_cache: SwaMode,
    #[arg(long, value_enum, default_value = "auto")]
    pub(crate) flash_attn: FlashMode,
    #[arg(long, value_enum, default_value = "auto")]
    pub(crate) attn_rotation: RotationMode,
    /// Force CPU weights, attention and operators for reference measurements.
    #[arg(long)]
    pub(crate) cpu: bool,
    /// Require validated Vulkan cache execution (experimental PQ3/PQ3).
    #[arg(long, conflicts_with = "cpu")]
    pub(crate) require_vulkan: bool,
    /// Emit owned native cache/memory snapshots to stderr after each interaction.
    #[arg(long, requires = "no_web")]
    pub(crate) diagnostics: bool,
    /// Sampled-token ceiling per model turn, including reasoning/protocol.
    #[arg(long, default_value_t = 4096)]
    pub(crate) max_tokens: usize,
    /// Total sampled tokens across model turns and tool continuations.
    #[arg(long, default_value_t = 8192)]
    pub(crate) run_max_tokens: usize,
    #[arg(long, default_value_t = 4)]
    pub(crate) threads: u32,
    #[arg(long)]
    pub(crate) thinking: bool,
    #[arg(long)]
    pub(crate) no_tools: bool,
    #[arg(long, conflicts_with_all = ["no_tools","memory_dir"], requires = "no_web")]
    pub(crate) require_tool: bool,
    /// Wall-clock allowance per interaction, excluding model startup.
    #[arg(long, default_value_t = 600)]
    pub(crate) timeout_seconds: u64,
    /// Print tool requests/results; prompt contents are not logged.
    #[arg(long, requires = "no_web")]
    pub(crate) trace: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CacheType {
    F16,
    #[value(name = "q8_0")]
    Q8,
    #[value(name = "q4_0")]
    Q4,
    #[cfg(feature = "turboquant")]
    #[value(name = "pq3_0")]
    Pq3,
}
impl From<CacheType> for KvCacheType {
    fn from(value: CacheType) -> Self {
        match value {
            CacheType::F16 => Self::F16,
            CacheType::Q8 => Self::Q8_0,
            CacheType::Q4 => Self::Q4_0,
            #[cfg(feature = "turboquant")]
            CacheType::Pq3 => Self::PQ3_0,
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SwaMode {
    Full,
    Compact,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum FlashMode {
    Auto,
    On,
    Off,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum RotationMode {
    Auto,
    Disabled,
}

pub(crate) fn model_config(args: &Args) -> assistant::engine::EngineResult<LLMConfig> {
    let mut config = LLMConfig::new(&args.model);
    config.gpu_layers = if args.cpu { 0 } else { args.gpu_layers };
    config.execution = if args.cpu {
        ExecutionMode::Cpu
    } else if args.require_vulkan {
        ExecutionMode::RequireVulkan
    } else {
        ExecutionMode::Auto
    };
    config.context_size = args.ctx_size;
    config.batch_size = args.batch_size;
    config.microbatch_size = args.ubatch_size;
    config.cache.key = args.cache_type_k.into();
    config.cache.value = args.cache_type_v.into();
    config.cache.swa = match args.swa_cache {
        SwaMode::Full => SwaCacheMode::Full,
        SwaMode::Compact => SwaCacheMode::Compact,
    };
    config.cache.flash_attention = match args.flash_attn {
        FlashMode::Auto => LlamaFlashAttnType::Auto,
        FlashMode::On => LlamaFlashAttnType::Enabled,
        FlashMode::Off => LlamaFlashAttnType::Disabled,
    };
    config.cache.rotation = match args.attn_rotation {
        RotationMode::Auto => AttentionRotation::Auto,
        RotationMode::Disabled => AttentionRotation::Disabled,
    };
    config.max_new_tokens = args.max_tokens;
    config.threads = args.threads;
    config.thinking = if args.thinking {
        Thinking::Enabled
    } else {
        Thinking::Disabled
    };
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn memory_web_command_keeps_cli_paths_and_requested_port() {
        let args = Args::try_parse_from([
            "assistant",
            "--model",
            "chat.gguf",
            "--memory-dir",
            "private-memory",
            "--embedding-model",
            "e5.gguf",
            "--port",
            "3333",
        ])
        .expect("memory and WebUI command");
        assert_eq!(
            args.memory_dir.as_deref(),
            Some(Path::new("private-memory"))
        );
        assert_eq!(args.embedding_model.as_deref(), Some(Path::new("e5.gguf")));
        assert_eq!(args.port, 3333);
        assert!(!args.no_web);
        let temporary = Args::try_parse_from(["assistant", "--model", "chat.gguf"])
            .expect("memory remains opt-in");
        assert!(temporary.memory_dir.is_none() && temporary.embedding_model.is_none());
    }
}
