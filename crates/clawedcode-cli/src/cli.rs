use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "A fast Rust-native coding agent shell",
    long_about = None
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, global = true, default_value = ".")]
    pub cwd: PathBuf,
    #[command(subcommand)]
    pub command: Option<Command>,
    #[arg(long, global = true, hide = true)]
    pub headless: bool,
    #[arg(long, global = true, hide = true)]
    pub remote: Option<String>,
    #[arg(long, global = true, hide = true)]
    pub direct_connect: Option<String>,
    #[arg(long, global = true, hide = true)]
    pub ssh: Option<String>,
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
    /// Resume a previous session by ID.
    Resume {
        #[arg(help = "Session ID to resume")]
        session_id: String,
        #[arg(short, long)]
        prompt: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Continue the most recent session.
    Continue {
        #[arg(short, long)]
        prompt: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the resolved configuration.
    Config,
    /// Inspect compatibility discovery for config, skills, and MCP servers.
    Compat,
    /// Headless batch execution mode (not yet implemented).
    Headless,
    /// Direct connection to a remote Claude Code instance (not yet implemented).
    DirectConnect,
    /// Connect via SSH to a remote host (not yet implemented).
    Ssh,
    /// Connect to a remote orchestrator (not yet implemented).
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Interactive,
    Headless,
    Resume,
    Continue,
    Config,
    Compat,
    Remote,
    DirectConnect,
    Ssh,
}

impl Cli {
    pub fn classify_intent(&self) -> Intent {
        if self.headless {
            return Intent::Headless;
        }
        if let Some(ref remote_addr) = self.remote {
            if !remote_addr.is_empty() {
                return Intent::Remote;
            }
            return Intent::Remote;
        }
        if self.direct_connect.is_some() {
            return Intent::DirectConnect;
        }
        if self.ssh.is_some() {
            return Intent::Ssh;
        }
        match self.command {
            Some(Command::Tui) => Intent::Interactive,
            Some(Command::Run { .. }) => Intent::Headless,
            Some(Command::Resume { .. }) => Intent::Resume,
            Some(Command::Continue { .. }) => Intent::Continue,
            Some(Command::Config) => Intent::Config,
            Some(Command::Compat) => Intent::Compat,
            Some(Command::Headless) => Intent::Headless,
            Some(Command::DirectConnect) => Intent::DirectConnect,
            Some(Command::Ssh) => Intent::Ssh,
            Some(Command::Remote) => Intent::Remote,
            None => Intent::Interactive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_cli(command: Option<Command>) -> Cli {
        Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command,
            headless: false,
            remote: None,
            direct_connect: None,
            ssh: None,
        }
    }

    fn make_cli_with_flags(
        command: Option<Command>,
        headless: bool,
        remote: Option<String>,
        direct_connect: Option<String>,
        ssh: Option<String>,
    ) -> Cli {
        Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command,
            headless,
            remote,
            direct_connect,
            ssh,
        }
    }

    #[test]
    fn test_classify_intent_tui() {
        let cli = make_cli(Some(Command::Tui));
        assert_eq!(cli.classify_intent(), Intent::Interactive);
    }

    #[test]
    fn test_classify_intent_run() {
        let cli = make_cli(Some(Command::Run {
            prompt: "hello".to_string(),
            system_prompt: None,
            json: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Headless);
    }

    #[test]
    fn test_classify_intent_resume() {
        let cli = make_cli(Some(Command::Resume {
            session_id: "abc".to_string(),
            prompt: None,
            json: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Resume);
    }

    #[test]
    fn test_classify_intent_continue() {
        let cli = make_cli(Some(Command::Continue {
            prompt: "go on".to_string(),
            json: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Continue);
    }

    #[test]
    fn test_classify_intent_config() {
        let cli = make_cli(Some(Command::Config));
        assert_eq!(cli.classify_intent(), Intent::Config);
    }

    #[test]
    fn test_classify_intent_compat() {
        let cli = make_cli(Some(Command::Compat));
        assert_eq!(cli.classify_intent(), Intent::Compat);
    }

    #[test]
    fn test_classify_intent_default_is_interactive() {
        let cli = make_cli(None);
        assert_eq!(cli.classify_intent(), Intent::Interactive);
    }

    #[test]
    fn test_classify_intent_headless_flag() {
        let cli = make_cli_with_flags(None, true, None, None, None);
        assert_eq!(cli.classify_intent(), Intent::Headless);
    }

    #[test]
    fn test_classify_intent_remote_flag() {
        let cli = make_cli_with_flags(None, false, Some("localhost:8080".to_string()), None, None);
        assert_eq!(cli.classify_intent(), Intent::Remote);
    }

    #[test]
    fn test_classify_intent_direct_connect_flag() {
        let cli = make_cli_with_flags(None, false, None, Some("addr".to_string()), None);
        assert_eq!(cli.classify_intent(), Intent::DirectConnect);
    }

    #[test]
    fn test_classify_intent_ssh_flag() {
        let cli = make_cli_with_flags(None, false, None, None, Some("user@host".to_string()));
        assert_eq!(cli.classify_intent(), Intent::Ssh);
    }

    #[test]
    fn test_classify_intent_headless_subcommand() {
        let cli = make_cli(Some(Command::Headless));
        assert_eq!(cli.classify_intent(), Intent::Headless);
    }

    #[test]
    fn test_classify_intent_direct_connect_subcommand() {
        let cli = make_cli(Some(Command::DirectConnect));
        assert_eq!(cli.classify_intent(), Intent::DirectConnect);
    }

    #[test]
    fn test_classify_intent_ssh_subcommand() {
        let cli = make_cli(Some(Command::Ssh));
        assert_eq!(cli.classify_intent(), Intent::Ssh);
    }

    #[test]
    fn test_classify_intent_remote_subcommand() {
        let cli = make_cli(Some(Command::Remote));
        assert_eq!(cli.classify_intent(), Intent::Remote);
    }
}
