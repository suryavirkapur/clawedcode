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
use std::path::Path;

#[derive(Debug, Clone)]
pub struct HookCommand {
    /// Lifecycle event, e.g. `PreToolUse` or `PostToolUse`.
    pub event: String,
    /// Regex matched against the tool name. `None` matches everything.
    pub matcher: Option<String>,
    pub command: String,
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
}

/// Load hooks from `.claude/settings.json` and `.claude/settings.local.json`
/// in `cwd` (the local file wins `disableAllHooks` like the reference's
/// settings precedence).
pub fn load_hooks(cwd: &Path) -> HooksConfig {
    let mut config = HooksConfig::default();
    for name in ["settings.json", "settings.local.json"] {
        let path = cwd.join(".claude").join(name);
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

pub enum PreHookOutcome {
    /// No hook blocked the tool call.
    Allow,
    /// A hook exited with code 2; carries the reason shown to the model.
    Block(String),
    /// A hook exited non-zero (non-blocking); carries a warning.
    Warn(String),
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

fn run_hook_command(
    command: &str,
    cwd: &Path,
    stdin_json: &str,
) -> std::io::Result<std::process::Output> {
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
    child.wait_with_output()
}

/// Run `PreToolUse` hooks matching `tool_name`. First blocking hook wins.
pub fn run_pre_tool_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> PreHookOutcome {
    if config.disabled {
        return PreHookOutcome::Allow;
    }
    let mut warning = None;
    for hook in hooks_for_event(config, "PreToolUse") {
        if !matcher_matches(hook.matcher.as_deref(), tool_name) {
            continue;
        }
        let stdin = hook_input_json("PreToolUse", tool_name, input, session_id, cwd);
        let Ok(out) = run_hook_command(&hook.command, cwd, &stdin) else {
            continue;
        };
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        match out.status.code() {
            Some(0) => {}
            Some(2) => {
                let reason = if stderr.is_empty() {
                    format!("blocked by PreToolUse hook `{}`", hook.command)
                } else {
                    stderr
                };
                return PreHookOutcome::Block(reason);
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
    match warning {
        Some(w) => PreHookOutcome::Warn(w),
        None => PreHookOutcome::Allow,
    }
}

/// Run `PostToolUse` hooks. Exit 2 appends stderr to the tool result (the
/// reference feeds hook stderr back to the model); other failures warn.
pub fn run_post_tool_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    tool_name: &str,
    input: &serde_json::Value,
    tool_response_preview: &str,
) -> Option<String> {
    if config.disabled {
        return None;
    }
    let mut feedback = Vec::new();
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
        let Ok(out) = run_hook_command(&hook.command, cwd, &stdin_json.to_string()) else {
            continue;
        };
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if out.status.code() == Some(2) && !stderr.is_empty() {
            feedback.push(stderr);
        }
    }
    if feedback.is_empty() {
        None
    } else {
        Some(feedback.join("\n"))
    }
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

/// Run `SessionStart` hooks — stdout becomes additional context for the
/// session, like the reference's `processSessionStartHooks`.
pub fn run_session_start_hooks(
    config: &HooksConfig,
    session_id: &str,
    cwd: &Path,
    source: &str,
    model: &str,
) -> Vec<String> {
    if config.disabled {
        return Vec::new();
    }
    let stdin = session_hook_input(
        "SessionStart",
        session_id,
        cwd,
        serde_json::json!({ "source": source, "model": model }),
    );
    let mut context = Vec::new();
    for hook in hooks_for_event(config, "SessionStart") {
        let Ok(out) = run_hook_command(&hook.command, cwd, &stdin) else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !stdout.is_empty() {
            context.push(stdout);
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
        let _ = run_hook_command(&hook.command, cwd, &stdin);
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
        let Ok(out) = run_hook_command(&hook.command, cwd, &stdin) else {
            continue;
        };
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
        let _ = run_hook_command(&hook.command, cwd, &stdin);
    }
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
        let _ = run_hook_command(&hook.command, cwd, &stdin);
    }
}

/// Run `PreCompact` hooks before session compaction — fire-and-forget.
pub fn run_pre_compact_hooks(config: &HooksConfig, session_id: &str, cwd: &Path) {
    if config.disabled {
        return;
    }
    let stdin = session_hook_input("PreCompact", session_id, cwd, serde_json::json!({}));
    for hook in hooks_for_event(config, "PreCompact") {
        let _ = run_hook_command(&hook.command, cwd, &stdin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_match_hooks() {
        let dir = std::env::temp_dir().join("clawed_hooks_test");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.local.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"apply_patch|write_file","hooks":[{"type":"command","command":"echo nope >&2; exit 2"}]}]}}"#,
        ).unwrap();
        let config = load_hooks(&dir);
        assert_eq!(config.hooks.len(), 1);
        match run_pre_tool_hooks(&config, "s", &dir, "apply_patch", &serde_json::json!({})) {
            PreHookOutcome::Block(r) => assert!(r.contains("nope"), "{r}"),
            _ => panic!("expected block"),
        }
        // Non-matching tool passes through.
        match run_pre_tool_hooks(&config, "s", &dir, "read_file", &serde_json::json!({})) {
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
        let Ok(out) = run_hook_command(&hook.command, cwd, &stdin) else {
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
                if !stdout.is_empty() {
                    context.push(stdout);
                }
            }
        }
    }
    PromptHookOutcome::Allow(context)
}
