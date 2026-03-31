use crate::{
    cli::{Cli, Command},
    compat,
    config::{AppConfig, default_config_path, default_data_dir},
    prompt::{builtin_prompts, resolve_prompt},
    runtime::Runtime,
    tool::builtin_tools,
    tui,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{fs, path::PathBuf};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Serialize)]
struct ResolvedConfig<'a> {
    config: &'a AppConfig,
    builtin_prompts: Vec<&'a str>,
    builtin_tools: Vec<&'a str>,
    compatibility: compat::CompatibilitySnapshot,
}

pub async fn run(cli: Cli) -> Result<()> {
    init_tracing();

    let config = AppConfig::load(cli.config.as_deref())?;
    ensure_default_config_exists(cli.config.as_deref())?;
    let compatibility = compat::discover(&cli.cwd)?;

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(),
        Command::Config => {
            let prompt_names = builtin_prompts().iter().map(|item| item.name).collect();
            let tool_names = builtin_tools().iter().map(|item| item.name).collect();
            let payload = ResolvedConfig {
                config: &config,
                builtin_prompts: prompt_names,
                builtin_tools: tool_names,
                compatibility,
            };
            println!("{}", serde_json::to_string_pretty(&payload)?);
            Ok(())
        }
        Command::Compat => {
            println!("{}", serde_json::to_string_pretty(&compatibility)?);
            Ok(())
        }
        Command::Run {
            prompt,
            system_prompt,
            json,
        } => {
            let runtime = Runtime::new(
                config,
                resolve_prompt(system_prompt.as_deref()),
                compatibility,
            );
            let mut session = runtime.start_session(cli.cwd);
            let output = runtime.submit(&mut session, &prompt);

            if let Some(path) = session_store_dir(cli.data_dir) {
                let _ = session.save(&path);
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("{}", output.response);
            }
            Ok(())
        }
    }
}

fn init_tracing() {
    let _ = fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("clawedcode=info".parse().expect("valid directive")),
        )
        .with_target(false)
        .compact()
        .try_init();
}

fn ensure_default_config_exists(explicit_path: Option<&std::path::Path>) -> Result<()> {
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
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    AppConfig::write_default(&path)
}

fn session_store_dir(explicit_data_dir: Option<PathBuf>) -> Option<PathBuf> {
    explicit_data_dir
        .or_else(default_data_dir)
        .map(|dir| dir.join("sessions"))
}
