//! Isolated cache-profile benchmark over the public engine API. Each child
//! retains weights and one context for cold/warm trials. All measurement ends
//! synchronize native work. Results are local JSONL artifacts, never model text.
use assistant::engine::{
    AttentionRotation, ExecutionMode, GenerationOptions, KvCacheType, LLMConfig, ModelEngine,
    StreamCmd, SwaCacheMode,
};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::Write,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

const CORPUS: &str = "The local assistant keeps careful notes. A red box contains seven stones. A blue box contains three stones. ";

#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)] // Independent CLI switches, mapped to typed engine policy.
struct Args {
    /// Select matrix profiles explicitly; useful for bounded 32K compact-only runs.
    #[arg(long, value_delimiter = ',')]
    profiles: Vec<String>,
    /// Include experimental full/compact PQ3 cells in the default matrix.
    #[arg(long)]
    include_pq3: bool,
    /// Fixed authored prose/code or the retained repetitive legacy fixture.
    #[arg(long, value_enum, default_value_t = Corpus::Legacy)]
    corpus: Corpus,
    #[arg(long)]
    model: PathBuf,
    /// Context capacities; each matrix cell runs in a fresh process.
    #[arg(long, value_delimiter = ',', default_value = "4096")]
    contexts: Vec<u32>,
    #[arg(long, default_value_t = 512)]
    batch: u32,
    #[arg(long, default_value_t = 512)]
    ubatch: u32,
    /// Desired prefill length. Actual token count is recorded (ASCII prefix).
    #[arg(long, default_value_t = 1024)]
    prompt_tokens: usize,
    #[arg(long, default_value_t = 64)]
    decode_tokens: usize,
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    #[arg(long, default_value_t = 4)]
    threads: u32,
    #[arg(long)]
    cpu: bool,
    #[arg(long)]
    rotation_disabled: bool,
    /// Require actual Vulkan weight/cache allocations, never CPU fallback.
    #[arg(long)]
    require_vulkan: bool,
    /// Trace scheduler placement separately; never compare these timings.
    #[arg(long)]
    audit: bool,
    /// Link a separately recorded quality-evaluation artifact.
    #[arg(long)]
    quality_artifact: Option<String>,
    #[arg(long, hide = true)]
    profile: Option<String>,
    #[arg(long, hide = true)]
    child_context: Option<u32>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Corpus {
    Legacy,
    Representative,
}
fn corpus_text(kind: Corpus) -> anyhow::Result<String> {
    if matches!(kind, Corpus::Legacy) {
        return Ok(CORPUS.to_owned());
    }
    let fixture: Value =
        serde_json::from_str(include_str!("../tests/fixtures/cache-quality-v1.json"))?;
    let mut text = String::new();
    for item in fixture["texts"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("quality corpus texts missing"))?
    {
        text.push_str(
            item["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("quality text missing"))?,
        );
        text.push_str("\n\n");
    }
    Ok(text)
}
fn hash_bytes(bytes: &[u8]) -> anyhow::Result<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("checksum stdin missing"))?
        .write_all(bytes)?;
    let result = child.wait_with_output()?;
    anyhow::ensure!(result.status.success(), "byte checksum failed");
    String::from_utf8(result.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("checksum missing"))
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // Scheduler tracing reads its environment at construction. Relaunch rather
    // than mutating process environment alongside native worker threads.
    if args.audit && std::env::var("GGML_SCHED_DEBUG").as_deref() != Ok("2") {
        let status = Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .env("GGML_SCHED_DEBUG", "2")
            .status()?;
        anyhow::ensure!(status.success(), "placement-audit child failed");
        return Ok(());
    }
    anyhow::ensure!(
        args.repeats > 0 && args.repeats <= 100,
        "repeats must be 1..=100"
    );
    anyhow::ensure!(
        !(args.cpu && args.require_vulkan),
        "CPU and required Vulkan conflict"
    );
    if let Some(profile) = &args.profile {
        return child(&args, profile);
    }
    let executable = std::env::current_exe()?;
    let mut failures = 0;
    #[allow(unused_mut)]
    let mut available = vec![
        "full-f16",
        "compact-f16",
        "full-q8",
        "compact-q8",
        "full-q8-q4",
        "compact-q8-q4",
        "full-q4",
        "compact-q4",
    ];
    #[cfg(feature = "turboquant")]
    available.extend(["full-pq3", "compact-pq3"]);
    anyhow::ensure!(
        !args.include_pq3 || cfg!(feature = "turboquant"),
        "PQ3 benchmark requires turboquant"
    );
    let profiles: Vec<_> = if args.profiles.is_empty() {
        available
            .iter()
            .copied()
            .filter(|p| args.include_pq3 || !p.ends_with("pq3"))
            .collect()
    } else {
        anyhow::ensure!(
            args.profiles
                .iter()
                .all(|p| available.contains(&p.as_str())),
            "unsupported benchmark profile"
        );
        args.profiles.iter().map(String::as_str).collect()
    };
    anyhow::ensure!(
        profiles
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == profiles.len(),
        "duplicate benchmark profiles"
    );
    for context in &args.contexts {
        for profile in &profiles {
            let mut command = Command::new(&executable);
            command
                .args(std::env::args_os().skip(1))
                .args(["--profile", profile, "--child-context"])
                .arg(context.to_string());
            if args.audit {
                command.env("GGML_SCHED_DEBUG", "2");
            }
            let output = command.output()?;
            std::io::stderr().write_all(&output.stderr)?;
            if output.status.success() {
                std::io::stdout().write_all(&output.stdout)?;
            } else {
                failures += 1;
                println!(
                    "{}",
                    json!({"schema":1,"status":"failed","profile":profile,"context":context,"exit_code":output.status.code(),"reason":"child failure; see stderr"})
                );
            }
        }
    }
    anyhow::ensure!(
        failures == 0,
        "{failures} benchmark matrix cells failed; see JSONL and stderr"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep setup and ordered measurement boundaries visible together.
fn child(args: &Args, profile: &str) -> anyhow::Result<()> {
    let context = args
        .child_context
        .or_else(|| args.contexts.first().copied())
        .ok_or_else(|| anyhow::anyhow!("a context capacity is required"))?;
    let mut config = LLMConfig::new(&args.model);
    config.context_size = context;
    config.batch_size = args.batch;
    config.threads = args.threads;
    config.microbatch_size = Some(
        NonZeroU32::new(args.ubatch).ok_or_else(|| anyhow::anyhow!("ubatch must be positive"))?,
    );
    config.max_new_tokens = args.decode_tokens;
    config.cache.swa = if profile.starts_with("compact-") {
        SwaCacheMode::Compact
    } else {
        SwaCacheMode::Full
    };
    (config.cache.key, config.cache.value) = match profile
        .strip_prefix("compact-")
        .or_else(|| profile.strip_prefix("full-"))
    {
        Some("f16") => (KvCacheType::F16, KvCacheType::F16),
        Some("q8") => (KvCacheType::Q8_0, KvCacheType::Q8_0),
        Some("q8-q4") => (KvCacheType::Q8_0, KvCacheType::Q4_0),
        Some("q4") => (KvCacheType::Q4_0, KvCacheType::Q4_0),
        #[cfg(feature = "turboquant")]
        Some("pq3") => (KvCacheType::PQ3_0, KvCacheType::PQ3_0),
        _ => anyhow::bail!("unknown profile {profile}"),
    };
    if args.rotation_disabled {
        config.cache.rotation = AttentionRotation::Disabled;
    }
    if args.cpu {
        config.gpu_layers = 0;
        config.execution = ExecutionMode::Cpu;
    }
    #[cfg(feature = "turboquant")]
    if config.cache.key == KvCacheType::PQ3_0 && args.require_vulkan && !args.cpu {
        config.execution = ExecutionMode::RequireVulkan;
    }
    config.validate()?;
    let identity = identity(&args.model)?;
    ModelEngine::init()?;
    if args.audit {
        // This isolated process owns its logging policy. Use the safe logger;
        // the tensor transaction/decode callback owner is never replaced.
        llama_cpp::logging::set_log_callback(|level, message| {
            if matches!(
                level,
                llama_cpp::LogLevel::Debug | llama_cpp::LogLevel::Continue
            ) {
                eprint!("{message}");
            }
        });
    }
    let engine = ModelEngine::get_instance()?;
    let started = Instant::now();
    let model = engine.load_llm(config)?;
    let load_seconds = started.elapsed().as_secs_f64();
    let topology = model.model_ref().cache_topology()?;
    if args.require_vulkan {
        anyhow::ensure!(
            topology
                .layers
                .iter()
                .any(|l| l.device_name.starts_with("Vulkan")),
            "required Vulkan weight placement unavailable"
        );
    }
    let started = Instant::now();
    let mut ctx = model.new_context()?;
    ctx.synchronize()?;
    let context_seconds = started.elapsed().as_secs_f64();
    let allocated = ctx.cache_diagnostics()?;
    if args.require_vulkan {
        anyhow::ensure!(
            allocated
                .memory
                .iter()
                .any(|b| b.buft_name.starts_with("Vulkan") && b.context > 0),
            "required Vulkan cache allocation unavailable"
        );
    }
    let capacity = ctx.capacity();
    let available = capacity
        .checked_sub(args.decode_tokens)
        .and_then(|v| v.checked_sub(1))
        .ok_or_else(|| anyhow::anyhow!("decode reserve exceeds context"))?;
    let target = args.prompt_tokens.min(available);
    anyhow::ensure!(
        target > 0 && target <= 1_000_000,
        "prompt length must be 1..=1000000"
    );
    let corpus = corpus_text(args.corpus)?;
    anyhow::ensure!(
        corpus.is_ascii(),
        "benchmark corpus must be ASCII for byte-prefix bisection"
    );
    let corpus_sha256 = hash_bytes(corpus.as_bytes())?;
    let text = corpus.repeat(target.div_ceil(8));
    // ASCII allows byte-prefix bisection without splitting UTF-8. Tokenization
    // is setup work, outside timed intervals. All formats use this same input.
    let (mut lo, mut hi) = (1, text.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let count = model
            .model_ref()
            .str_to_token(&text[..mid], llama_cpp::model::AddBos::Always)?
            .len();
        if count <= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let input = ctx.prepare_raw(&text[..lo])?;
    let mut trials = Vec::new();
    for trial in 0..=args.repeats {
        ctx.reset();
        ctx.synchronize()?;
        let started = Instant::now();
        let usage = ctx.prefill(&input)?;
        let prefill_seconds = started.elapsed().as_secs_f64();
        let prefilled = ctx.cache_diagnostics()?;
        let started = Instant::now();
        let mut first_text = None;
        let generation = ctx.generate(&input, GenerationOptions::default(), &mut |_: &str| {
            first_text.get_or_insert_with(|| started.elapsed().as_secs_f64());
            Ok(StreamCmd::Continue)
        })?;
        ctx.synchronize()?;
        let decode_seconds = started.elapsed().as_secs_f64();
        let decoded = ctx.cache_diagnostics()?;
        anyhow::ensure!(
            generation.usage.cached_tokens == input.token_count(),
            "decode must reuse the measured prompt"
        );
        trials.push(json!({"temperature":if trial==0 {"cold"} else {"warm"},"trial":trial,"input_tokens":input.token_count(),"prefill_tokens":usage.prefill_tokens,"generated_tokens":generation.usage.generated_tokens,"finish_reason":format!("{:?}",generation.finish_reason),"prefill_seconds":prefill_seconds,"prefill_tokens_per_second":f64::from(u32::try_from(usage.prefill_tokens)?)/prefill_seconds,"decode_seconds":decode_seconds,"decode_tokens_per_second":f64::from(u32::try_from(generation.usage.generated_tokens)?)/decode_seconds,"time_to_first_text_seconds":first_text.map(|v|v+prefill_seconds),"after_prefill":prefilled.to_json(),"after_decode":decoded.to_json()}));
    }
    println!(
        "{}",
        json!({"schema":1,"status":"ok","profile":profile,"identity":identity,"corpus":format!("{:?}",args.corpus),"corpus_sha256":corpus_sha256,"prompt_text_sha256":hash_bytes(text[..lo].as_bytes())?,"load_seconds":load_seconds,"context_creation_seconds":context_seconds,"allocated":allocated.to_json(),"trials":trials,"devices":topology.layers.iter().map(|l|&l.device_name).collect::<std::collections::BTreeSet<_>>(),"quality_artifact":args.quality_artifact,"placement_audit":if args.audit {"scheduler_trace_on_stderr; inspect assignments"} else {"not_run"},"timing_valid_for_comparison":!args.audit,"host_peak":host_peak(),"device_peak":null})
    );
    Ok(())
}

fn command_text(program: &str, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program).args(arguments).output()?;
    anyhow::ensure!(
        output.status.success(),
        "metadata command failed: {program}"
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
fn hash_file(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("sha256sum").arg("--").arg(path).output()?;
    anyhow::ensure!(output.status.success(), "sha256sum failed");
    let text = String::from_utf8(output.stdout)?;
    let digest = text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing checksum"))?;
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid checksum"
    );
    Ok(digest.to_owned())
}
fn identity(model: &Path) -> anyhow::Result<Value> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root_text = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path is not UTF-8"))?;
    let patch_dir = root.join("crates/llama_cpp_sys/patches");
    let mut paths = std::fs::read_dir(patch_dir)?
        .map(|v| v.map(|v| v.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|p| {
        p.extension().is_some_and(|e| e == "patch")
            && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                matches!(n.get(..4), Some("0003" | "0004" | "0005" | "0006" | "0007"))
                    || cfg!(feature = "turboquant")
                        && matches!(n.get(..4), Some("0008" | "0009" | "0010"))
            })
    });
    paths.sort();
    let patches = paths
        .iter()
        .map(|p| Ok(json!({"name":p.file_name().and_then(|s|s.to_str()),"sha256":hash_file(p)?})))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let diff = Command::new("git")
        .args(["-C", root_text, "diff", "--binary", "HEAD"])
        .output()?;
    anyhow::ensure!(diff.status.success(), "git diff identity failed");
    let mut hasher = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    hasher
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("checksum input unavailable"))?
        .write_all(&diff.stdout)?;
    let diff_hash = hasher.wait_with_output()?;
    anyhow::ensure!(diff_hash.status.success(), "source diff hash failed");
    // Include new files: git diff alone omits untracked implementations.
    let files = Command::new("git")
        .args([
            "-C",
            root_text,
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    anyhow::ensure!(files.status.success(), "source manifest failed");
    let mut source_files = Vec::new();
    for relative in String::from_utf8(files.stdout)?
        .split('\0')
        .filter(|v| !v.is_empty())
    {
        let path = root.join(relative);
        if path.is_file()
            && path.extension().is_some_and(|e| {
                ["rs", "cpp", "h", "hpp", "toml", "lock"]
                    .iter()
                    .any(|v| e == *v)
            })
        {
            source_files.push(json!({"path":relative,"sha256":hash_file(&path)?}));
        }
    }
    let overrides = [
        "LLAMA_ATTN_ROT_DISABLE",
        "GGML_SCHED_DEBUG",
        "GGML_VK_DISABLE_COOPMAT",
        "GGML_VK_DISABLE_COOPMAT2",
        "GGML_VK_DISABLE_COOPMAT2_DECODE_VECTOR",
        "GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM",
        "GGML_VK_DISABLE_F16",
        "GGML_VK_VISIBLE_DEVICES",
        "VK_ICD_FILENAMES",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key, value)))
    .collect::<std::collections::BTreeMap<_, _>>();
    Ok(
        json!({"workspace_revision":command_text("git",&["-C",root_text,"rev-parse","HEAD"])?,"working_diff_sha256":String::from_utf8(diff_hash.stdout)?.split_whitespace().next(),"source_files":source_files,"rustc":command_text("rustc",&["--version"])?,"working_tree_status":command_text("git",&["-C",root_text,"status","--porcelain"])?,"native_revision":command_text("git",&["-C",root_text,"-C","crates/llama_cpp_sys/llama.cpp","rev-parse","HEAD"])?,"patches":patches,"executable_sha256":hash_file(&std::env::current_exe()?)?,"build_rustflags":option_env!("CARGO_ENCODED_RUSTFLAGS"),"model_sha256":hash_file(model)?,"model_bytes":std::fs::metadata(model)?.len(),"corpus":"selected_in_record", "features":{"turboquant":cfg!(feature="turboquant"),"vulkan":cfg!(feature="vulkan")},"build_profile":if cfg!(debug_assertions) {"debug"} else {"release"},"gpu_driver_nvidia_smi":command_text("nvidia-smi",&["--query-gpu=name,driver_version","--format=csv,noheader"]).ok(),"environment":overrides}),
    )
}
fn host_peak() -> Option<Value> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    Some(
        json!({"source":"Linux /proc/self/status VmHWM (host resident bytes)","bytes":status.lines().find_map(|l|l.strip_prefix("VmHWM:"))?.split_whitespace().next()?.parse::<u64>().ok()?.checked_mul(1024)?}),
    )
}
