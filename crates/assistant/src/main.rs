//! Thin manual adapter over the library. One-shot and stdin interactions use the
//! same persistent runtime. Diagnostic content is printed only with `--trace`.
use assistant::{
    AssistantRuntime, RunEvent,
    chat::{InputMode, RunLimits, ToolUse},
    engine::{LLMConfig, ModelEngine, Thinking},
    tools::builtin::{Calculator, CurrentTime},
};
use clap::Parser;
use std::{
    io::{self, Write},
    path::PathBuf,
};
use tokio::io::{AsyncBufReadExt, BufReader};

// Independent CLI flags map to typed library options after parsing.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(about = "Local assistant: one-shot, interactive stdin, or resident daemon")]
struct Args {
    /// Local model weights.
    #[arg(long)]
    model: PathBuf,
    /// Run one interaction; otherwise read lines until EOF.
    #[arg(short, long)]
    prompt: Option<String>,
    /// Keep the runtime resident until Ctrl-C, without stdin input.
    #[arg(long, conflicts_with = "prompt")]
    daemon: bool,
    /// Raw completion for decoder testing, without chat or tools.
    #[arg(long)]
    raw: bool,
    #[arg(long, default_value_t = 1000)]
    gpu_layers: u32,
    #[arg(long, default_value_t = 4096)]
    ctx_size: u32,
    #[arg(long, default_value_t = 512)]
    batch_size: u32,
    #[arg(long, default_value_t = 512)]
    max_tokens: usize,
    #[arg(long, default_value_t = 4)]
    threads: u32,
    #[arg(long)]
    thinking: bool,
    #[arg(long)]
    no_tools: bool,
    #[arg(long, conflicts_with = "no_tools")]
    require_tool: bool,
    /// Wall-clock allowance per interaction, excluding model startup.
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
    /// Print tool requests/results; prompt contents are not logged.
    #[arg(long)]
    trace: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ModelEngine::init()?;
    let engine = ModelEngine::get_instance()?;
    let mut config = LLMConfig::new(&args.model);
    config.gpu_layers = args.gpu_layers;
    config.context_size = args.ctx_size;
    config.batch_size = args.batch_size;
    config.max_new_tokens = args.max_tokens;
    config.threads = args.threads;
    config.thinking = if args.thinking {
        Thinking::Enabled
    } else {
        Thinking::Disabled
    };
    let system = "You are a concise, helpful assistant. Always use calculate for arithmetic and current_time for current time or date. Use actual tool results. When asked for a date command's output, quote its complete local_time value exactly.";
    let mut builder = AssistantRuntime::builder(engine)
        .model("main", config)
        .system(system)
        .limits(RunLimits {
            duration: std::time::Duration::from_secs(args.timeout_seconds),
            ..RunLimits::default()
        });
    if !args.no_tools && !args.raw {
        builder = builder.tool(Calculator).tool(CurrentTime);
    }
    let runtime = builder.build()?.start().await?;
    let chat = runtime.chat();
    let trace = args.trace;
    let ask = |input: String| {
        let request = chat
            .request(input)
            .mode(if args.raw {
                InputMode::Raw
            } else {
                InputMode::Chat
            })
            .tool_use(if args.require_tool {
                ToolUse::RequireFirst
            } else if args.no_tools || args.raw {
                ToolUse::Disabled
            } else {
                ToolUse::Auto
            });
        request.on_event(move |event| {
            if let RunEvent::TextDelta { text, .. } = event {
                print!("{text}");
                io::stdout()
                    .flush()
                    .map_err(|error| assistant::chat::ChatError::Extension {
                        name: "stdout".into(),
                        message: error.to_string(),
                    })?;
            }
            if trace {
                match event {
                    RunEvent::ToolStarted { call, .. } => {
                        eprintln!("[tool request] {} {}", call.name, call.arguments);
                    }
                    RunEvent::ToolFinished { receipt, .. } => {
                        eprintln!("[tool result]  {} {}", receipt.call.name, receipt.result);
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    };
    let result: anyhow::Result<()> = async {
        if args.daemon {
            tokio::signal::ctrl_c().await?;
        } else if let Some(prompt) = &args.prompt {
            let response = ask(prompt.clone()).run().await?;
            println!();
            eprintln!("Prompt: {} tokens", response.usage.input_tokens);
            eprintln!("Generated {} tokens", response.usage.generated_tokens);
        } else {
            eprintln!("Enter messages; EOF ends the runtime.");
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Some(line) = lines.next_line().await? {
                if line.trim().is_empty() {
                    continue;
                }
                let response = ask(line).run().await?;
                println!();
                eprintln!("Generated {} tokens", response.usage.generated_tokens);
                if args.raw {
                    chat.clear_working_history().await;
                }
            }
        }
        Ok(())
    }
    .await;
    let stopped = runtime.shutdown().await;
    match (result, stopped) {
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!("{error}; shutdown: {cleanup}")),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}
