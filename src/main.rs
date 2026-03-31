use clap::Parser;
use clawedcode::{app::run, cli::Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run(cli).await
}
