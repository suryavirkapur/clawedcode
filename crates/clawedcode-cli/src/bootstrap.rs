use crate::cli::{Cli, Command};
use anyhow::{Context, Result};
use clawedcode_core::{
    compat,
    config::{AppConfig, default_config_path},
};
use std::{fs, path::PathBuf};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug)]
pub struct BootstrappedApp {
    pub cli: Cli,
    pub config: AppConfig,
    pub compatibility: compat::CompatibilitySnapshot,
    pub mode: ExecutionMode,
}

#[derive(Debug, Clone)]
pub enum ExecutionMode {
    Tui,
    Run(RunMode),
    Resume(ResumeMode),
    Continue(ContinueMode),
    Config,
    Compat,
    Update,
    Headless(HeadlessMode),
    DirectConnect(DirectConnectMode),
    Ssh(SshMode),
    Remote(RemoteMode),
}

#[derive(Debug, Clone)]
pub struct RunMode {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub json: bool,
    pub stream_json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct ResumeMode {
    pub session_id: String,
    pub prompt: Option<String>,
    pub json: bool,
    pub stream_json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct ContinueMode {
    pub prompt: String,
    pub json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct HeadlessMode {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub json: bool,
    pub show_thinking: bool,
    pub yes: bool,
    pub output_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DirectConnectMode {
    pub address: String,
    pub prompt: Option<String>,
    pub system_prompt: Option<String>,
    pub json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct SshMode {
    pub target: String,
    pub prompt: Option<String>,
    pub system_prompt: Option<String>,
    pub json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteMode {
    pub orchestrator: String,
    pub prompt: Option<String>,
    pub system_prompt: Option<String>,
    pub json: bool,
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone, Default)]
struct PromptExecutionPayload {
    prompt: Option<String>,
    system_prompt: Option<String>,
    json: bool,
    show_thinking: bool,
    yes: bool,
    output_path: Option<PathBuf>,
}

pub fn bootstrap(cli: Cli) -> Result<BootstrappedApp> {
    init_tracing();
    let config = AppConfig::load(cli.config.as_deref())?;
    ensure_default_config_exists(cli.config.as_deref())?;
    let compatibility = compat::discover(&cli.cwd)?;
    let mode = resolve_mode(&cli)?;
    Ok(BootstrappedApp {
        cli,
        config,
        compatibility,
        mode,
    })
}

pub fn resolve_mode(cli: &Cli) -> Result<ExecutionMode> {
    let prompt_payload = prompt_payload_from_command(cli.command.as_ref());

    if cli.headless {
        return Ok(ExecutionMode::Headless(HeadlessMode {
            prompt: prompt_payload.prompt.unwrap_or_default(),
            system_prompt: prompt_payload.system_prompt,
            json: prompt_payload.json,
            show_thinking: prompt_payload.show_thinking,
            yes: prompt_payload.yes,
            output_path: prompt_payload.output_path,
        }));
    }
    if let Some(ref remote_addr) = cli.remote {
        return Ok(ExecutionMode::Remote(RemoteMode {
            orchestrator: remote_addr.clone(),
            prompt: prompt_payload.prompt,
            system_prompt: prompt_payload.system_prompt,
            json: prompt_payload.json,
            show_thinking: prompt_payload.show_thinking,
            yes: prompt_payload.yes,
        }));
    }
    if let Some(ref address) = cli.direct_connect {
        return Ok(ExecutionMode::DirectConnect(DirectConnectMode {
            address: address.clone(),
            prompt: prompt_payload.prompt,
            system_prompt: prompt_payload.system_prompt,
            json: prompt_payload.json,
            show_thinking: prompt_payload.show_thinking,
            yes: prompt_payload.yes,
        }));
    }
    if let Some(ref target) = cli.ssh {
        return Ok(ExecutionMode::Ssh(SshMode {
            target: target.clone(),
            prompt: prompt_payload.prompt,
            system_prompt: prompt_payload.system_prompt,
            json: prompt_payload.json,
            show_thinking: prompt_payload.show_thinking,
            yes: prompt_payload.yes,
        }));
    }

    let cmd = cli.command.clone().unwrap_or(Command::Tui);
    match cmd {
        Command::Tui => Ok(ExecutionMode::Tui),
        Command::Run {
            prompt,
            system_prompt,
            json,
            stream_json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Run(RunMode {
            prompt,
            system_prompt,
            json,
            stream_json,
            show_thinking,
            yes,
        })),
        Command::Resume {
            session_id,
            prompt,
            json,
            stream_json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Resume(ResumeMode {
            session_id,
            prompt,
            json,
            stream_json,
            show_thinking,
            yes,
        })),
        Command::Continue {
            prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Continue(ContinueMode {
            prompt,
            json,
            show_thinking,
            yes,
        })),
        Command::Config => Ok(ExecutionMode::Config),
        Command::Compat => Ok(ExecutionMode::Compat),
        Command::Update => Ok(ExecutionMode::Update),
        Command::Headless {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
            output_path,
        } => Ok(ExecutionMode::Headless(HeadlessMode {
            prompt: prompt.unwrap_or_default(),
            system_prompt,
            json,
            show_thinking,
            yes,
            output_path,
        })),
        Command::DirectConnect {
            address,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::DirectConnect(DirectConnectMode {
            address,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        })),
        Command::Ssh {
            target,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Ssh(SshMode {
            target,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        })),
        Command::Remote {
            orchestrator,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Remote(RemoteMode {
            orchestrator,
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        })),
    }
}

fn prompt_payload_from_command(command: Option<&Command>) -> PromptExecutionPayload {
    match command {
        Some(Command::Run {
            prompt,
            system_prompt,
            json,
            stream_json: _,
            show_thinking,
            yes,
        }) => PromptExecutionPayload {
            prompt: Some(prompt.clone()),
            system_prompt: system_prompt.clone(),
            json: *json,
            show_thinking: *show_thinking,
            yes: *yes,
            output_path: None,
        },
        Some(Command::Resume {
            prompt,
            json,
            stream_json: _,
            show_thinking,
            yes,
            ..
        }) => PromptExecutionPayload {
            prompt: prompt.clone(),
            system_prompt: None,
            json: *json,
            show_thinking: *show_thinking,
            yes: *yes,
            output_path: None,
        },
        Some(Command::Headless {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
            output_path,
        }) => PromptExecutionPayload {
            prompt: prompt.clone(),
            system_prompt: system_prompt.clone(),
            json: *json,
            show_thinking: *show_thinking,
            yes: *yes,
            output_path: output_path.clone(),
        },
        Some(Command::DirectConnect {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
            ..
        })
        | Some(Command::Ssh {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
            ..
        })
        | Some(Command::Remote {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
            ..
        }) => PromptExecutionPayload {
            prompt: prompt.clone(),
            system_prompt: system_prompt.clone(),
            json: *json,
            show_thinking: *show_thinking,
            yes: *yes,
            output_path: None,
        },
        _ => PromptExecutionPayload::default(),
    }
}

pub fn init_tracing() {
    let _ = fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("clawedcode=warn".parse().expect("valid directive")),
        )
        .with_target(false)
        .compact()
        .try_init();
}

pub fn ensure_default_config_exists(explicit_path: Option<&std::path::Path>) -> Result<()> {
    let Some(path) = explicit_path
        .map(PathBuf::from)
        .or_else(default_config_path)
    else {
        return Ok(());
    };

    if path.exists() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| anyhow::anyhow!("failed to create {}: {err}", parent.display()))?;
    }
    AppConfig::write_default(&path)
}

pub fn resolve_session_path(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    let base = data_dir
        .or_else(clawedcode_core::config::default_data_dir)
        .context("no data directory configured and no default found")?;
    Ok(base.join("sessions"))
}

pub fn load_session(
    data_dir: Option<PathBuf>,
    session_id: &str,
) -> Result<clawedcode_core::session::Session> {
    let sessions_dir = resolve_session_path(data_dir)?;
    clawedcode_core::session::Session::load_by_id(&sessions_dir, session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;
    use std::path::PathBuf;

    fn make_cli(cmd: Command) -> Cli {
        Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: Some(cmd),
            headless: false,
            remote: None,
            direct_connect: None,
            ssh: None,
        }
    }

    #[test]
    fn test_resolve_mode_tui() {
        let cli = make_cli(Command::Tui);
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Tui));
    }

    #[test]
    fn test_resolve_mode_run() {
        let cli = make_cli(Command::Run {
            prompt: "hello".to_string(),
            system_prompt: None,
            json: false,
            stream_json: false,
            show_thinking: false,
            yes: false,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Run(_)));
        if let ExecutionMode::Run(run) = mode {
            assert_eq!(run.prompt, "hello");
            assert!(!run.json);
            assert!(!run.show_thinking);
            assert!(!run.yes);
        }
    }

    #[test]
    fn test_resolve_mode_run_stream_json() {
        let cli = make_cli(Command::Run {
            prompt: "hello".to_string(),
            system_prompt: None,
            json: true,
            stream_json: true,
            show_thinking: true,
            yes: true,
        });
        let mode = resolve_mode(&cli).unwrap();
        if let ExecutionMode::Run(run) = mode {
            assert!(run.json);
            assert!(run.stream_json);
            assert!(run.show_thinking);
            assert!(run.yes);
        } else {
            panic!("expected run mode");
        }
    }

    #[test]
    fn test_resolve_mode_resume() {
        let cli = make_cli(Command::Resume {
            session_id: "abc-123".to_string(),
            prompt: Some("continue".to_string()),
            json: false,
            stream_json: false,
            show_thinking: false,
            yes: false,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Resume(_)));
        if let ExecutionMode::Resume(resume) = mode {
            assert_eq!(resume.session_id, "abc-123");
            assert_eq!(resume.prompt.as_deref(), Some("continue"));
            assert!(!resume.show_thinking);
            assert!(!resume.yes);
        }
    }

    #[test]
    fn test_resolve_mode_resume_stream_json() {
        let cli = make_cli(Command::Resume {
            session_id: "abc-123".to_string(),
            prompt: Some("continue".to_string()),
            json: true,
            stream_json: true,
            show_thinking: true,
            yes: true,
        });
        let mode = resolve_mode(&cli).unwrap();
        if let ExecutionMode::Resume(resume) = mode {
            assert_eq!(resume.session_id, "abc-123");
            assert_eq!(resume.prompt.as_deref(), Some("continue"));
            assert!(resume.json);
            assert!(resume.stream_json);
            assert!(resume.show_thinking);
            assert!(resume.yes);
        } else {
            panic!("expected resume mode");
        }
    }

    #[test]
    fn test_resolve_mode_continue() {
        let cli = make_cli(Command::Continue {
            prompt: "keep going".to_string(),
            json: true,
            show_thinking: false,
            yes: true,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Continue(_)));
        if let ExecutionMode::Continue(cont) = mode {
            assert_eq!(cont.prompt, "keep going");
            assert!(cont.json);
            assert!(!cont.show_thinking);
            assert!(cont.yes);
        }
    }

    #[test]
    fn test_resolve_mode_config() {
        let cli = make_cli(Command::Config);
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Config));
    }

    #[test]
    fn test_resolve_mode_compat() {
        let cli = make_cli(Command::Compat);
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Compat));
    }

    #[test]
    fn test_resolve_mode_update() {
        let cli = make_cli(Command::Update);
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Update));
    }

    #[test]
    fn test_resolve_mode_headless() {
        let cli = make_cli(Command::Headless {
            prompt: Some("hello".to_string()),
            system_prompt: Some("be helpful".to_string()),
            json: true,
            show_thinking: true,
            yes: true,
            output_path: Some(PathBuf::from("/tmp/out")),
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Headless(_)));
        if let ExecutionMode::Headless(headless) = mode {
            assert_eq!(headless.prompt, "hello");
            assert_eq!(headless.system_prompt.as_deref(), Some("be helpful"));
            assert!(headless.json);
            assert!(headless.show_thinking);
            assert!(headless.yes);
            assert_eq!(
                headless.output_path.as_ref(),
                Some(&PathBuf::from("/tmp/out"))
            );
        }
    }

    #[test]
    fn test_resolve_mode_direct_connect() {
        let cli = make_cli(Command::DirectConnect {
            address: "localhost:8080".to_string(),
            prompt: Some("test".to_string()),
            system_prompt: None,
            json: false,
            show_thinking: true,
            yes: false,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::DirectConnect(_)));
        if let ExecutionMode::DirectConnect(dc) = mode {
            assert_eq!(dc.address, "localhost:8080");
            assert_eq!(dc.prompt.as_deref(), Some("test"));
        }
    }

    #[test]
    fn test_resolve_mode_ssh() {
        let cli = make_cli(Command::Ssh {
            target: "user@host".to_string(),
            prompt: None,
            system_prompt: Some("ssh context".to_string()),
            json: true,
            show_thinking: false,
            yes: true,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Ssh(_)));
        if let ExecutionMode::Ssh(ssh) = mode {
            assert_eq!(ssh.target, "user@host");
            assert_eq!(ssh.system_prompt.as_deref(), Some("ssh context"));
            assert!(ssh.json);
            assert!(!ssh.show_thinking);
            assert!(ssh.yes);
        }
    }

    #[test]
    fn test_resolve_mode_remote() {
        let cli = make_cli(Command::Remote {
            orchestrator: "orchestrator.example.com".to_string(),
            prompt: Some("remote task".to_string()),
            system_prompt: None,
            json: false,
            show_thinking: true,
            yes: false,
        });
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Remote(_)));
        if let ExecutionMode::Remote(remote) = mode {
            assert_eq!(remote.orchestrator, "orchestrator.example.com");
            assert_eq!(remote.prompt.as_deref(), Some("remote task"));
            assert!(remote.show_thinking);
        }
    }

    #[test]
    fn test_resolve_mode_default_is_tui() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: None,
            headless: false,
            remote: None,
            direct_connect: None,
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Tui));
    }

    #[test]
    fn test_resolve_mode_hidden_headless_flag() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: None,
            headless: true,
            remote: None,
            direct_connect: None,
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Headless(_)));
    }

    #[test]
    fn test_resolve_mode_hidden_remote_flag() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: None,
            headless: false,
            remote: Some("remote.example.com".to_string()),
            direct_connect: None,
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Remote(_)));
        if let ExecutionMode::Remote(remote) = mode {
            assert_eq!(remote.orchestrator, "remote.example.com");
        }
    }

    #[test]
    fn test_resolve_mode_hidden_direct_connect_flag() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: None,
            headless: false,
            remote: None,
            direct_connect: Some("localhost:9000".to_string()),
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::DirectConnect(_)));
        if let ExecutionMode::DirectConnect(dc) = mode {
            assert_eq!(dc.address, "localhost:9000");
        }
    }

    #[test]
    fn test_resolve_mode_hidden_ssh_flag() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: None,
            headless: false,
            remote: None,
            direct_connect: None,
            ssh: Some("user@server".to_string()),
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Ssh(_)));
        if let ExecutionMode::Ssh(ssh) = mode {
            assert_eq!(ssh.target, "user@server");
        }
    }

    #[test]
    fn test_resolve_mode_hidden_flags_take_precedence_over_subcommand() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: Some(Command::Run {
                prompt: "hello".to_string(),
                system_prompt: Some("be helpful".to_string()),
                json: true,
                stream_json: false,
                show_thinking: true,
                yes: true,
            }),
            headless: true,
            remote: None,
            direct_connect: None,
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Headless(_)));
        if let ExecutionMode::Headless(headless) = mode {
            assert_eq!(headless.prompt, "hello");
            assert_eq!(headless.system_prompt.as_deref(), Some("be helpful"));
            assert!(headless.json);
            assert!(headless.show_thinking);
            assert!(headless.yes);
        }
    }

    #[test]
    fn test_resolve_mode_hidden_remote_flag_takes_precedence_over_subcommand() {
        let cli = Cli {
            config: None,
            data_dir: None,
            cwd: PathBuf::from("."),
            command: Some(Command::Headless {
                prompt: Some("hello".to_string()),
                system_prompt: None,
                json: false,
                show_thinking: false,
                yes: false,
                output_path: None,
            }),
            headless: false,
            remote: Some("remote.example.com".to_string()),
            direct_connect: None,
            ssh: None,
        };
        let mode = resolve_mode(&cli).unwrap();
        assert!(matches!(mode, ExecutionMode::Remote(_)));
        if let ExecutionMode::Remote(remote) = mode {
            assert_eq!(remote.orchestrator, "remote.example.com");
            assert_eq!(remote.prompt.as_deref(), Some("hello"));
        }
    }
}
