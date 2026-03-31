use anyhow::{Context, Result};
use clawedcode_api::ApiEvent;
use clawedcode_core::{
    compat,
    config::AppConfig,
    config::default_data_dir,
    interactive::TuiContext,
    permissions::PermissionMode,
    prompt::{builtin_prompts, resolve_prompt},
    runtime::{ApprovalFn, Runtime},
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
    builtin_tools: Vec<String>,
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
            let tool_names = builtin_tools().iter().map(|item| item.name.clone()).collect();
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
        ExecutionMode::Update => execute_update(),
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

fn execute_update() -> Result<()> {
    let outcome = clawedcode_core::update::run_self_update()?;
    println!(
        "Updated clawedcode via {:?} using `{}`",
        outcome.method, outcome.command
    );
    Ok(())
}

fn build_approval_fn(yes: bool) -> ApprovalFn {
    if yes {
        Box::new(|_, _, _| true)
    } else {
        Box::new(|_tool_use_id, tool_name, input| {
            let input_preview = serde_json::to_string(input).unwrap_or_default();
            eprintln!(
                "\n[approval] Tool '{}' requested. Input: {}",
                tool_name, input_preview
            );
            if atty::is(atty::Stream::Stdin) {
                eprint!("Approve? [y/N] ");
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).is_err() {
                    return false;
                }
                let trimmed = line.trim().to_lowercase();
                trimmed == "y" || trimmed == "yes"
            } else {
                eprintln!("Not a TTY; denying by default. Use --yes to auto-approve.");
                false
            }
        })
    }
}

async fn execute_run(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    run_mode: crate::bootstrap::RunMode,
) -> Result<()> {
    let data_dir = cli.data_dir.clone();
    let runtime = Runtime::with_mode(
        config,
        resolve_prompt(run_mode.system_prompt.as_deref()),
        compatibility,
        PermissionMode::Default,
    );
    let mut session = runtime.start_session(cli.cwd);
    let approval_fn = build_approval_fn(run_mode.yes);

    if run_mode.json {
        let output = runtime.submit_with_approval(&mut session, &run_mode.prompt, &*approval_fn);

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let show_thinking = run_mode.show_thinking;
        let output = execute_streaming_submit_with_approval(
            &runtime,
            &mut session,
            &run_mode.prompt,
            show_thinking,
            &approval_fn,
        )
        .await?;

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!(
            "\n---\ntool_count: {}, tools_executed: {}",
            output.tool_count, output.tools_executed
        );
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

    let runtime = Runtime::with_mode(
        config,
        resolve_prompt(None),
        compatibility,
        PermissionMode::Default,
    );
    let approval_fn = build_approval_fn(resume_mode.yes);

    if resume_mode.json {
        let output = if let Some(prompt) = resume_mode.prompt.clone() {
            runtime.submit_with_approval(&mut session, &prompt, &*approval_fn)
        } else {
            runtime.submit_with_approval(&mut session, "Continue.", &*approval_fn)
        };

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let prompt = resume_mode.prompt.as_deref().unwrap_or("Continue.");
        let show_thinking = resume_mode.show_thinking;
        let output = execute_streaming_submit_with_approval(
            &runtime,
            &mut session,
            prompt,
            show_thinking,
            &approval_fn,
        )
        .await?;

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!(
            "\n---\ntool_count: {}, tools_executed: {}",
            output.tool_count, output.tools_executed
        );
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

    let runtime = Runtime::with_mode(
        config,
        resolve_prompt(None),
        compatibility,
        PermissionMode::Default,
    );
    let approval_fn = build_approval_fn(continue_mode.yes);

    if continue_mode.json {
        let output =
            runtime.submit_with_approval(&mut session, &continue_mode.prompt, &*approval_fn);

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let show_thinking = continue_mode.show_thinking;
        let output = execute_streaming_submit_with_approval(
            &runtime,
            &mut session,
            &continue_mode.prompt,
            show_thinking,
            &approval_fn,
        )
        .await?;

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        println!(
            "\n---\ntool_count: {}, tools_executed: {}",
            output.tool_count, output.tools_executed
        );
    }
    Ok(())
}

async fn execute_streaming_submit_with_approval<A>(
    runtime: &Runtime,
    session: &mut Session,
    prompt: &str,
    show_thinking: bool,
    approval_fn: &A,
) -> Result<clawedcode_core::runtime::StreamingRuntimeOutput>
where
    A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
{
    let output = runtime
        .submit_stream_with_approval(
            session,
            prompt,
            |event| match event {
                ApiEvent::MessageDelta { text } => {
                    print!("{text}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                ApiEvent::ThinkingDelta { text } => {
                    if show_thinking {
                        eprintln!("[thinking] {text}");
                    }
                }
                ApiEvent::ToolUse { tool_use } => {
                    eprintln!("\n[tool] {} {}", tool_use.name, tool_use.id);
                }
                ApiEvent::ToolResult { tool_result } => {
                    let status = if tool_result.is_error { "error" } else { "ok" };
                    eprintln!("[tool_result:{}] {}", status, tool_result.tool_use_id);
                }
                ApiEvent::Usage { usage } => {
                    eprintln!(
                        "[usage] in={} out={} cache_r={} cache_w={}",
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_read_tokens,
                        usage.cache_write_tokens
                    );
                }
                ApiEvent::Completed => {}
            },
            approval_fn,
        )
        .await;

    Ok(output)
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
