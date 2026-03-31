use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, version, about = "A fast Rust-native coding agent shell")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, global = true, default_value = ".")]
    pub cwd: PathBuf,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Launch the interactive terminal UI.
    Tui,
    /// Run a single prompt through the local runtime.
    Run {
        #[arg(short, long)]
        prompt: String,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print the resolved configuration.
    Config,
    /// Inspect compatibility discovery for config, skills, and MCP servers.
    Compat,
}
