use anyhow::{Context, Result};
use clawedcode_api::ApiEvent;
use clawedcode_core::{
    compat,
    config::AppConfig,
    config::default_data_dir,
    interactive::{ExternalTurnExecutor, ExternalTurnResult, TuiContext},
    permissions::PermissionMode,
    prompt::{builtin_prompts, resolve_prompt},
    runtime::{ApprovalFn, Runtime, RuntimeOutput},
    session::{Session, SessionMode},
};
use clawedcode_tools::builtin_tools;
use clawedcode_tui as tui;
use serde::Serialize;
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::Arc,
};

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

    if let Some(data_dir) = cli.data_dir.as_ref() {
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", data_dir) };
    }

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
            let tool_names = builtin_tools()
                .iter()
                .map(|item| item.name.clone())
                .collect();
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
        ExecutionMode::Headless(headless_mode) => {
            execute_headless(cli, config, compatibility, headless_mode).await
        }
        ExecutionMode::DirectConnect(direct_connect_mode) => {
            execute_direct_connect(cli, config, compatibility, direct_connect_mode).await
        }
        ExecutionMode::Ssh(ssh_mode) => {
            execute_ssh(cli, config, compatibility, ssh_mode).await
        }
        ExecutionMode::Remote(remote_mode) => {
            execute_remote(cli, config, compatibility, remote_mode).await
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
    let mut session = runtime.start_session_with_mode(cli.cwd, SessionMode::Headless);
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

    session.execution_mode = SessionMode::Resume;

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

    session.execution_mode = SessionMode::Continue;

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

async fn execute_headless(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    headless_mode: crate::bootstrap::HeadlessMode,
) -> Result<()> {
    let crate::bootstrap::HeadlessMode {
        prompt,
        system_prompt,
        json,
        show_thinking,
        yes,
        output_path,
    } = headless_mode;

    let prompt = if prompt.is_empty() {
        eprintln!("No prompt provided for headless mode. Reading from stdin...");
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input.trim().to_string()
    } else {
        prompt
    };

    let data_dir = cli.data_dir.clone();
    let runtime = Runtime::with_mode(
        config,
        resolve_prompt(system_prompt.as_deref()),
        compatibility,
        PermissionMode::Default,
    );
    let mut session = runtime.start_session_with_mode(cli.cwd, SessionMode::Headless);
    let approval_fn = build_approval_fn(yes);

    if json {
        let output = runtime.submit_with_approval(&mut session, &prompt, &*approval_fn);

        if let Some(path) = session_store_dir(data_dir) {
            let _ = session.save(&path);
        }

        let rendered = serde_json::to_string_pretty(&output)?;
        write_headless_output(output_path.as_deref(), &rendered)?;
        println!("{rendered}");
    } else {
        let output = execute_streaming_submit_with_approval(
            &runtime,
            &mut session,
            &prompt,
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

        if let Some(output_path) = output_path.as_ref() {
            std::fs::write(output_path, output.response.as_bytes()).with_context(|| {
                format!("failed to write headless output to {}", output_path.display())
            })?;
        }
    }
    Ok(())
}

fn write_headless_output(path: Option<&std::path::Path>, rendered: &str) -> Result<()> {
    if let Some(path) = path {
        std::fs::write(path, rendered.as_bytes())
            .with_context(|| format!("failed to write headless output to {}", path.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransportOutput {
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportPromptSource {
    Argument,
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransportCommandSpec {
    transport_name: &'static str,
    destination: String,
    program: OsString,
    args: Vec<OsString>,
}

#[derive(Debug, Clone)]
enum InteractiveTransportMode {
    Ssh(crate::bootstrap::SshMode),
    DirectConnect(crate::bootstrap::DirectConnectMode),
    Remote(crate::bootstrap::RemoteMode),
}

#[derive(Debug, Clone)]
struct InteractiveTransportExecutor {
    cwd: PathBuf,
    mode: InteractiveTransportMode,
}

fn ssh_binary() -> OsString {
    std::env::var_os("CLAWEDCODE_SSH_BIN").unwrap_or_else(|| OsString::from("ssh"))
}

fn direct_connect_binary() -> OsString {
    std::env::var_os("CLAWEDCODE_DIRECT_CONNECT_BIN")
        .unwrap_or_else(|| OsString::from("clawedcode-direct-connect"))
}

fn remote_binary() -> OsString {
    std::env::var_os("CLAWEDCODE_REMOTE_BIN")
        .unwrap_or_else(|| OsString::from("clawedcode-remote"))
}

fn resolve_transport_prompt_from_reader<R: Read>(
    transport_name: &'static str,
    prompt: Option<String>,
    reader: &mut R,
    stdin_is_tty: bool,
) -> Result<(String, TransportPromptSource)> {
    match prompt {
        Some(prompt) if !prompt.trim().is_empty() => Ok((prompt, TransportPromptSource::Argument)),
        _ if stdin_is_tty => {
            anyhow::bail!(
                "{transport_name} transport requires a prompt. Pass --prompt or pipe one on stdin."
            );
        }
        _ => {
            let mut input = String::new();
            reader.read_to_string(&mut input)?;
            let prompt = input.trim().to_string();
            if prompt.is_empty() {
                anyhow::bail!(
                    "{transport_name} transport received an empty prompt from stdin."
                );
            }
            Ok((prompt, TransportPromptSource::Stdin))
        }
    }
}

fn resolve_transport_prompt(
    transport_name: &'static str,
    prompt: Option<String>,
) -> Result<(String, TransportPromptSource)> {
    let mut stdin = std::io::stdin();
    resolve_transport_prompt_from_reader(
        transport_name,
        prompt,
        &mut stdin,
        atty::is(atty::Stream::Stdin),
    )
}

fn append_common_headless_args(
    args: &mut Vec<OsString>,
    cwd: &Path,
    prompt: &str,
    system_prompt: Option<&String>,
    json: bool,
    show_thinking: bool,
    yes: bool,
) {
    args.push(OsString::from("--cwd"));
    args.push(cwd.as_os_str().to_os_string());
    args.push(OsString::from("headless"));
    args.push(OsString::from("--prompt"));
    args.push(OsString::from(prompt));

    if let Some(system_prompt) = system_prompt {
        args.push(OsString::from("--system-prompt"));
        args.push(OsString::from(system_prompt));
    }
    if json {
        args.push(OsString::from("--json"));
    }
    if show_thinking {
        args.push(OsString::from("--show-thinking"));
    }
    if yes {
        args.push(OsString::from("-y"));
    }
}

fn build_ssh_command_spec(
    cwd: &Path,
    ssh_mode: &crate::bootstrap::SshMode,
    prompt: &str,
) -> TransportCommandSpec {
    let mut args = vec![
        OsString::from(&ssh_mode.target),
        OsString::from("clawedcode"),
    ];
    append_common_headless_args(
        &mut args,
        cwd,
        prompt,
        ssh_mode.system_prompt.as_ref(),
        ssh_mode.json,
        ssh_mode.show_thinking,
        ssh_mode.yes,
    );

    TransportCommandSpec {
        transport_name: "ssh",
        destination: ssh_mode.target.clone(),
        program: ssh_binary(),
        args,
    }
}

fn build_direct_connect_command_spec(
    cwd: &Path,
    dc_mode: &crate::bootstrap::DirectConnectMode,
    prompt: &str,
) -> TransportCommandSpec {
    let mut args = vec![
        OsString::from(&dc_mode.address),
        OsString::from("clawedcode"),
    ];
    append_common_headless_args(
        &mut args,
        cwd,
        prompt,
        dc_mode.system_prompt.as_ref(),
        dc_mode.json,
        dc_mode.show_thinking,
        dc_mode.yes,
    );

    TransportCommandSpec {
        transport_name: "direct-connect",
        destination: dc_mode.address.clone(),
        program: direct_connect_binary(),
        args,
    }
}

fn build_remote_command_spec(
    cwd: &Path,
    remote_mode: &crate::bootstrap::RemoteMode,
    prompt: &str,
) -> TransportCommandSpec {
    let mut args = vec![
        OsString::from(&remote_mode.orchestrator),
        OsString::from("clawedcode"),
    ];
    append_common_headless_args(
        &mut args,
        cwd,
        prompt,
        remote_mode.system_prompt.as_ref(),
        remote_mode.json,
        remote_mode.show_thinking,
        remote_mode.yes,
    );

    TransportCommandSpec {
        transport_name: "remote",
        destination: remote_mode.orchestrator.clone(),
        program: remote_binary(),
        args,
    }
}

fn append_common_interactive_run_args(
    args: &mut Vec<OsString>,
    cwd: &Path,
    prompt: &str,
    system_prompt: Option<&String>,
) {
    args.push(OsString::from("--cwd"));
    args.push(cwd.as_os_str().to_os_string());
    args.push(OsString::from("run"));
    args.push(OsString::from("--prompt"));
    args.push(OsString::from(prompt));
    if let Some(system_prompt) = system_prompt {
        args.push(OsString::from("--system-prompt"));
        args.push(OsString::from(system_prompt));
    }
    args.push(OsString::from("--json"));
    // Interactive transport turns cannot bridge remote approval requests yet.
    // Force remote auto-approval so the local TUI does not deadlock on an
    // approval prompt it cannot answer over this transport boundary.
    args.push(OsString::from("-y"));
}

fn append_common_interactive_resume_args(
    args: &mut Vec<OsString>,
    cwd: &Path,
    transport_session_id: &str,
    prompt: &str,
) {
    args.push(OsString::from("--cwd"));
    args.push(cwd.as_os_str().to_os_string());
    args.push(OsString::from("resume"));
    args.push(OsString::from(transport_session_id));
    args.push(OsString::from("--prompt"));
    args.push(OsString::from(prompt));
    args.push(OsString::from("--json"));
    args.push(OsString::from("-y"));
}

impl InteractiveTransportExecutor {
    fn new_ssh(cwd: PathBuf, mode: crate::bootstrap::SshMode) -> Self {
        Self {
            cwd,
            mode: InteractiveTransportMode::Ssh(mode),
        }
    }

    fn new_direct_connect(cwd: PathBuf, mode: crate::bootstrap::DirectConnectMode) -> Self {
        Self {
            cwd,
            mode: InteractiveTransportMode::DirectConnect(mode),
        }
    }

    fn new_remote(cwd: PathBuf, mode: crate::bootstrap::RemoteMode) -> Self {
        Self {
            cwd,
            mode: InteractiveTransportMode::Remote(mode),
        }
    }

    fn build_run_spec(&self, prompt: &str) -> TransportCommandSpec {
        match &self.mode {
            InteractiveTransportMode::Ssh(mode) => {
                let mut args = vec![OsString::from(&mode.target), OsString::from("clawedcode")];
                append_common_interactive_run_args(
                    &mut args,
                    &self.cwd,
                    prompt,
                    mode.system_prompt.as_ref(),
                );
                TransportCommandSpec {
                    transport_name: "ssh",
                    destination: mode.target.clone(),
                    program: ssh_binary(),
                    args,
                }
            }
            InteractiveTransportMode::DirectConnect(mode) => {
                let mut args = vec![OsString::from(&mode.address), OsString::from("clawedcode")];
                append_common_interactive_run_args(
                    &mut args,
                    &self.cwd,
                    prompt,
                    mode.system_prompt.as_ref(),
                );
                TransportCommandSpec {
                    transport_name: "direct-connect",
                    destination: mode.address.clone(),
                    program: direct_connect_binary(),
                    args,
                }
            }
            InteractiveTransportMode::Remote(mode) => {
                let mut args =
                    vec![OsString::from(&mode.orchestrator), OsString::from("clawedcode")];
                append_common_interactive_run_args(
                    &mut args,
                    &self.cwd,
                    prompt,
                    mode.system_prompt.as_ref(),
                );
                TransportCommandSpec {
                    transport_name: "remote",
                    destination: mode.orchestrator.clone(),
                    program: remote_binary(),
                    args,
                }
            }
        }
    }

    fn build_resume_spec(&self, transport_session_id: &str, prompt: &str) -> TransportCommandSpec {
        match &self.mode {
            InteractiveTransportMode::Ssh(mode) => {
                let mut args = vec![OsString::from(&mode.target), OsString::from("clawedcode")];
                append_common_interactive_resume_args(
                    &mut args,
                    &self.cwd,
                    transport_session_id,
                    prompt,
                );
                TransportCommandSpec {
                    transport_name: "ssh",
                    destination: mode.target.clone(),
                    program: ssh_binary(),
                    args,
                }
            }
            InteractiveTransportMode::DirectConnect(mode) => {
                let mut args = vec![OsString::from(&mode.address), OsString::from("clawedcode")];
                append_common_interactive_resume_args(
                    &mut args,
                    &self.cwd,
                    transport_session_id,
                    prompt,
                );
                TransportCommandSpec {
                    transport_name: "direct-connect",
                    destination: mode.address.clone(),
                    program: direct_connect_binary(),
                    args,
                }
            }
            InteractiveTransportMode::Remote(mode) => {
                let mut args =
                    vec![OsString::from(&mode.orchestrator), OsString::from("clawedcode")];
                append_common_interactive_resume_args(
                    &mut args,
                    &self.cwd,
                    transport_session_id,
                    prompt,
                );
                TransportCommandSpec {
                    transport_name: "remote",
                    destination: mode.orchestrator.clone(),
                    program: remote_binary(),
                    args,
                }
            }
        }
    }
}

impl ExternalTurnExecutor for InteractiveTransportExecutor {
    fn submit_turn(
        &self,
        session: &mut Session,
        visible_prompt: &str,
        execution_prompt: Option<&str>,
    ) -> Result<ExternalTurnResult> {
        let prompt = execution_prompt.unwrap_or(visible_prompt);
        let spec = if let Some(transport_session_id) = session.transport_session_id.as_deref() {
            self.build_resume_spec(transport_session_id, prompt)
        } else {
            self.build_run_spec(prompt)
        };
        let output = run_transport_command(&spec)?;
        let response: RuntimeOutput = serde_json::from_str(&output.stdout).with_context(|| {
            format!(
                "failed to parse {} interactive transport JSON output",
                spec.transport_name
            )
        })?;
        session.transport_session_id = Some(response.session_id.clone());
        Ok(ExternalTurnResult {
            response: response.response,
            transport_session_id: Some(response.session_id),
        })
    }
}

fn transport_program_display(program: &OsString) -> String {
    PathBuf::from(program).display().to_string()
}

fn transport_launch_banner(
    spec: &TransportCommandSpec,
    cwd: &Path,
    prompt_source: TransportPromptSource,
) -> Vec<String> {
    let mut lines = vec![
        format!(
            "[{}] Launching headless session on `{}` via `{}`",
            spec.transport_name,
            spec.destination,
            transport_program_display(&spec.program)
        ),
        format!("[{}] Working directory: {}", spec.transport_name, cwd.display()),
    ];

    if matches!(prompt_source, TransportPromptSource::Stdin) {
        lines.push(format!(
            "[{}] Prompt source: stdin",
            spec.transport_name
        ));
    }

    lines
}

fn run_transport_command(spec: &TransportCommandSpec) -> Result<TransportOutput> {
    let output = match ProcessCommand::new(&spec.program).args(&spec.args).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "{} transport binary not found via `{}` for `{}`",
                spec.transport_name,
                transport_program_display(&spec.program),
                spec.destination
            );
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to launch {} transport via `{}` for `{}`",
                    spec.transport_name,
                    transport_program_display(&spec.program),
                    spec.destination
                )
            });
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exited with status {}", output.status)
        };
        anyhow::bail!(
            "{} transport via `{}` for `{}` failed: {detail}",
            spec.transport_name,
            transport_program_display(&spec.program),
            spec.destination
        );
    }

    Ok(TransportOutput { stdout, stderr })
}

fn prepare_ssh_headless(
    cwd: &Path,
    ssh_mode: &crate::bootstrap::SshMode,
) -> Result<(TransportCommandSpec, TransportPromptSource)> {
    let (prompt, prompt_source) = resolve_transport_prompt("ssh", ssh_mode.prompt.clone())?;
    let spec = build_ssh_command_spec(cwd, ssh_mode, &prompt);
    Ok((spec, prompt_source))
}

#[cfg_attr(not(test), allow(dead_code))]
fn run_ssh_headless(cwd: &Path, ssh_mode: &crate::bootstrap::SshMode) -> Result<TransportOutput> {
    let (spec, _) = prepare_ssh_headless(cwd, ssh_mode)?;
    run_transport_command(&spec)
}

fn prepare_direct_connect_headless(
    cwd: &Path,
    dc_mode: &crate::bootstrap::DirectConnectMode,
) -> Result<(TransportCommandSpec, TransportPromptSource)> {
    let (prompt, prompt_source) = resolve_transport_prompt("direct-connect", dc_mode.prompt.clone())?;
    let spec = build_direct_connect_command_spec(cwd, dc_mode, &prompt);
    Ok((spec, prompt_source))
}

#[cfg_attr(not(test), allow(dead_code))]
fn run_direct_connect_headless(
    cwd: &Path,
    dc_mode: &crate::bootstrap::DirectConnectMode,
) -> Result<TransportOutput> {
    let (spec, _) = prepare_direct_connect_headless(cwd, dc_mode)?;
    run_transport_command(&spec)
}

fn prepare_remote_headless(
    cwd: &Path,
    remote_mode: &crate::bootstrap::RemoteMode,
) -> Result<(TransportCommandSpec, TransportPromptSource)> {
    let (prompt, prompt_source) = resolve_transport_prompt("remote", remote_mode.prompt.clone())?;
    let spec = build_remote_command_spec(cwd, remote_mode, &prompt);
    Ok((spec, prompt_source))
}

#[cfg_attr(not(test), allow(dead_code))]
fn run_remote_headless(
    cwd: &Path,
    remote_mode: &crate::bootstrap::RemoteMode,
) -> Result<TransportOutput> {
    let (spec, _) = prepare_remote_headless(cwd, remote_mode)?;
    run_transport_command(&spec)
}

fn execute_transport_tui(
    cwd: PathBuf,
    data_dir: Option<PathBuf>,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    system_prompt_override: Option<String>,
    session_mode: SessionMode,
    executor: Arc<dyn ExternalTurnExecutor>,
) -> Result<()> {
    let sessions_dir = session_store_dir(data_dir)
        .context("no sessions directory available")?;
    let ctx = TuiContext::with_external_turn_executor(
        config,
        resolve_prompt(system_prompt_override.as_deref()),
        compatibility,
        cwd,
        sessions_dir,
        session_mode,
        executor,
    );
    tui::run_with_context(ctx)
}

async fn execute_direct_connect(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    direct_connect_mode: crate::bootstrap::DirectConnectMode,
) -> Result<()> {
    if !direct_connect_mode.json {
        return execute_transport_tui(
            cli.cwd.clone(),
            cli.data_dir.clone(),
            config,
            compatibility,
            direct_connect_mode.system_prompt.clone(),
            SessionMode::DirectConnect,
            Arc::new(InteractiveTransportExecutor::new_direct_connect(
                cli.cwd.clone(),
                direct_connect_mode,
            )),
        );
    }

    let (spec, prompt_source) = prepare_direct_connect_headless(&cli.cwd, &direct_connect_mode)?;
    for line in transport_launch_banner(&spec, &cli.cwd, prompt_source) {
        eprintln!("{line}");
    }
    let output = run_transport_command(&spec)?;
    emit_transport_output(&output);
    Ok(())
}

async fn execute_ssh(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    ssh_mode: crate::bootstrap::SshMode,
) -> Result<()> {
    if !ssh_mode.json {
        return execute_transport_tui(
            cli.cwd.clone(),
            cli.data_dir.clone(),
            config,
            compatibility,
            ssh_mode.system_prompt.clone(),
            SessionMode::Ssh,
            Arc::new(InteractiveTransportExecutor::new_ssh(
                cli.cwd.clone(),
                ssh_mode,
            )),
        );
    }

    let (spec, prompt_source) = prepare_ssh_headless(&cli.cwd, &ssh_mode)?;
    for line in transport_launch_banner(&spec, &cli.cwd, prompt_source) {
        eprintln!("{line}");
    }
    let output = run_transport_command(&spec)?;
    emit_transport_output(&output);
    Ok(())
}

async fn execute_remote(
    cli: Cli,
    config: AppConfig,
    compatibility: compat::CompatibilitySnapshot,
    remote_mode: crate::bootstrap::RemoteMode,
) -> Result<()> {
    if !remote_mode.json {
        return execute_transport_tui(
            cli.cwd.clone(),
            cli.data_dir.clone(),
            config,
            compatibility,
            remote_mode.system_prompt.clone(),
            SessionMode::Remote,
            Arc::new(InteractiveTransportExecutor::new_remote(
                cli.cwd.clone(),
                remote_mode,
            )),
        );
    }

    let (spec, prompt_source) = prepare_remote_headless(&cli.cwd, &remote_mode)?;
    for line in transport_launch_banner(&spec, &cli.cwd, prompt_source) {
        eprintln!("{line}");
    }
    let output = run_transport_command(&spec)?;
    emit_transport_output(&output);
    Ok(())
}

fn emit_transport_output(output: &TransportOutput) {
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
    print!("{}", output.stdout);
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
                    sessions.push(session.with_default_task_list_id());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        sync::{Mutex, MutexGuard, OnceLock},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        match LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn find_latest_session_prefers_most_recent_updated_at() {
        let dir = std::env::temp_dir().join(format!(
            "clawed_latest_session_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let older = Session::new(PathBuf::from("/tmp/older"));
        older.save(&dir).unwrap();

        std::thread::sleep(Duration::from_millis(5));

        let newer = Session::new(PathBuf::from("/tmp/newer"));
        newer.save(&dir).unwrap();

        let latest = find_latest_session(&dir).unwrap();
        assert_eq!(latest.id, newer.id);
        assert_eq!(latest.cwd, PathBuf::from("/tmp/newer"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn headless_output_json_mode_produces_valid_serialized_payload() {
        use clawedcode_core::runtime::RuntimeOutput;

        let output = RuntimeOutput {
            session_id: "test-session-123".to_string(),
            system_prompt: "test-system".to_string(),
            response: "hello world".to_string(),
            tool_count: 2,
            skill_count: 0,
            mcp_server_count: 0,
            tools_executed: 1,
        };

        let rendered = serde_json::to_string_pretty(&output).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(reparsed["session_id"], "test-session-123");
        assert_eq!(reparsed["response"], "hello world");
        assert_eq!(reparsed["tool_count"], 2);
        assert_eq!(reparsed["tools_executed"], 1);
        assert!(reparsed.is_object());
    }

    #[test]
    fn resolve_transport_prompt_prefers_explicit_prompt() {
        let mut reader = Cursor::new("ignored stdin");
        let (prompt, source) = resolve_transport_prompt_from_reader(
            "ssh",
            Some("explicit prompt".to_string()),
            &mut reader,
            false,
        )
        .unwrap();
        assert_eq!(prompt, "explicit prompt");
        assert_eq!(source, TransportPromptSource::Argument);
    }

    #[test]
    fn resolve_transport_prompt_reads_from_reader_when_missing() {
        let mut reader = Cursor::new("  streamed prompt  ");
        let (prompt, source) =
            resolve_transport_prompt_from_reader("remote", None, &mut reader, false).unwrap();
        assert_eq!(prompt, "streamed prompt");
        assert_eq!(source, TransportPromptSource::Stdin);
    }

    #[test]
    fn resolve_transport_prompt_errors_on_tty_without_prompt() {
        let mut reader = Cursor::new("");
        let err = resolve_transport_prompt_from_reader("ssh", None, &mut reader, true)
            .expect_err("missing tty prompt should fail");
        assert!(err.to_string().contains("ssh transport requires a prompt"));
    }

    #[test]
    fn resolve_transport_prompt_errors_on_empty_stdin_prompt() {
        let mut reader = Cursor::new("   ");
        let err = resolve_transport_prompt_from_reader("remote", None, &mut reader, false)
            .expect_err("empty stdin prompt should fail");
        assert!(
            err.to_string()
                .contains("remote transport received an empty prompt from stdin")
        );
    }

    #[test]
    fn transport_launch_banner_mentions_transport_destination_and_source() {
        let spec = TransportCommandSpec {
            transport_name: "ssh",
            destination: "devbox".to_string(),
            program: OsString::from("/tmp/fake-ssh"),
            args: vec![],
        };

        let lines = transport_launch_banner(&spec, Path::new("/repo"), TransportPromptSource::Stdin);
        assert_eq!(
            lines,
            vec![
                "[ssh] Launching headless session on `devbox` via `/tmp/fake-ssh`".to_string(),
                "[ssh] Working directory: /repo".to_string(),
                "[ssh] Prompt source: stdin".to_string(),
            ]
        );
    }

    #[test]
    fn build_ssh_command_spec_contains_expected_flags() {
        let _guard = env_lock();
        let ssh_mode = crate::bootstrap::SshMode {
            target: "devbox".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: Some("system".to_string()),
            json: true,
            show_thinking: true,
            yes: true,
        };
        unsafe { std::env::set_var("CLAWEDCODE_SSH_BIN", "/tmp/fake-ssh") };

        let spec = build_ssh_command_spec(Path::new("/repo"), &ssh_mode, "hello");

        assert_eq!(spec.program, OsString::from("/tmp/fake-ssh"));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "devbox",
                "clawedcode",
                "--cwd",
                "/repo",
                "headless",
                "--prompt",
                "hello",
                "--system-prompt",
                "system",
                "--json",
                "--show-thinking",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_SSH_BIN") };
    }

    #[test]
    fn run_ssh_headless_uses_override_binary_and_relays_output() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "clawed_ssh_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake-ssh.sh");
        let capture_path = dir.join("argv.txt");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n: > \"{capture}\"\nfor arg in \"$@\"; do\n  printf '%s\\n' \"$arg\" >> \"{capture}\"\ndone\nprintf '{{\"response\":\"remote ok\"}}'\nprintf 'remote stderr\\n' >&2\n",
                capture = capture_path.display()
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        unsafe { std::env::set_var("CLAWEDCODE_SSH_BIN", &script_path) };
        let ssh_mode = crate::bootstrap::SshMode {
            target: "devbox".to_string(),
            prompt: Some("hello over ssh".to_string()),
            system_prompt: Some("system".to_string()),
            json: true,
            show_thinking: false,
            yes: true,
        };

        let output = run_ssh_headless(Path::new("/workspace"), &ssh_mode).unwrap();
        assert_eq!(output.stdout, "{\"response\":\"remote ok\"}");
        assert_eq!(output.stderr, "remote stderr\n");

        let argv = std::fs::read_to_string(&capture_path).unwrap();
        let args: Vec<&str> = argv.lines().collect();
        assert_eq!(
            args,
            vec![
                "devbox",
                "clawedcode",
                "--cwd",
                "/workspace",
                "headless",
                "--prompt",
                "hello over ssh",
                "--system-prompt",
                "system",
                "--json",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_SSH_BIN") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_direct_connect_command_spec_contains_expected_flags() {
        let _guard = env_lock();
        let dc_mode = crate::bootstrap::DirectConnectMode {
            address: "localhost:8080".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: Some("system".to_string()),
            json: true,
            show_thinking: true,
            yes: true,
        };
        unsafe { std::env::set_var("CLAWEDCODE_DIRECT_CONNECT_BIN", "/tmp/fake-dc") };

        let spec = build_direct_connect_command_spec(Path::new("/repo"), &dc_mode, "hello");

        assert_eq!(spec.program, OsString::from("/tmp/fake-dc"));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "localhost:8080",
                "clawedcode",
                "--cwd",
                "/repo",
                "headless",
                "--prompt",
                "hello",
                "--system-prompt",
                "system",
                "--json",
                "--show-thinking",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_DIRECT_CONNECT_BIN") };
    }

    #[test]
    fn build_remote_command_spec_contains_expected_flags() {
        let _guard = env_lock();
        let remote_mode = crate::bootstrap::RemoteMode {
            orchestrator: "orchestrator.example.com".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: Some("system".to_string()),
            json: true,
            show_thinking: true,
            yes: true,
        };
        unsafe { std::env::set_var("CLAWEDCODE_REMOTE_BIN", "/tmp/fake-remote") };

        let spec = build_remote_command_spec(Path::new("/repo"), &remote_mode, "hello");

        assert_eq!(spec.program, OsString::from("/tmp/fake-remote"));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "orchestrator.example.com",
                "clawedcode",
                "--cwd",
                "/repo",
                "headless",
                "--prompt",
                "hello",
                "--system-prompt",
                "system",
                "--json",
                "--show-thinking",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_REMOTE_BIN") };
    }

    #[test]
    fn run_direct_connect_headless_uses_override_binary_and_relays_output() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "clawed_dc_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake-dc.sh");
        let capture_path = dir.join("argv.txt");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n: > \"{capture}\"\nfor arg in \"$@\"; do\n  printf '%s\\n' \"$arg\" >> \"{capture}\"\ndone\nprintf '{{\"response\":\"dc ok\"}}'\nprintf 'dc stderr\\n' >&2\n",
                capture = capture_path.display()
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        unsafe { std::env::set_var("CLAWEDCODE_DIRECT_CONNECT_BIN", &script_path) };
        let dc_mode = crate::bootstrap::DirectConnectMode {
            address: "localhost:9000".to_string(),
            prompt: Some("hello dc".to_string()),
            system_prompt: Some("dc-system".to_string()),
            json: true,
            show_thinking: false,
            yes: true,
        };

        let output = run_direct_connect_headless(Path::new("/workspace"), &dc_mode).unwrap();
        assert_eq!(output.stdout, "{\"response\":\"dc ok\"}");
        assert_eq!(output.stderr, "dc stderr\n");

        let argv = std::fs::read_to_string(&capture_path).unwrap();
        let args: Vec<&str> = argv.lines().collect();
        assert_eq!(
            args,
            vec![
                "localhost:9000",
                "clawedcode",
                "--cwd",
                "/workspace",
                "headless",
                "--prompt",
                "hello dc",
                "--system-prompt",
                "dc-system",
                "--json",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_DIRECT_CONNECT_BIN") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_remote_headless_uses_override_binary_and_relays_output() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "clawed_remote_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake-remote.sh");
        let capture_path = dir.join("argv.txt");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n: > \"{capture}\"\nfor arg in \"$@\"; do\n  printf '%s\\n' \"$arg\" >> \"{capture}\"\ndone\nprintf '{{\"response\":\"remote ok\"}}'\nprintf 'remote stderr\\n' >&2\n",
                capture = capture_path.display()
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        unsafe { std::env::set_var("CLAWEDCODE_REMOTE_BIN", &script_path) };
        let remote_mode = crate::bootstrap::RemoteMode {
            orchestrator: "my-orchestrator.local".to_string(),
            prompt: Some("hello remote".to_string()),
            system_prompt: Some("remote-system".to_string()),
            json: true,
            show_thinking: false,
            yes: true,
        };

        let output = run_remote_headless(Path::new("/workspace"), &remote_mode).unwrap();
        assert_eq!(output.stdout, "{\"response\":\"remote ok\"}");
        assert_eq!(output.stderr, "remote stderr\n");

        let argv = std::fs::read_to_string(&capture_path).unwrap();
        let args: Vec<&str> = argv.lines().collect();
        assert_eq!(
            args,
            vec![
                "my-orchestrator.local",
                "clawedcode",
                "--cwd",
                "/workspace",
                "headless",
                "--prompt",
                "hello remote",
                "--system-prompt",
                "remote-system",
                "--json",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_REMOTE_BIN") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_ssh_headless_reports_missing_binary() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_SSH_BIN", "/tmp/does-not-exist-ssh") };

        let ssh_mode = crate::bootstrap::SshMode {
            target: "devbox".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: None,
            json: true,
            show_thinking: false,
            yes: true,
        };

        let err = run_ssh_headless(Path::new("/workspace"), &ssh_mode)
            .expect_err("missing ssh binary should fail");
        let message = err.to_string();
        assert!(message.contains("ssh transport binary not found via `/tmp/does-not-exist-ssh`"));
        assert!(message.contains("for `devbox`"));

        unsafe { std::env::remove_var("CLAWEDCODE_SSH_BIN") };
    }

    #[test]
    fn run_direct_connect_headless_reports_missing_binary() {
        let _guard = env_lock();
        unsafe {
            std::env::set_var(
                "CLAWEDCODE_DIRECT_CONNECT_BIN",
                "/tmp/does-not-exist-direct-connect",
            )
        };

        let dc_mode = crate::bootstrap::DirectConnectMode {
            address: "localhost:9000".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: None,
            json: true,
            show_thinking: false,
            yes: true,
        };

        let err = run_direct_connect_headless(Path::new("/workspace"), &dc_mode)
            .expect_err("missing direct-connect binary should fail");
        let message = err.to_string();
        assert!(message.contains(
            "direct-connect transport binary not found via `/tmp/does-not-exist-direct-connect`"
        ));
        assert!(message.contains("for `localhost:9000`"));

        unsafe { std::env::remove_var("CLAWEDCODE_DIRECT_CONNECT_BIN") };
    }

    #[test]
    fn run_remote_headless_reports_missing_binary() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_REMOTE_BIN", "/tmp/does-not-exist-remote") };

        let remote_mode = crate::bootstrap::RemoteMode {
            orchestrator: "my-orchestrator.local".to_string(),
            prompt: Some("hello".to_string()),
            system_prompt: None,
            json: true,
            show_thinking: false,
            yes: true,
        };

        let err = run_remote_headless(Path::new("/workspace"), &remote_mode)
            .expect_err("missing remote binary should fail");
        let message = err.to_string();
        assert!(message.contains("remote transport binary not found via `/tmp/does-not-exist-remote`"));
        assert!(message.contains("for `my-orchestrator.local`"));

        unsafe { std::env::remove_var("CLAWEDCODE_REMOTE_BIN") };
    }

    #[test]
    fn interactive_ssh_executor_uses_run_then_resume() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "clawed_interactive_ssh_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake-ssh.sh");
        let capture_path = dir.join("argv.txt");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{capture}\"\nprintf '{{\"session_id\":\"remote-abc\",\"system_prompt\":\"system\",\"response\":\"remote ok\",\"tool_count\":0,\"skill_count\":0,\"mcp_server_count\":0,\"tools_executed\":0}}'\n",
                capture = capture_path.display()
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        unsafe { std::env::set_var("CLAWEDCODE_SSH_BIN", &script_path) };
        let executor = InteractiveTransportExecutor::new_ssh(
            PathBuf::from("/workspace"),
            crate::bootstrap::SshMode {
                target: "devbox".to_string(),
                prompt: None,
                system_prompt: Some("system".to_string()),
                json: false,
                show_thinking: false,
                yes: false,
            },
        );
        let mut session = Session::with_mode(PathBuf::from("/workspace"), SessionMode::Ssh);

        let first = executor.submit_turn(&mut session, "hello", None).unwrap();
        assert_eq!(first.transport_session_id.as_deref(), Some("remote-abc"));
        session.transport_session_id = first.transport_session_id.clone();

        let second = executor
            .submit_turn(&mut session, "next", Some("resume prompt"))
            .unwrap();
        assert_eq!(second.transport_session_id.as_deref(), Some("remote-abc"));

        let argv = std::fs::read_to_string(&capture_path).unwrap();
        let args: Vec<&str> = argv.lines().collect();
        assert_eq!(
            args,
            vec![
                "devbox",
                "clawedcode",
                "--cwd",
                "/workspace",
                "run",
                "--prompt",
                "hello",
                "--system-prompt",
                "system",
                "--json",
                "-y",
                "devbox",
                "clawedcode",
                "--cwd",
                "/workspace",
                "resume",
                "remote-abc",
                "--prompt",
                "resume prompt",
                "--json",
                "-y",
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_SSH_BIN") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interactive_transport_builders_use_transport_specific_binaries() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_SSH_BIN", "/tmp/fake-ssh") };
        unsafe { std::env::set_var("CLAWEDCODE_DIRECT_CONNECT_BIN", "/tmp/fake-dc") };
        unsafe { std::env::set_var("CLAWEDCODE_REMOTE_BIN", "/tmp/fake-remote") };

        let ssh_spec = InteractiveTransportExecutor::new_ssh(
            PathBuf::from("/workspace"),
            crate::bootstrap::SshMode {
                target: "devbox".to_string(),
                prompt: None,
                system_prompt: None,
                json: false,
                show_thinking: false,
                yes: false,
            },
        )
        .build_run_spec("hello");
        assert_eq!(ssh_spec.program, OsString::from("/tmp/fake-ssh"));
        assert_eq!(ssh_spec.transport_name, "ssh");

        let direct_spec = InteractiveTransportExecutor::new_direct_connect(
            PathBuf::from("/workspace"),
            crate::bootstrap::DirectConnectMode {
                address: "localhost:9999".to_string(),
                prompt: None,
                system_prompt: None,
                json: false,
                show_thinking: false,
                yes: false,
            },
        )
        .build_run_spec("hello");
        assert_eq!(direct_spec.program, OsString::from("/tmp/fake-dc"));
        assert_eq!(direct_spec.transport_name, "direct-connect");

        let remote_spec = InteractiveTransportExecutor::new_remote(
            PathBuf::from("/workspace"),
            crate::bootstrap::RemoteMode {
                orchestrator: "orchestrator.local".to_string(),
                prompt: None,
                system_prompt: None,
                json: false,
                show_thinking: false,
                yes: false,
            },
        )
        .build_run_spec("hello");
        assert_eq!(remote_spec.program, OsString::from("/tmp/fake-remote"));
        assert_eq!(remote_spec.transport_name, "remote");

        unsafe { std::env::remove_var("CLAWEDCODE_SSH_BIN") };
        unsafe { std::env::remove_var("CLAWEDCODE_DIRECT_CONNECT_BIN") };
        unsafe { std::env::remove_var("CLAWEDCODE_REMOTE_BIN") };
    }

    use clawedcode_core::compat::CompatibilitySnapshot;
    use clawedcode_core::session::{Role, SessionMode};

        fn temp_sessions_dir(name: &str) -> PathBuf {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("clawed_resume_{name}_{unique}"));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn empty_compatibility() -> CompatibilitySnapshot {
            CompatibilitySnapshot {
                settings_files: vec![],
                settings: serde_json::Value::Null,
                skills: vec![],
                memory_files: vec![],
                memory: String::new(),
                mcp_servers: std::collections::BTreeMap::new(),
            }
        }

        fn make_cli(data_dir: &PathBuf) -> Cli {
            Cli {
                config: None,
                data_dir: Some(data_dir.clone()),
                cwd: PathBuf::from("/tmp"),
                command: None,
                headless: false,
                remote: None,
                direct_connect: None,
                ssh: None,
            }
        }

        fn run_resume_command(
            data_dir: &PathBuf,
            session_id: &str,
            prompt: Option<&str>,
        ) {
            use std::{
                future::Future,
                pin::pin,
                task::{Context, Poll, Waker},
            };

            let _guard = env_lock();
            unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
            unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };
            unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN") };
            unsafe { std::env::set_var("CLAWEDCODE_PROVIDER", "mock") };

            let mut future = pin!(execute_resume(
                make_cli(data_dir),
                AppConfig::default(),
                empty_compatibility(),
                crate::bootstrap::ResumeMode {
                    session_id: session_id.to_string(),
                    prompt: prompt.map(str::to_string),
                    json: true,
                    show_thinking: false,
                    yes: true,
                },
            ));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);
            match Future::poll(future.as_mut(), &mut cx) {
                Poll::Ready(result) => result.unwrap(),
                Poll::Pending => panic!("json resume path should complete without awaiting"),
            }

            unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };
        }

        #[test]
        fn resume_uses_continue_as_default_prompt() {
            let data_dir = temp_sessions_dir("default_prompt");
            let sessions_dir = data_dir.join("sessions");
            std::fs::create_dir_all(&sessions_dir).unwrap();
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::User, "First message");
            session.push(Role::Assistant, "First response");
            session.save(&sessions_dir).unwrap();

            run_resume_command(&data_dir, &session.id.to_string(), None);

            let loaded = Session::load_by_id(&sessions_dir, &session.id.to_string()).unwrap();
            assert_eq!(loaded.execution_mode, SessionMode::Resume);
            assert_eq!(loaded.last_user_text(), Some("Continue."));
            std::fs::remove_dir_all(&data_dir).ok();
        }

        #[test]
        fn resume_persists_explicit_prompt_and_mode() {
            let data_dir = temp_sessions_dir("explicit_prompt");
            let sessions_dir = data_dir.join("sessions");
            std::fs::create_dir_all(&sessions_dir).unwrap();
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::User, "Original question");
            session.push(Role::Assistant, "Original answer");
            session.save(&sessions_dir).unwrap();

            run_resume_command(&data_dir, &session.id.to_string(), Some("follow up"));

            let loaded = Session::load(&sessions_dir, session.id).unwrap();
            assert_eq!(loaded.execution_mode, SessionMode::Resume);
            assert_eq!(loaded.last_user_text(), Some("follow up"));
            std::fs::remove_dir_all(&data_dir).ok();
        }

        #[test]
        fn continue_chooses_newest_session() {
            let dir = temp_sessions_dir("continue_newest");

            let older = Session::new(PathBuf::from("/tmp/older"));
            older.save(&dir).unwrap();

            std::thread::sleep(Duration::from_millis(10));

            let newer = Session::new(PathBuf::from("/tmp/newer"));
            newer.save(&dir).unwrap();

            let latest = find_latest_session(&dir).unwrap();
            assert_eq!(latest.id, newer.id);
            assert_eq!(latest.cwd, PathBuf::from("/tmp/newer"));

            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn continue_after_resume_still_chooses_newest() {
            let data_dir = temp_sessions_dir("continue_after_resume");
            let sessions_dir = data_dir.join("sessions");
            std::fs::create_dir_all(&sessions_dir).unwrap();

            let first = Session::new(PathBuf::from("/tmp/first"));
            first.save(&sessions_dir).unwrap();

            std::thread::sleep(Duration::from_millis(10));

            let second = Session::new(PathBuf::from("/tmp/second"));
            second.save(&sessions_dir).unwrap();

            std::thread::sleep(Duration::from_millis(10));
            run_resume_command(&data_dir, &first.id.to_string(), Some("Resume message"));

            let latest = find_latest_session(&sessions_dir).unwrap();
            assert_eq!(latest.id, first.id);
            assert_eq!(latest.execution_mode, SessionMode::Resume);
            assert_eq!(latest.last_user_text(), Some("Resume message"));

            std::fs::remove_dir_all(&data_dir).ok();
        }

        #[test]
        fn session_mode_resume_is_distinct_from_continue() {
            let resume_session = Session::with_mode(
                PathBuf::from("/tmp/resume"),
                SessionMode::Resume,
            );
            let continue_session = Session::with_mode(
                PathBuf::from("/tmp/continue"),
                SessionMode::Continue,
            );

            assert_eq!(resume_session.execution_mode, SessionMode::Resume);
            assert_eq!(continue_session.execution_mode, SessionMode::Continue);
            assert_ne!(resume_session.execution_mode, continue_session.execution_mode);
        }

        #[test]
        fn session_save_load_preserves_execution_mode() {
            let dir = temp_sessions_dir("mode_persistence");

            for mode in [
                SessionMode::Interactive,
                SessionMode::Headless,
                SessionMode::Resume,
                SessionMode::Continue,
                SessionMode::DirectConnect,
                SessionMode::Ssh,
                SessionMode::Remote,
            ] {
                let mut session = Session::with_mode(PathBuf::from("/tmp/test"), mode.clone());
                session.push(Role::User, "test");
                session.save(&dir).unwrap();

                let loaded = Session::load(&dir, session.id).unwrap();
                assert_eq!(
                    loaded.execution_mode, mode,
                    "mode {:?} should persist",
                    mode
                );
            }

            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn find_latest_returns_error_on_empty_dir() {
            let dir = temp_sessions_dir("empty_dir");
            std::fs::remove_dir_all(&dir).ok();

            let result = find_latest_session(&dir);
            assert!(result.is_err());
        }

        #[test]
        fn find_latest_ignores_corrupt_session_files() {
            let dir = temp_sessions_dir("corrupt_files");

            let valid_session = Session::new(PathBuf::from("/tmp/valid"));
            valid_session.save(&dir).unwrap();

            std::fs::write(dir.join("corrupt.json"), "not valid json").unwrap();

            let latest = find_latest_session(&dir).unwrap();
            assert_eq!(latest.id, valid_session.id);

            std::fs::remove_dir_all(&dir).ok();
        }
}
