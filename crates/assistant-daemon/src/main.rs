//! The single installed assistant executable. CLI validation precedes startup;
//! the runtime supervises plugins and owns all model/service cleanup.
mod app;
mod cli;
use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    app::run(cli::Args::parse()).await
}
