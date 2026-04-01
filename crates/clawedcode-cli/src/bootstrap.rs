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
    pub show_thinking: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub struct ResumeMode {
    pub session_id: String,
    pub prompt: Option<String>,
    pub json: bool,
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

#[derive(Debug, Clone, Default)]
pub struct HeadlessMode;

#[derive(Debug, Clone, Default)]
pub struct DirectConnectMode;

#[derive(Debug, Clone, Default)]
pub struct SshMode;

#[derive(Debug, Clone, Default)]
pub struct RemoteMode;

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
    let cmd = cli.command.clone().unwrap_or(Command::Tui);
    match cmd {
        Command::Tui => Ok(ExecutionMode::Tui),
        Command::Run {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Run(RunMode {
            prompt,
            system_prompt,
            json,
            show_thinking,
            yes,
        })),
        Command::Resume {
            session_id,
            prompt,
            json,
            show_thinking,
            yes,
        } => Ok(ExecutionMode::Resume(ResumeMode {
            session_id,
            prompt,
            json,
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
        Command::Headless => Err(anyhow::anyhow!("headless mode is not yet implemented")),
        Command::DirectConnect => Err(anyhow::anyhow!(
            "direct-connect mode is not yet implemented"
        )),
        Command::Ssh => Err(anyhow::anyhow!("ssh mode is not yet implemented")),
        Command::Remote => Err(anyhow::anyhow!("remote mode is not yet implemented")),
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
    fn test_resolve_mode_resume() {
        let cli = make_cli(Command::Resume {
            session_id: "abc-123".to_string(),
            prompt: Some("continue".to_string()),
            json: false,
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
    fn test_resolve_mode_future_returns_error() {
        let cli = make_cli(Command::Headless);
        let err = resolve_mode(&cli).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
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
}
