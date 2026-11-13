//! User-configured lifecycle hooks, matching Claude Code's settings format:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       { "matcher": "Bash|apply_patch", "hooks": [{"type": "command", "command": "..."}] }
//!     ]
//!   },
//!   "disableAllHooks": false
//! }
//! ```
//!
//! Hook commands receive a JSON payload on stdin
//! (`hook_event_name`, `tool_name`, `tool_input`, `session_id`, `cwd`).
//! Exit code 0 allows the event; exit code 2 blocks it and the hook's stderr
//! becomes the block reason. Any other non-zero exit is a non-blocking
//! warning whose stderr is surfaced to the caller.

use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct HookCommand {
    /// Lifecycle event, e.g. `PreToolUse` or `PostToolUse`.
    pub event: String,
    /// Regex matched against the tool name. `None` matches everything.
    pub matcher: Option<String>,
    pub command: String,
    /// Per-hook timeout override in milliseconds (settings `timeout` is
    /// seconds, like the reference).
    pub timeout_ms: Option<u64>,
    /// Settings file the hook came from (for display in `/hooks`).
    pub source: std::path::PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct HooksConfig {
    pub disabled: bool,
    pub hooks: Vec<HookCommand>,
}

#[derive(Debug, Deserialize)]
struct HookGroup {
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    hooks: Vec<HookSpec>,
}

#[derive(Debug, Deserialize)]
struct HookSpec {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    command: Option<String>,
    /// Timeout in seconds, like the reference's hook timeout field.
    timeout: Option<u64>,
}

/// Load hooks from `.claude/settings.json` and `.claude/settings.local.json`
/// in `cwd` (the local file wins `disableAllHooks` like the reference's
/// settings precedence).
/// Settings files in precedence order, honoring `CLAWEDCODE_SETTING_SOURCES`
/// and `CLAWEDCODE_SETTINGS` — shared by hook and permission-rule loading.
pub(crate) fn settings_file_paths(cwd: &Path) -> Vec<PathBuf> {
    // `--settings <file>` / CLAWEDCODE_SETTINGS: extra settings file merged
    // last like the reference flag.
    let extra = std::env::var_os("CLAWEDCODE_SETTINGS").map(PathBuf::from);
    // `--setting-sources <list>` (CLAWEDCODE_SETTING_SOURCES): only load the
    // listed sources — "project" is .claude/settings.json, "local" is
    // settings.local.json, "user" is ~/.claude/settings.json.
    let sources: Option<Vec<String>> = std::env::var("CLAWEDCODE_SETTING_SOURCES")
        .ok()
        .map(|raw| raw.split(',').map(|s| s.trim().to_string()).collect());
    let wants = |source: &str| {
        sources
            .as_ref()
            .map(|s| s.iter().any(|x| x == source))
            .unwrap_or(true)
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    if wants("user") {
        if let Some(home) = crate::compat::claude_home() {
            paths.push(home.join("settings.json"));
        }
    }
    if wants("project") {
        paths.push(cwd.join(".claude").join("settings.json"));
    }
    if wants("local") {
        paths.push(cwd.join(".claude").join("settings.local.json"));
    }
    paths.extend(extra);
    paths
}

pub fn load_hooks(cwd: &Path) -> HooksConfig {
    let mut config = HooksConfig::default();
    // `--bare`: minimal mode skips hooks like the reference.
    if std::env::var_os("CLAWEDCODE_SIMPLE").is_some() {
        config.disabled = true;
        return config;
    }
    for path in settings_file_paths(cwd) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if json
            .get("disableAllHooks")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            config.disabled = true;
        }
        let Some(hooks_obj) = json.get("hooks").and_then(|v| v.as_object()) else {
            continue;
        };
        for (event, groups) in hooks_obj {
            let Ok(groups) = serde_json::from_value::<Vec<HookGroup>>(groups.clone()) else {
                continue;
            };
            for group in groups {
                for spec in group.hooks {
                    // Only `command` hooks exist today; the reference also
                    // defines llm/prompt hooks which we do not emulate.
                    if spec.kind.as_deref().unwrap_or("command") != "command" {
                        continue;
                    }
                    let Some(command) = spec.command.filter(|c| !c.trim().is_empty()) else {
                        continue;
                    };
                    config.hooks.push(HookCommand {
                        event: event.clone(),
                        matcher: group.matcher.clone(),
                        command,
                        timeout_ms: spec.timeout.map(|secs| secs * 1000),
                        source: path.clone(),
                    });
                }
            }
        }
    }
    config
}

/// Hooks configured for `event`, in file order.
pub fn hooks_for_event<'a>(config: &'a HooksConfig, event: &str) -> Vec<&'a HookCommand> {
    config
        .hooks
        .iter()
        .filter(|hook| hook.event == event)
        .collect()
}

fn matcher_matches(matcher: Option<&str>, tool_name: &str) -> bool {
    match matcher.map(str::trim) {
        None | Some("") | Some("*") => true,
        Some(pattern) => regex_lite_match(pattern, tool_name),
    }
}

/// Small regex subset good enough for tool matchers (`A|B`, `.*`, `^`, `$`,
/// char classes are uncommon here). Falls back to substring match when the
/// pattern is not valid regex — the `regex` crate is not a core dependency.
fn regex_lite_match(pattern: &str, tool_name: &str) -> bool {
    // Treat `|` as alternation of full-pattern checks.
    for alt in pattern.split('|') {
        let alt = alt.trim();
        if alt.is_empty() {
            continue;
        }
        if simple_pattern_match(alt, tool_name) {
            return true;
        }
    }
    false
}

/// `*` (any run) and `?` (single char) wildcards plus `^`/`$` anchors;
/// everything else is literal. This covers the matchers people actually
/// write (`Bash`, `Edit|Write`, `mcp__.*`, `WebFetch`).
fn simple_pattern_match(pattern: &str, text: &str) -> bool {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$');
    let pattern = pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        // `.*`/`x*` collapse to prefix/wildcard semantics users expect.
        .replace(".*", "*");

    let mut segs: Vec<&str> = pattern.split('*').collect();
    let leading_wild = pattern.starts_with('*');
    let trailing_wild = pattern.ends_with('*');

    // Fast path: no wildcards → exact/contains depending on anchors.
    if segs.len() == 1 {
        let seg = segs[0];
        if anchored_start && anchored_end {
            return text == seg;
        }
        if anchored_start {
            return text.starts_with(seg);
        }
        if anchored_end {
            return text.ends_with(seg);
        }
        return text == seg || text.contains(seg);
    }

    let mut pos = 0usize;
    if !leading_wild || anchored_start {
        let first = segs.remove(0);
        if !text[pos..].starts_with(first) {
            return false;
        }
        pos += first.len();
    }
    let last = if !trailing_wild || anchored_end {
        segs.pop()
    } else {
        None
    };
    for seg in segs {
        match text[pos..].find(seg) {
            Some(idx) => pos += idx + seg.len(),
            None => return false,
        }
    }
    if let Some(seg) = last {
        return text[pos..].ends_with(seg);
    }
    true
}

#[derive(Debug)]
pub enum PreHookOutcome {
    /// No hook blocked the tool call; normal permission flow applies.
    Allow,
    /// `permissionDecision: "allow"` — skip the permission prompt.
    Approve,
    /// `permissionDecision: "ask"` — force the permission prompt.
    ForceAsk,
    /// A hook exited with code 2; carries the reason shown to the model.
    Block(String),
    /// A hook exited non-zero (non-blocking); carries a warning.
    Warn(String),
}

/// Result of `PreToolUse` hooks, matching the reference's JSON contract:
/// `permissionDecision`, `updatedInput`, `additionalContext`, `systemMessage`.
#[derive(Debug)]
pub struct PreToolHookResult {
    pub outcome: PreHookOutcome,
    /// `hookSpecificOutput.updatedInput` — replaces the tool input entirely.
    pub updated_input: Option<serde_json::Value>,
    /// `hookSpecificOutput.additionalContext` — appended to the tool result.
    pub additional_context: Option<String>,
    /// `systemMessage` — surfaced to the user.
    pub system_message: Option<String>,
}

impl PreToolHookResult {
    fn allow() -> Self {
        Self {
            outcome: PreHookOutcome::Allow,
            updated_input: None,
            additional_context: None,
            system_message: None,
        }
    }
}

fn hook_input_json(
    event: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    session_id: &str,
    cwd: &Path,
) -> String {
    serde_json::json!({
        "hook_event_name": event,
        "tool_name": tool_name,
        "tool_input": tool_input,
        "session_id": session_id,
        "cwd": cwd.display().to_string(),
    })
    .to_string()
}

/// Event-specific default timeouts, matching the reference: SessionEnd
/// hooks get 1.5s; everything else 10 minutes (tool-hook default).
fn default_hook_timeout_ms(event: &str) -> u64 {
    if event == "SessionEnd" {
        1_500
    } else {
        10 * 60 * 1_000
    }
}

fn run_hook_command(
    hook: &HookCommand,
    cwd: &Path,
    stdin_json: &str,
) -> std::io::Result<std::process::Output> {
    let command = hook.command.as_str();
    let timeout_ms = hook
        .timeout_ms
        .unwrap_or_else(|| default_hook_timeout_ms(&hook.event));
    let parsed: serde_json::Value =
        serde_json::from_str(stdin_json).unwrap_or(serde_json::Value::Null);
    let hook_event = parsed["hook_event_name"].as_str().unwrap_or("unknown");
    let tool_name = parsed["tool_name"].as_str().unwrap_or_default();
    let session_id = parsed["session_id"].as_str().unwrap_or_default();
    let hook_id = format!("{}:{}", hook_event, &command[..command.len().min(40)]);
    emit_hook_lifecycle(&serde_json::json!({
        "stage": "started",
        "hook_id": hook_id,
        "hook_name": command,
        "hook_event": hook_event,
        "tool_name": tool_name,
        "session_id": session_id,
    }));

    let mut child = std::process::Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_json.as_bytes());
    }
    // Drain pipes on threads so a hook writing lots of output can't
    // deadlock on a full pipe while we poll for the deadline. The stdout
    // reader forwards the first line over a channel so we can detect
    // `{"async": true}` markers, like the reference's async hooks.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (first_tx, first_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut first_sent = false;
        let mut chunk = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut stdout, &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if !first_sent {
                        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let _ = first_tx.send(buf[..pos].to_vec());
                            first_sent = true;
                        }
                    }
                }
            }
        }
        if !first_sent {
            let _ = first_tx.send(buf.clone());
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut first_checked = false;
    let status = loop {
        // A hook emitting {"async": true} as its first stdout line runs
        // detached — we return immediately and a waiter thread reaps it.
        if !first_checked {
            if let Ok(first) = first_rx.try_recv() {
                first_checked = true;
                if serde_json::from_str::<serde_json::Value>(
                    &String::from_utf8_lossy(&first),
                )
                .ok()
                .and_then(|j| j.get("async").and_then(|a| a.as_bool()))
                == Some(true)
                {
                    let async_hook_id = hook_id.clone();
                    let async_command = command.to_string();
                    let async_event = hook_event.to_string();
                    let async_tool = tool_name.to_string();
                    let async_session = session_id.to_string();
                    std::thread::spawn(move || {
                        let _ = child.wait();
                        let _ = out_thread.join();
                        let _ = err_thread.join();
                        emit_hook_lifecycle(&serde_json::json!({
                            "stage": "response",
                            "hook_id": async_hook_id,
                            "hook_name": async_command,
                            "hook_event": async_event,
                            "tool_name": async_tool,
                            "session_id": async_session,
                            "async_completed": true,
                        }));
                    });
                    emit_hook_lifecycle(&serde_json::json!({
                        "stage": "response",
                        "hook_id": hook_id,
                        "hook_name": command,
                        "hook_event": hook_event,
                        "tool_name": tool_name,
                        "session_id": session_id,
                        "async": true,
                    }));
                    return Ok(std::process::Output {
                        status: std::process::ExitStatus::default(),
                        stdout: first,
                        stderr: Vec::new(),
                    });
                }
            }
        }
        match child.try_wait()? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_thread.join();
                let _ = err_thread.join();
                emit_hook_lifecycle(&serde_json::json!({
                    "stage": "response",
                    "hook_id": hook_id,
                    "hook_name": command,
                    "hook_event": hook_event,
                    "tool_name": tool_name,
                    "session_id": session_id,
                    "timed_out": true,
                    "timeout_ms": timeout_ms,
                }));
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("hook `{command}` timed out after {timeout_ms}ms"),
                ));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    let out = std::process::Output {
        status,
        stdout: out_thread.join().unwrap_or_default(),
        stderr: err_thread.join().unwrap_or_default(),
    };
    emit_hook_lifecycle(&serde_json::json!({
        "stage": "response",
        "hook_id": hook_id,
        "hook_name": command,
        "hook_event": hook_event,
        "tool_name": tool_name,
        "session_id": session_id,
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
        "exit_code": out.status.code(),
        "outcome": if out.status.success() { "success" } else { "non_blocking_error" },
    }));
    Ok(out)
}

/// `--include-hook-events`: a process-global sink that receives
/// hook-lifecycle records (`hook_started`/`hook_response`) so the
/// stream-json protocol can forward them.
static HOOK_EVENT_SINK: std::sync::Mutex<Option<Box<dyn Fn(&serde_json::Value) + Send + Sync>>> =
    std::sync::Mutex::new(None);

pub fn set_hook_event_sink(sink: Option<Box<dyn Fn(&serde_json::Value) + Send + Sync>>) {
    if let Ok(mut guard) = HOOK_EVENT_SINK.lock() {
        *guard = sink;
    }
}

fn emit_hook_lifecycle(record: &serde_json::Value) {
    if let Ok(guard) = HOOK_EVENT_SINK.lock() {
        if let Some(sink) = guard.as_ref() {
            sink(record);
        }
    }
}

/// Run `PreToolUse` hooks matching `tool_name`. First blocking hook wins.
pub fn run_pre_tool_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> PreToolHookResult {
    if config.disabled {
        return PreToolHookResult::allow();
    }
    let mut result = PreToolHookResult::allow();
    let mut warning = None;
    for hook in hooks_for_event(config, "PreToolUse") {
        if !matcher_matches(hook.matcher.as_deref(), tool_name) {
            continue;
        }
        let stdin = hook_input_json("PreToolUse", tool_name, input, session_id, cwd);
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        // JSON stdout can carry a programmatic permission decision, like
        // the reference's hookSpecificOutput.permissionDecision.
        if let Ok(json) =
            serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&out.stdout).trim())
        {
            let reason_of = |j: &serde_json::Value| {
                j.get("hookSpecificOutput")
                    .and_then(|o| o.get("permissionDecisionReason"))
                    .and_then(|r| r.as_str())
                    .filter(|r| !r.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        j.get("reason")
                            .and_then(|r| r.as_str())
                            .filter(|r| !r.trim().is_empty())
                            .map(str::to_string)
                    })
            };
            if let Some(msg) = json
                .get("systemMessage")
                .and_then(|m| m.as_str())
                .filter(|m| !m.trim().is_empty())
            {
                result.system_message = Some(msg.to_string());
            }
            if let Some(hso) = json.get("hookSpecificOutput") {
                match hso.get("permissionDecision").and_then(|d| d.as_str()) {
                    Some("deny") => {
                        result.outcome = PreHookOutcome::Block(reason_of(&json).unwrap_or_else(
                            || format!("Blocked by hook `{}`", hook.command),
                        ));
                        return result;
                    }
                    Some("allow") => result.outcome = PreHookOutcome::Approve,
                    Some("ask") => result.outcome = PreHookOutcome::ForceAsk,
                    _ => {}
                }
                if let Some(ui) = hso.get("updatedInput") {
                    result.updated_input = Some(ui.clone());
                }
                if let Some(ctx) = hso
                    .get("additionalContext")
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.trim().is_empty())
                {
                    result.additional_context = Some(ctx.to_string());
                }
            }
            // Legacy top-level decision: "approve" | "block".
            match json.get("decision").and_then(|d| d.as_str()) {
                Some("block") => {
                    result.outcome = PreHookOutcome::Block(reason_of(&json).unwrap_or_else(
                        || format!("Blocked by hook `{}`", hook.command),
                    ));
                    return result;
                }
                Some("approve") => result.outcome = PreHookOutcome::Approve,
                _ => {}
            }
            if json.get("continue").and_then(|v| v.as_bool()) == Some(false) {
                let reason = json
                    .get("stopReason")
                    .and_then(|r| r.as_str())
                    .filter(|r| !r.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| reason_of(&json))
                    .unwrap_or_else(|| format!("Stopped by hook `{}`", hook.command));
                result.outcome = PreHookOutcome::Block(reason);
                return result;
            }
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        match out.status.code() {
            Some(0) => {}
            Some(2) => {
                let reason = if stderr.is_empty() {
                    format!("blocked by PreToolUse hook `{}`", hook.command)
                } else {
                    stderr
                };
                result.outcome = PreHookOutcome::Block(reason);
                return result;
            }
            _ => {
                if !stderr.is_empty() && warning.is_none() {
                    warning = Some(format!(
                        "PreToolUse hook `{}` exited {}: {}",
                        hook.command,
                        out.status.code().unwrap_or(-1),
                        stderr
                    ));
                }
            }
        }
    }
    if let Some(w) = warning {
        result.outcome = PreHookOutcome::Warn(w);
    }
    result
}

/// Run `PostToolUse` hooks. Exit 2 appends stderr to the tool result (the
/// reference feeds hook stderr back to the model); other failures warn.
/// Result of `PostToolUse` hooks — `feedback` is appended to the tool
/// result, `updated_mcp_output` replaces an MCP tool's output entirely
/// (reference: hookSpecificOutput.updatedMCPToolOutput).
#[derive(Debug, Default)]
pub struct PostToolHookResult {
    pub feedback: Vec<String>,
    pub updated_mcp_output: Option<serde_json::Value>,
}

pub fn run_post_tool_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    input: &serde_json::Value,
    tool_response_preview: &str,
) -> PostToolHookResult {
    let mut result = PostToolHookResult::default();
    if config.disabled {
        return result;
    }
    let feedback = &mut result.feedback;
    for hook in hooks_for_event(config, "PostToolUse") {
        if !matcher_matches(hook.matcher.as_deref(), tool_name) {
            continue;
        }
        let mut stdin_json: serde_json::Value = serde_json::from_str(&hook_input_json(
            "PostToolUse",
            tool_name,
            input,
            session_id,
            cwd,
        ))
        .unwrap_or_default();
        stdin_json["tool_response"] = tool_response_preview.into();
        let Ok(out) = run_hook_command(hook, cwd, &stdin_json.to_string()) else {
            continue;
        };
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(ctx) = json
                .get("hookSpecificOutput")
                .and_then(|o| o.get("additionalContext"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.trim().is_empty())
            {
                feedback.push(ctx.to_string());
            }
            if json.get("decision").and_then(|d| d.as_str()) == Some("block") {
                feedback.push(
                    json.get("reason")
                        .and_then(|r| r.as_str())
                        .filter(|r| !r.trim().is_empty())
                        .unwrap_or("Blocked by hook")
                        .to_string(),
                );
            }
            if let Some(updated) = json
                .get("hookSpecificOutput")
                .and_then(|o| o.get("updatedMCPToolOutput"))
            {
                result.updated_mcp_output = Some(updated.clone());
            }
        }
        if out.status.code() == Some(2) && !stderr.is_empty() {
            feedback.push(stderr);
        }
    }
    result
}

fn session_hook_input(
    event: &str,
    session_id: &str,
    cwd: &Path,
    extra: serde_json::Value,
) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": event,
        "session_id": session_id,
        "cwd": cwd.display().to_string(),
    });
    if let (Some(base), serde_json::Value::Object(extra_obj)) = (payload.as_object_mut(), extra) {
        base.extend(extra_obj);
    }
    payload.to_string()
}

/// Result of `SessionStart` hooks — `context` becomes session context,
/// `initial_user_message` (reference: hookSpecificOutput.initialUserMessage)
/// optionally becomes the session's first user message.
#[derive(Debug, Default)]
pub struct SessionStartHookResult {
    pub context: Vec<String>,
    pub initial_user_message: Option<String>,
}

/// Run `SessionStart` hooks — stdout becomes additional context for the
/// session, like the reference's `processSessionStartHooks`.
pub fn run_session_start_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    source: &str,
    model: &str,
) -> SessionStartHookResult {
    let mut result = SessionStartHookResult::default();
    if config.disabled {
        return result;
    }
    let stdin = session_hook_input(
        "SessionStart",
        session_id,
        cwd,
        serde_json::json!({ "source": source, "model": model }),
    );
    for hook in hooks_for_event(config, "SessionStart") {
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            let hso = json.get("hookSpecificOutput");
            if let Some(msg) = hso
                .and_then(|o| o.get("initialUserMessage"))
                .and_then(|m| m.as_str())
                .filter(|m| !m.trim().is_empty())
            {
                result.initial_user_message = Some(msg.to_string());
            }
            if let Some(ctx) = hso
                .and_then(|o| o.get("additionalContext"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.trim().is_empty())
            {
                result.context.push(ctx.to_string());
                continue;
            }
        }
        if !stdout.is_empty() {
            result.context.push(stdout);
        }
    }
    result
}

/// Run a simple event and collect `hookSpecificOutput.additionalContext`
/// values from JSON stdout — used by PostToolUseFailure and Notification
/// whose additionalContext is appended to the tool result.
pub fn run_event_hooks_for_context(
    config: &HooksConfig,
    event: &str,
    session_id: &str,
    cwd: &Path,
    extra: serde_json::Value,
) -> Vec<String> {
    if config.disabled {
        return Vec::new();
    }
    let stdin = session_hook_input(event, session_id, cwd, extra);
    let mut context = Vec::new();
    for hook in hooks_for_event(config, event) {
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(ctx) = json
                .get("hookSpecificOutput")
                .and_then(|o| o.get("additionalContext"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.trim().is_empty())
            {
                context.push(ctx.to_string());
            }
        }
    }
    context
}

/// Run `SessionEnd` hooks — fire-and-forget (clear, resume-away, exit).
pub fn run_session_end_hooks(config: &HooksConfig, session_id: &str, cwd: &Path, reason: &str) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input(
        "SessionEnd",
        session_id,
        cwd,
        serde_json::json!({ "reason": reason }),
    );
    for hook in hooks_for_event(config, "SessionEnd") {
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

/// Outcome of `Stop` hooks, which run when the assistant would finish a
/// turn. A blocking hook forces the turn to continue with the reason fed
/// back to the model.
pub enum StopHookOutcome {
    Continue,
    Block(String),
}

/// Run `Stop` hooks. Exit 2 blocks stopping; stderr is the reason shown
/// to the model (reference `runStopHooks` semantics).
pub fn run_stop_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    stop_hook_active: bool,
) -> StopHookOutcome {
    if config.disabled {
        return StopHookOutcome::Continue;
    }
    let stdin = session_hook_input(
        "Stop",
        session_id,
        cwd,
        serde_json::json!({ "stop_hook_active": stop_hook_active }),
    );
    for hook in hooks_for_event(config, "Stop") {
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        // JSON stdout: decision:block or continue:false stops the loop
        // from ending, with reason/stopReason fed back to the model.
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            let reason = || {
                json.get("reason")
                    .or_else(|| json.get("stopReason"))
                    .and_then(|r| r.as_str())
                    .filter(|r| !r.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("blocked by Stop hook `{}`", hook.command))
            };
            if json.get("decision").and_then(|d| d.as_str()) == Some("block")
                || json.get("continue").and_then(|v| v.as_bool()) == Some(false)
            {
                return StopHookOutcome::Block(reason());
            }
        }
        if out.status.code() == Some(2) {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return StopHookOutcome::Block(if stderr.is_empty() {
                format!("blocked by Stop hook `{}`", hook.command)
            } else {
                stderr
            });
        }
    }
    StopHookOutcome::Continue
}

/// Run `SubagentStop` hooks when a child agent finishes — fire-and-forget.
pub fn run_subagent_stop_hooks(config: &HooksConfig, session_id: &str, cwd: &Path, agent_id: &str) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input(
        "SubagentStop",
        session_id,
        cwd,
        serde_json::json!({ "agent_id": agent_id }),
    );
    for hook in hooks_for_event(config, "SubagentStop") {
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

/// Decision returned by `PermissionRequest` hooks — they can resolve a
/// permission prompt programmatically via JSON stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionHookDecision {
    /// Approved — `updated_input` replaces the tool input when present
    /// (reference: hookSpecificOutput.decision.updatedInput).
    Allow {
        updated_input: Option<serde_json::Value>,
        /// `decision.updatedPermissions` — rule/mode/directory updates to
        /// apply when the decision allows.
        updates: Vec<crate::permissions::PermissionUpdate>,
    },
    /// Denied — `interrupt: true` also ends the turn, not just this call.
    Deny { interrupt: bool },
    Ask,
}

/// Run `PermissionRequest` hooks when a permission prompt would be raised.
/// A hook's `hookSpecificOutput.permissionDecision` (`allow`|`deny`|`ask`)
/// short-circuits the prompt; `deny`/`ask` also carry an optional reason.
pub fn run_permission_request_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<PermissionHookDecision> {
    if config.disabled {
        return None;
    }
    for hook in hooks_for_event(config, "PermissionRequest") {
        if !matcher_matches(hook.matcher.as_deref(), tool_name) {
            continue;
        }
        let stdin = hook_input_json("PermissionRequest", tool_name, input, session_id, cwd);
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        let Ok(json) =
            serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&out.stdout).trim())
        else {
            continue;
        };
        let hso = match json.get("hookSpecificOutput") {
            Some(o) => o,
            None => continue,
        };
        // Reference shape: hookSpecificOutput.decision.{behavior,
        // updatedInput?, message?}. Fall back to permissionDecision for
        // compatibility with hooks written against the PreToolUse shape.
        if let Some(decision) = hso.get("decision") {
            match decision.get("behavior").and_then(|b| b.as_str()) {
                Some("allow") => {
                    return Some(PermissionHookDecision::Allow {
                        updated_input: decision.get("updatedInput").cloned(),
                        updates: decision
                            .get("updatedPermissions")
                            .map(crate::permissions::parse_permission_updates)
                            .unwrap_or_default(),
                    })
                }
                Some("deny") => {
                    return Some(PermissionHookDecision::Deny {
                        interrupt: decision
                            .get("interrupt")
                            .and_then(|i| i.as_bool())
                            .unwrap_or(false),
                    })
                }
                _ => {}
            }
        }
        match hso.get("permissionDecision").and_then(|d| d.as_str()) {
            Some("allow") => {
                return Some(PermissionHookDecision::Allow {
                    updated_input: None,
                    updates: Vec::new(),
                })
            }
            Some("deny") => return Some(PermissionHookDecision::Deny { interrupt: false }),
            Some("ask") => return Some(PermissionHookDecision::Ask),
            _ => {}
        }
    }
    None
}

/// Run `PermissionDenied` hooks when a tool call is denied — fire-and-forget.
pub fn run_permission_denied_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    reason: &str,
) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input(
        "PermissionDenied",
        session_id,
        cwd,
        serde_json::json!({ "tool_name": tool_name, "reason": reason }),
    );
    for hook in hooks_for_event(config, "PermissionDenied") {
        if !matcher_matches(hook.matcher.as_deref(), tool_name) {
            continue;
        }
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

/// Run `SubagentStart` hooks when a child agent begins. Returns
/// `hookSpecificOutput.additionalContext` for injection into the
/// subagent's context, like the reference.
pub fn run_subagent_start_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    agent_id: &str,
    agent_type: Option<&str>,
) -> Option<String> {
    if config.disabled {
        return None;
    }
    let stdin = session_hook_input(
        "SubagentStart",
        session_id,
        cwd,
        serde_json::json!({
            "agent_id": agent_id,
            "agent_type": agent_type.unwrap_or("general-purpose"),
        }),
    );
    for hook in hooks_for_event(config, "SubagentStart") {
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(ctx) = json
                .get("hookSpecificOutput")
                .and_then(|o| o.get("additionalContext"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.trim().is_empty())
            {
                return Some(ctx.to_string());
            }
        }
    }
    None
}

/// Run `Notification` hooks — fired when the UI would notify the user
/// (permission requests, idle). Fire-and-forget.
pub fn run_notification_hooks(config: &HooksConfig, session_id: &str, cwd: &Path, message: &str) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input(
        "Notification",
        session_id,
        cwd,
        serde_json::json!({ "message": message }),
    );
    for hook in hooks_for_event(config, "Notification") {
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

/// Generic fire-and-forget runner for events whose payload is just the
/// shared base plus an extra object (TaskCreated, TaskCompleted, …).
pub fn run_simple_event_hooks(
    config: &HooksConfig,
    event: &str,
    session_id: &str,
    cwd: &Path,
    extra: serde_json::Value,
) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input(event, session_id, cwd, extra);
    for hook in hooks_for_event(config, event) {
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

/// Whether any hooks are configured for an event.
pub fn has_hooks(config: &HooksConfig, event: &str) -> bool {
    !config.disabled && !hooks_for_event(config, event).is_empty()
}

/// Run hooks for an event and return the first non-empty stdout. Used by
/// `WorktreeCreate`, where the hook's output IS the worktree path (the hook
/// fully replaces the default git backend when configured).
pub fn run_output_hooks(
    config: &HooksConfig,
    event: &str,
    session_id: &str,
    cwd: &Path,
    extra: serde_json::Value,
) -> Option<String> {
    if config.disabled {
        return None;
    }
    let stdin = session_hook_input(event, session_id, cwd, extra);
    for hook in hooks_for_event(config, event) {
        if let Ok(out) = run_hook_command(hook, cwd, &stdin) {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if stdout.is_empty() {
                    continue;
                }
                // JSON output: hookSpecificOutput carries the value under
                // event-specific keys (e.g. WorktreeCreate.worktreePath).
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    if let Some(v) = json
                        .get("hookSpecificOutput")
                        .and_then(|o| o.get("worktreePath"))
                        .and_then(|p| p.as_str())
                        .filter(|p| !p.trim().is_empty())
                    {
                        return Some(v.to_string());
                    }
                }
                return Some(stdout);
            }
        }
    }
    None
}

/// Run `PreCompact` hooks before session compaction — fire-and-forget.
pub fn run_pre_compact_hooks(config: &HooksConfig, session_id: &str, cwd: &Path) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input("PreCompact", session_id, cwd, serde_json::json!({}));
    for hook in hooks_for_event(config, "PreCompact") {
        let _ = run_hook_command(hook, cwd, &stdin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_match_hooks() {
        let _guard = crate::test_support::env_lock();
        let fake_home = std::env::temp_dir().join("clawed_hooks_home");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
        let dir = std::env::temp_dir().join("clawed_hooks_test");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.local.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"apply_patch|write_file","hooks":[{"type":"command","command":"echo nope >&2; exit 2"}]}]}}"#,
        ).unwrap();
        let config = load_hooks(&dir);
        assert_eq!(config.hooks.len(), 1);
        match run_pre_tool_hooks(&config, "s", &dir, "apply_patch", &serde_json::json!({}))
            .outcome
        {
            PreHookOutcome::Block(r) => assert!(r.contains("nope"), "{r}"),
            _ => panic!("expected block"),
        }
        // Non-matching tool passes through.
        match run_pre_tool_hooks(&config, "s", &dir, "read_file", &serde_json::json!({}))
            .outcome
        {
            PreHookOutcome::Allow => {}
            _ => panic!("expected allow"),
        }
    }

    #[test]
    fn matcher_variants() {
        assert!(matcher_matches(None, "apply_patch"));
        assert!(matcher_matches(Some("*"), "apply_patch"));
        assert!(matcher_matches(Some("apply_patch"), "apply_patch"));
        assert!(matcher_matches(Some("Edit|Write"), "Write"));
        assert!(matcher_matches(Some("mcp__*"), "mcp__github__get"));
        assert!(!matcher_matches(Some("Bash"), "read_file"));
    }
}

pub enum PromptHookOutcome {
    /// Prompt may proceed; carries hook stdout to append as context.
    Allow(Vec<String>),
    /// A hook exited 2; prompt is blocked and the reason shown to the user.
    Block(String),
}

/// Run `UserPromptSubmit` hooks. Matchers apply to the prompt text (empty or
/// `*` matchers always run). Hook stdout is appended to the prompt as extra
/// context, like the reference's `additionalContext`.
pub fn run_prompt_submit_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    prompt: &str,
) -> PromptHookOutcome {
    if config.disabled {
        return PromptHookOutcome::Allow(Vec::new());
    }
    let mut context = Vec::new();
    for hook in hooks_for_event(config, "UserPromptSubmit") {
        if !matcher_matches(hook.matcher.as_deref(), prompt) {
            continue;
        }
        let stdin = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
            "session_id": session_id,
            "cwd": cwd.display().to_string(),
        })
        .to_string();
        let Ok(out) = run_hook_command(hook, cwd, &stdin) else {
            continue;
        };
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match out.status.code() {
            Some(2) => {
                return PromptHookOutcome::Block(if stderr.is_empty() {
                    format!("blocked by UserPromptSubmit hook `{}`", hook.command)
                } else {
                    stderr
                });
            }
            _ => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    if let Some(ctx) = json
                        .get("hookSpecificOutput")
                        .and_then(|o| o.get("additionalContext"))
                        .and_then(|c| c.as_str())
                        .filter(|c| !c.trim().is_empty())
                    {
                        context.push(ctx.to_string());
                        continue;
                    }
                }
                if !stdout.is_empty() {
                    context.push(stdout);
                }
            }
        }
    }
    PromptHookOutcome::Allow(context)
}
