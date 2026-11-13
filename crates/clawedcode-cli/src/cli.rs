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
    /// Start the session inside a new worktree (git, or WorktreeCreate hooks).
    #[arg(long, global = true)]
    pub worktree: Option<String>,
    /// Run the main session as a named agent (from .claude/agents or built-ins).
    #[arg(long, global = true)]
    pub agent: Option<String>,
    /// With `resume`, fork the session instead of resuming it in place.
    #[arg(long, global = true)]
    pub fork_session: bool,
    /// Additional directories to grant tool access to (repeatable).
    #[arg(long, global = true)]
    pub add_dir: Vec<PathBuf>,
    /// Permission mode: default, acceptEdits, plan, or bypassPermissions.
    #[arg(long, global = true)]
    pub permission_mode: Option<String>,
    /// Override the configured model for this run.
    #[arg(long, global = true)]
    pub model: Option<String>,
    /// Maximum agentic turns before stopping.
    #[arg(long, global = true)]
    pub max_turns: Option<u16>,
    /// Extra text appended to the system prompt.
    #[arg(long, global = true)]
    pub append_system_prompt: Option<String>,
    /// Restrict the session to these tool names (comma-separated).
    #[arg(long, global = true, value_delimiter = ',')]
    pub allowed_tools: Vec<String>,
    /// Remove these tool names from the session (comma-separated).
    #[arg(long, global = true, value_delimiter = ',')]
    pub disallowed_tools: Vec<String>,
    /// Use a specific session id for the new session.
    #[arg(long, global = true)]
    pub session_id: Option<String>,
    /// Print the fully rendered system prompt and exit.
    #[arg(long, global = true)]
    pub dump_system_prompt: bool,
    /// Skip all permission prompts (equivalent to --permission-mode bypassPermissions).
    #[arg(long, global = true)]
    pub dangerously_skip_permissions: bool,
    /// Output format for headless runs: text, json, or stream-json.
    #[arg(long, global = true)]
    pub output_format: Option<String>,
    /// Input format for headless runs: text or stream-json.
    #[arg(long, global = true)]
    pub input_format: Option<String>,
    /// Extra MCP server config file(s) to merge (colon-separated).
    #[arg(long, global = true)]
    pub mcp_config: Option<String>,
    /// Use only --mcp-config servers, ignoring project/user sources.
    #[arg(long, global = true)]
    pub strict_mcp_config: bool,
    /// Extra settings file merged after .claude/settings*.json.
    #[arg(long, global = true)]
    pub settings: Option<PathBuf>,
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
        #[arg(long, hide = true)]
        stream_json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Resume a previous session by ID.
    Resume {
        #[arg(help = "Session ID to resume")]
        session_id: String,
        #[arg(short, long)]
        prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long, hide = true)]
        stream_json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Continue the most recent session.
    Continue {
        #[arg(short, long)]
        prompt: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print the resolved configuration.
    Config,
    /// Inspect compatibility discovery for config, skills, and MCP servers.
    Compat,
    /// Update clawedcode using the package manager that installed it.
    Update,
    /// Headless batch execution mode (reads from stdin if no --prompt).
    Headless {
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        output_path: Option<PathBuf>,
    },
    /// Direct connection to a remote Claude Code instance.
    DirectConnect {
        #[arg(long)]
        address: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Connect via SSH to a remote host.
    Ssh {
        #[arg(long)]
        target: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Manage MCP server configuration (.mcp.json).
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Connect to a remote orchestrator.
    Remote {
        #[arg(long)]
        orchestrator: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        show_thinking: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum McpAction {
    /// List configured MCP servers.
    List,
    /// Show one server's configuration.
    Get { name: String },
    /// Add a stdio MCP server to .mcp.json.
    Add {
        name: String,
        command: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Write to the user-level ~/.claude.json-style scope instead.
        #[arg(long)]
        user: bool,
    },
    /// Remove a server from .mcp.json.
    Remove { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Interactive,
    Headless,
    Resume,
    Continue,
    Config,
    Compat,
    Update,
    Remote,
    DirectConnect,
    Ssh,
    Mcp,
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
            Some(Command::Update) => Intent::Update,
            Some(Command::Headless { .. }) => Intent::Headless,
            Some(Command::DirectConnect { .. }) => Intent::DirectConnect,
            Some(Command::Ssh { .. }) => Intent::Ssh,
            Some(Command::Remote { .. }) => Intent::Remote,
            Some(Command::Mcp { .. }) => Intent::Mcp,
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
            worktree: None,
            agent: None,
            fork_session: false,
            add_dir: Vec::new(),
            permission_mode: None,
            model: None,
            max_turns: None,
            append_system_prompt: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            session_id: None,
            dump_system_prompt: false,
            dangerously_skip_permissions: false,
            output_format: None,
            input_format: None,
            mcp_config: None,
            strict_mcp_config: false,
            settings: None,
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
            worktree: None,
            agent: None,
            fork_session: false,
            add_dir: Vec::new(),
            permission_mode: None,
            model: None,
            max_turns: None,
            append_system_prompt: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            session_id: None,
            dump_system_prompt: false,
            dangerously_skip_permissions: false,
            output_format: None,
            input_format: None,
            mcp_config: None,
            strict_mcp_config: false,
            settings: None,
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
            stream_json: false,
            show_thinking: false,
            yes: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Headless);
    }

    #[test]
    fn test_classify_intent_resume() {
        let cli = make_cli(Some(Command::Resume {
            session_id: "abc".to_string(),
            prompt: None,
            json: false,
            stream_json: false,
            show_thinking: false,
            yes: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Resume);
    }

    #[test]
    fn test_classify_intent_continue() {
        let cli = make_cli(Some(Command::Continue {
            prompt: "go on".to_string(),
            json: false,
            show_thinking: false,
            yes: false,
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
    fn test_classify_intent_update() {
        let cli = make_cli(Some(Command::Update));
        assert_eq!(cli.classify_intent(), Intent::Update);
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
        let cli = make_cli(Some(Command::Headless {
            prompt: None,
            system_prompt: None,
            json: false,
            show_thinking: false,
            yes: false,
            output_path: None,
        }));
        assert_eq!(cli.classify_intent(), Intent::Headless);
    }

    #[test]
    fn test_classify_intent_direct_connect_subcommand() {
        let cli = make_cli(Some(Command::DirectConnect {
            address: "localhost:8080".to_string(),
            prompt: None,
            system_prompt: None,
            json: false,
            show_thinking: false,
            yes: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::DirectConnect);
    }

    #[test]
    fn test_classify_intent_ssh_subcommand() {
        let cli = make_cli(Some(Command::Ssh {
            target: "user@host".to_string(),
            prompt: None,
            system_prompt: None,
            json: false,
            show_thinking: false,
            yes: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Ssh);
    }

    #[test]
    fn test_classify_intent_remote_subcommand() {
        let cli = make_cli(Some(Command::Remote {
            orchestrator: "orchestrator.example.com".to_string(),
            prompt: None,
            system_prompt: None,
            json: false,
            show_thinking: false,
            yes: false,
        }));
        assert_eq!(cli.classify_intent(), Intent::Remote);
    }
}
