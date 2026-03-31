use clap::Parser;
use clawedcode_cli::{app::run, cli::Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(Cli::parse()).await
}
