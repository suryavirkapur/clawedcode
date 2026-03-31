use anyhow::{Context, Result};
use clawedcode_core::{
    compat,
    config::AppConfig,
    config::default_data_dir,
    interactive::TuiContext,
    prompt::{builtin_prompts, resolve_prompt},
    runtime::Runtime,
    session::Session,
};
use clawedcode_tools::builtin_tools;
use clawedcode_tui as tui;
use serde::Serialize;
use std::path::PathBuf;

use crate::bootstrap::{BootstrappedApp, ExecutionMode};
use crate::cli::Cli;

#[derive(Debug, Serialize)]
struct ResolvedConfig<'a> {
    config: &'a AppConfig,
    builtin_prompts: Vec<&'a str>,
    builtin_tools: Vec<&'a str>,
    compatibility: compat::CompatibilitySnapshot,
}

pub async fn run(cli: Cli) -> Result<()> {
    let boot = bootstrap(cli)?;
    execute(boot).await
}

pub async fn execute(boot: BootstrappedApp) -> Result<()> {
    let mode = boot.mode;
    let cli = boot.cli;
    let config = boot.config;
    let compatibility = boot.compatibility;

    match mode {
        ExecutionMode::Tui => {
            let sessions_dir = session_store_dir(cli.data_dir.clone())
                .context("no sessions directory available")?;
            let ctx = TuiContext::new(
                config,
                resolve_prompt(None),
                compatibility,
                cli.cwd,
                sessions_dir,
            );
            tui::run_with_context(ctx)
        }
        ExecutionMode::Config => {
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
        ExecutionMode::Compat => {
            println!("{}", serde_json::to_string_pretty(&compatibility)?);
            Ok(())
        }
        ExecutionMode::Run(run_mode) => execute_run(cli, config, compatibility, run_mode).await,
        ExecutionMode::Resume(resume_mode) => {
            execute_resume(cli, config, compatibility, resume_mode).await
        }
        ExecutionMode::Continue(continue_mode) => {
            execute_continue(cli, config, compatibility, continue_mode).await
        }
        ExecutionMode::Headless(_)
        | ExecutionMode::DirectConnect(_)
        | ExecutionMode::Ssh(_)
        | ExecutionMode::Remote(_) => {
            unreachable!("future modes are rejected during mode resolution");
        }
    }
}

async fn execute_run(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    run_mode: crate::bootstrap::RunMode,
) -> Result<()> {
    let data_dir = cli.data_dir.clone();
    let runtime = Runtime::new(
        config,
        resolve_prompt(run_mode.system_prompt.as_deref()),
        compatibility,
    );
    let mut session = runtime.start_session(cli.cwd);
    let output = runtime.submit(&mut session, &run_mode.prompt);

    if let Some(path) = session_store_dir(data_dir) {
        let _ = session.save(&path);
    }

    if run_mode.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", output.response);
    }
    Ok(())
}

async fn execute_resume(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    resume_mode: crate::bootstrap::ResumeMode,
) -> Result<()> {
    let data_dir = cli.data_dir.clone();
    let sessions_dir =
        session_store_dir(data_dir.clone()).context("no sessions directory available")?;
    let mut session = Session::load_by_id(&sessions_dir, &resume_mode.session_id)
        .with_context(|| format!("failed to load session {}", resume_mode.session_id))?;

    let runtime = Runtime::new(config, resolve_prompt(None), compatibility);

    let output = if let Some(prompt) = resume_mode.prompt.clone() {
        runtime.submit(&mut session, &prompt)
    } else {
        runtime.submit(&mut session, "Continue.")
    };

    if let Some(path) = session_store_dir(data_dir) {
        let _ = session.save(&path);
    }

    if resume_mode.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", output.response);
    }
    Ok(())
}

async fn execute_continue(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    continue_mode: crate::bootstrap::ContinueMode,
) -> Result<()> {
    let data_dir = cli.data_dir.clone();
    let sessions_dir =
        session_store_dir(data_dir.clone()).context("no sessions directory available")?;

    let latest_session =
        find_latest_session(&sessions_dir).context("no sessions found to continue")?;

    let mut session = latest_session;

    let runtime = Runtime::new(config, resolve_prompt(None), compatibility);

    let output = runtime.submit(&mut session, &continue_mode.prompt);

    if let Some(path) = session_store_dir(data_dir) {
        let _ = session.save(&path);
    }

    if continue_mode.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", output.response);
    }
    Ok(())
}

fn session_store_dir(explicit_data_dir: Option<PathBuf>) -> Option<PathBuf> {
    explicit_data_dir
        .or_else(default_data_dir)
        .map(|dir| dir.join("sessions"))
}

fn find_latest_session(sessions_dir: &PathBuf) -> Result<Session> {
    let entries = std::fs::read_dir(sessions_dir).with_context(|| {
        format!(
            "failed to read sessions directory {}",
            sessions_dir.display()
        )
    })?;

    let mut sessions: Vec<Session> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str::<Session>(&content) {
                    sessions.push(session);
                }
            }
        }
    }

    sessions
        .into_iter()
        .max_by_key(|s| s.updated_at)
        .context("no valid sessions found")
}

fn bootstrap(cli: Cli) -> Result<BootstrappedApp> {
    crate::bootstrap::bootstrap(cli)
}
