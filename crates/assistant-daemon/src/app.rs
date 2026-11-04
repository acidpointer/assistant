//! Process composition and signal handling. This module initializes one engine,
//! installs the default web plugin and always awaits runtime cleanup. Explicit
//! one-shot/stdin adapters remain diagnostic modes and never start HTTP.
use crate::cli::{Args, model_config};
use assistant::{
    AssistantRuntime, RunEvent,
    chat::{InputMode, RunLimits, ToolUse},
    engine::{Cancellation, ModelEngine},
    tools::builtin::{Calculator, CurrentTime},
};
use std::io::{self, Write};
use tokio::io::{AsyncBufReadExt, BufReader};

pub(crate) async fn run(args: Args) -> anyhow::Result<()> {
    let config = model_config(&args)?;
    if args.raw && args.prompt.is_none() && !args.interactive {
        anyhow::bail!("--raw requires --prompt or --interactive (and --no-web)");
    }
    // Install both handlers before model startup. The task remains alive through
    // initialization so a termination request cannot abandon startup cleanup.
    let signal = shutdown_signal()?;
    ModelEngine::init()?;
    let engine = ModelEngine::get_instance()?;
    let system = "You are a concise, helpful assistant. Always use calculate for arithmetic and current_time for current time or date. Use actual tool results. When asked for a date command's output, quote its complete local_time value exactly.";
    let mut builder = AssistantRuntime::builder(engine)
        .model("main", config)
        .system(system)
        .limits(RunLimits {
            generated_tokens: args.run_max_tokens,
            duration: std::time::Duration::from_secs(args.timeout_seconds),
            ..RunLimits::default()
        });
    if !args.no_tools && !args.raw {
        builder = builder.tool(Calculator).tool(CurrentTime);
    }
    if !args.no_web {
        builder = builder.plugin(assistant_web::WebPlugin::new(assistant_web::WebConfig {
            host: args.host,
            port: args.port,
        }));
    }
    let runtime = builder.build()?.start().await?;
    let chat = runtime.chat();
    let result: anyhow::Result<()> = async {
        if args.prompt.is_none() && !args.interactive {
            tokio::select! {
                result = signal.cancelled() => { let () = result; }
                result = runtime.wait_for_failure() => { result?; }
            }
        } else if let Some(prompt) = &args.prompt {
            let response = tokio::select! {
                () = signal.cancelled() => return Ok(()),
                result = request(&chat, &args, prompt.clone()).run() => result?,
            };
            println!();
            eprintln!("Prompt: {} tokens", response.usage.input_tokens);
            eprintln!("Generated {} tokens", response.usage.generated_tokens);
            if args.diagnostics {
                print_diagnostics(&runtime).await?;
            }
        } else {
            eprintln!("Enter messages; EOF ends the runtime.");
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            loop {
                let line = tokio::select! {
                    () = signal.cancelled() => break,
                    line = lines.next_line() => line?,
                };
                let Some(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let response = tokio::select! {
                    () = signal.cancelled() => break,
                    result = request(&chat, &args, line).run() => result?,
                };
                println!();
                eprintln!("Generated {} tokens", response.usage.generated_tokens);
                if args.diagnostics {
                    print_diagnostics(&runtime).await?;
                }
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

async fn print_diagnostics(runtime: &assistant::runtime::RunningAssistant) -> anyhow::Result<()> {
    let reports = runtime.cache_diagnostics().await?;
    let reports: std::collections::BTreeMap<_, _> = reports
        .into_iter()
        .map(|(name, report)| (name, report.to_json()))
        .collect();
    eprintln!("{}", serde_json::to_string(&reports)?);
    Ok(())
}

/// Register termination before startup; the signal task owns no model resources.
fn shutdown_signal() -> anyhow::Result<Cancellation> {
    let cancellation = Cancellation::default();
    let notify = cancellation.clone();
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        #[cfg(not(unix))]
        if tokio::signal::ctrl_c().await.is_err() {
            eprintln!("shutdown signal handler failed");
        }
        notify.cancel();
    });
    Ok(cancellation)
}

/// CLI-only streaming adapter; this path is unavailable while `WebUI` is enabled.
fn request(chat: &assistant::Chat, args: &Args, input: String) -> assistant::chat::Request {
    let trace = args.trace;
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
}
