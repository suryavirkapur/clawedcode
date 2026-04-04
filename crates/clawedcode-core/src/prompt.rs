use crate::{compat::CompatibilitySnapshot, session::Session};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub body: &'static str,
}

pub struct PromptRenderContext<'a> {
    pub session: &'a Session,
    pub model: &'a str,
    pub tool_names: &'a [String],
    pub compatibility: &'a CompatibilitySnapshot,
}

pub fn builtin_prompts() -> Vec<PromptSpec> {
    vec![
        PromptSpec {
            name: "core",
            summary: "Default coding assistant behavior",
            body: include_str!("../prompts/core.md"),
        },
        PromptSpec {
            name: "review",
            summary: "Focus on defects, regressions, and missing tests",
            body: include_str!("../prompts/review.md"),
        },
        PromptSpec {
            name: "planning",
            summary: "Bias toward explicit execution plans and checkpoints",
            body: include_str!("../prompts/planning.md"),
        },
    ]
}

pub fn resolve_prompt(name: Option<&str>) -> PromptSpec {
    let wanted = name.unwrap_or("core");
    builtin_prompts()
        .into_iter()
        .find(|prompt| prompt.name == wanted)
        .unwrap_or_else(|| {
            builtin_prompts()
                .into_iter()
                .next()
                .expect("prompt registry is not empty")
        })
}

pub fn render_system_prompt(spec: &PromptSpec, ctx: &PromptRenderContext<'_>) -> String {
    let mut sections = vec![
        section("System", spec.body.trim()),
        doing_tasks_section(),
        actions_with_care_section(),
        using_your_tools_section(ctx.tool_names),
        tone_and_style_section(),
        session_guidance_section(ctx),
        language_section(ctx.compatibility),
        environment_section(ctx.session, ctx.model),
    ];

    if !ctx.compatibility.memory.trim().is_empty() {
        sections.push(format!(
            "## Loaded Memory\n{}",
            ctx.compatibility.memory.trim()
        ));
    }

    if let Some(mcp_section) = mcp_server_section(ctx.compatibility) {
        sections.push(mcp_section);
    }

    sections.join("\n\n")
}

fn doing_tasks_section() -> String {
    [
        "## Doing Tasks",
        "- Build context from the workspace before making strong assumptions when the task depends on repository, filesystem, or runtime state.",
        "- Prefer direct execution over speculative planning when the next step is clear.",
        "- Do not proactively scan the workspace for casual chat, greetings, or simple conversational replies.",
    ]
    .join("\n")
}

fn actions_with_care_section() -> String {
    [
        "## Actions With Care",
        "- Preserve user changes unless explicitly asked to replace them.",
        "- Avoid destructive actions unless the user clearly asked for them.",
        "- Surface blockers, approval needs, and verification status explicitly.",
    ]
    .join("\n")
}

fn using_your_tools_section(tool_names: &[String]) -> String {
    let mut names: Vec<_> = tool_names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let tool_list = if names.is_empty() {
        "No tools are currently available.".to_string()
    } else {
        format!("Available tools in this session: {}.", names.join(", "))
    };

    let mut lines = vec![
        "## Using Your Tools",
        "- Use tools when the request requires file, shell, MCP, session, or verification context.",
        "- Prefer the simplest tool that can complete the next step cleanly.",
        "- When a tool requires approval, wait for the decision instead of assuming permission.",
        &tool_list,
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();

    if names.contains(&"shell") {
        lines.push(
            "- `shell` input must be a JSON object like {\"command\":\"pwd\"}. Use `run_in_background: true` only for long-running commands."
                .to_string(),
        );
    }

    if names.contains(&"read_file") {
        lines.push(
            "- `read_file` input must be a JSON object like {\"path\":\"relative/path.txt\"}."
                .to_string(),
        );
    }

    if names.contains(&"apply_patch") {
        lines.push(
            "- `apply_patch` input must be a JSON object with a `patch` string. The patch string must start with `*** Begin Patch` and end with `*** End Patch`."
                .to_string(),
        );
        lines.push(
            "- For `apply_patch`, prefer the exact grammar: `*** Update File: path`, then `@@` hunks, then changed lines, then `*** End Patch`."
                .to_string(),
        );
    }

    if names.contains(&"TaskOutput") || names.contains(&"TaskStop") {
        lines.push(
            "- Background-task helpers use task IDs returned by earlier tool calls. Use `TaskOutput` to poll and `TaskStop` to interrupt."
                .to_string(),
        );
    }

    lines.join("\n")
}

fn tone_and_style_section() -> String {
    [
        "## Tone And Style",
        "- Keep updates concise, factual, and technically grounded.",
        "- Favor direct answers and actionable execution details over filler.",
        "- Keep final responses tight unless the task requires more detail.",
    ]
    .join("\n")
}

fn session_guidance_section(ctx: &PromptRenderContext<'_>) -> String {
    let mut lines = vec![
        "## Session Guidance".to_string(),
        format!(
            "- Active session mode: {}.",
            session_mode_label(&ctx.session.execution_mode)
        ),
    ];

    if ctx
        .tool_names
        .iter()
        .any(|name| name == "Agent" || name == "Task")
    {
        lines.push(
            "- Sub-agent tools are available for bounded parallel work when delegation helps."
                .to_string(),
        );
    }

    if ctx.tool_names.iter().any(|name| name == "ListMcpResourcesTool") {
        lines.push(
            "- Use MCP helper tools to discover resources before reading them directly."
                .to_string(),
        );
    }

    if !ctx.compatibility.skills.is_empty() {
        let mut commands: Vec<_> = ctx
            .compatibility
            .skills
            .iter()
            .map(|skill| skill.slash_command.as_str())
            .collect();
        commands.sort_unstable();
        lines.push(format!(
            "- Discovered slash commands in this workspace: {}.",
            commands.join(", ")
        ));
    }

    lines.join("\n")
}

fn language_section(compatibility: &CompatibilitySnapshot) -> String {
    let language = compatibility
        .settings
        .get("language")
        .and_then(|value| value.as_str())
        .unwrap_or("default");

    [
        "## Language",
        &format!(
            "- Follow the configured interaction language when present. Current setting: {language}."
        ),
        "- Keep technical terms precise; do not over-translate code, commands, or file paths.",
    ]
    .join("\n")
}

fn environment_section(session: &Session, model: &str) -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    let date = session
        .created_at
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string();
    let cwd = session.cwd.as_path();
    let is_git = cwd
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists());

    [
        "## Environment".to_string(),
        "You have been invoked in the following environment:".to_string(),
        format!("- Primary working directory: {}", cwd.display()),
        format!("- Is a git repository: {}", if is_git { "true" } else { "false" }),
        format!("- Platform: {}", env::consts::OS),
        format!("- Shell: {shell}"),
        format!("- Date: {date}"),
        format!("- Model: {model}"),
    ]
    .join("\n")
}

fn section(title: &str, body: &str) -> String {
    format!("## {title}\n{body}")
}

fn session_mode_label(mode: &crate::session::SessionMode) -> &'static str {
    match mode {
        crate::session::SessionMode::Interactive => "interactive",
        crate::session::SessionMode::Headless => "headless",
        crate::session::SessionMode::Resume => "resume",
        crate::session::SessionMode::Continue => "continue",
        crate::session::SessionMode::DirectConnect => "direct_connect",
        crate::session::SessionMode::Ssh => "ssh",
        crate::session::SessionMode::Remote => "remote",
    }
}

fn mcp_server_section(compatibility: &CompatibilitySnapshot) -> Option<String> {
    if compatibility.mcp_servers.is_empty() {
        return None;
    }

    let mut lines = vec![
        "## MCP Server Instructions".to_string(),
        "The following MCP servers are configured for this session:".to_string(),
    ];

    for (name, config) in &compatibility.mcp_servers {
        let summary = match config {
            clawedcode_mcp::McpServerConfig::Stdio { command, .. } => {
                format!("stdio via `{command}`")
            }
            clawedcode_mcp::McpServerConfig::Http { url, .. } => {
                format!("http at `{url}`")
            }
            clawedcode_mcp::McpServerConfig::Sse { url, .. } => {
                format!("sse at `{url}`")
            }
            clawedcode_mcp::McpServerConfig::Ws { url, .. } => {
                format!("ws at `{url}`")
            }
            clawedcode_mcp::McpServerConfig::Sdk { name: sdk_name, .. } => {
                format!("sdk `{sdk_name}`")
            }
        };
        lines.push(format!("- {name}: {summary}"));
    }

    Some(lines.join("\n"))
}
