//! Minimal Linux sandbox subsystem, modeled on the reference's
//! `sandbox.*` settings + bubblewrap adapter. Commands run inside `bwrap`
//! with a read-only root, writable cwd, and per-path write/deny binds.
//! Network isolation and macOS seatbelt are intentionally out of scope.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub enabled: bool,
    /// Managed deployments: exit at startup when sandboxing can't start.
    pub fail_if_unavailable: bool,
    /// Reference `autoAllowBashIfSandboxed` — sandboxed shell needs no prompt.
    pub auto_allow_bash_if_sandboxed: bool,
    /// Reference `allowUnsandboxedCommands` (default true): whether the
    /// `dangerouslyDisableSandbox` tool input may escape the sandbox.
    pub allow_unsandboxed_commands: bool,
    /// `filesystem.allowWrite` — extra writable paths.
    pub allow_write: Vec<PathBuf>,
    /// `filesystem.denyWrite` — paths forced read-only inside the sandbox.
    pub deny_write: Vec<PathBuf>,
    /// Undocumented passthrough: `enabledPlatforms` restricts platforms
    /// (e.g. ["macos"]). None means every supported platform.
    pub enabled_platforms: Option<Vec<String>>,
    /// `excludedCommands` — command patterns that skip the sandbox.
    pub excluded_commands: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fail_if_unavailable: false,
            auto_allow_bash_if_sandboxed: false,
            allow_unsandboxed_commands: true,
            allow_write: Vec::new(),
            deny_write: Vec::new(),
            enabled_platforms: None,
            excluded_commands: Vec::new(),
        }
    }
}

static CONFIG: RwLock<Option<SandboxConfig>> = RwLock::new(None);

fn config() -> SandboxConfig {
    CONFIG
        .read()
        .ok()
        .and_then(|c| c.clone())
        .unwrap_or_default()
}

/// Apply `settings.sandbox` — called by the runtime with the merged
/// settings snapshot.
pub fn configure(settings: &Value) {
    let mut cfg = SandboxConfig::default();
    let Some(sandbox) = settings.get("sandbox") else {
        if let Ok(mut lock) = CONFIG.write() {
            *lock = Some(cfg);
        }
        return;
    };
    cfg.enabled = sandbox
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    cfg.fail_if_unavailable = sandbox
        .get("failIfUnavailable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    cfg.auto_allow_bash_if_sandboxed = sandbox
        .get("autoAllowBashIfSandboxed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    cfg.allow_unsandboxed_commands = sandbox
        .get("allowUnsandboxedCommands")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    cfg.enabled_platforms = sandbox
        .get("enabledPlatforms")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    let fs = sandbox.get("filesystem");
    let paths = |key: &str| -> Vec<PathBuf> {
        fs.and_then(|f| f.get(key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    cfg.allow_write = paths("allowWrite");
    cfg.deny_write = paths("denyWrite");
    cfg.excluded_commands = sandbox
        .get("excludedCommands")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // The reference deny-writes the project's own settings + skills inside
    // the sandbox so a sandboxed agent can't rewrite its permissions.
    cfg.deny_write.push(PathBuf::from(".claude/settings.json"));
    cfg.deny_write.push(PathBuf::from(".claude/settings.local.json"));
    cfg.deny_write.push(PathBuf::from(".claude/skills"));
    if let Ok(mut lock) = CONFIG.write() {
        *lock = Some(cfg);
    }
}

/// Current platform token per the reference's enabledPlatforms values.
fn platform_token() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// `bwrap` on PATH (Linux only — the reference uses seatbelt on macOS,
/// which this port does not implement).
pub fn is_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        cfg!(target_os = "linux")
            && std::process::Command::new("bwrap")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    })
}

/// Why the sandbox can't start, when `enabled` is set.
pub fn unavailable_reason() -> Option<String> {
    let cfg = config();
    if !cfg.enabled {
        return None;
    }
    if let Some(platforms) = &cfg.enabled_platforms
        && !platforms.iter().any(|p| p == platform_token())
    {
        return Some(format!(
            "sandbox.enabledPlatforms does not include {}",
            platform_token()
        ));
    }
    if !is_available() {
        return Some("bubblewrap (bwrap) is not installed".to_string());
    }
    None
}

/// Whether sandboxing is actually in effect this session.
pub fn is_active() -> bool {
    unavailable_reason().is_none() && config().enabled
}

/// `autoAllowBashIfSandboxed` — true only when sandboxing is live.
pub fn auto_allow_active() -> bool {
    is_active() && config().auto_allow_bash_if_sandboxed
}

/// Status summary for `/sandbox` / diagnostics.
pub fn status_lines() -> Vec<String> {
    let cfg = config();
    let mut lines = vec![
        format!("enabled: {}", cfg.enabled),
        format!("bwrap available: {}", is_available()),
        format!("active: {}", is_active()),
    ];
    if let Some(reason) = unavailable_reason() {
        lines.push(format!("unavailable: {reason}"));
    }
    if cfg.enabled {
        lines.push(format!(
            "allowUnsandboxedCommands: {}",
            cfg.allow_unsandboxed_commands
        ));
        lines.push(format!(
            "autoAllowBashIfSandboxed: {}",
            cfg.auto_allow_bash_if_sandboxed
        ));
        lines.push(format!("allowWrite: {} paths", cfg.allow_write.len()));
        lines.push(format!("denyWrite: {} paths", cfg.deny_write.len()));
    }
    lines
}

/// `sandbox.excludedCommands` — the reference's shouldUseSandbox check:
/// split compound commands, strip env-var/wrapper prefixes, then match
/// each subcommand against `name:*` prefix, exact, or `*` wildcard
/// patterns. Convenience only, not a security boundary.
pub fn is_command_excluded(command: &str) -> bool {
    let cfg = config();
    if cfg.excluded_commands.is_empty() {
        return false;
    }
    for sub in split_compound(command) {
        for cand in stripped_candidates(&sub) {
            for pattern in &cfg.excluded_commands {
                if pattern_matches_command(pattern, &cand) {
                    return true;
                }
            }
        }
    }
    false
}

fn split_compound(command: &str) -> Vec<String> {
    command
        .split(|c| matches!(c, ';' | '|'))
        .flat_map(|seg| seg.split("&&"))
        .map(|seg| seg.trim().to_string())
        .filter(|seg| !seg.is_empty())
        .collect()
}

/// Strip leading `VAR=value` pairs and wrapper prefixes (`timeout N`,
/// `sudo`, `env`, `nice`, `nohup`) to a fixed point — like the reference's
/// candidates loop.
fn stripped_candidates(sub: &str) -> Vec<String> {
    let mut out = vec![sub.to_string()];
    let mut idx = 0;
    while idx < out.len() {
        let cand = out[idx].clone();
        idx += 1;
        let mut words = cand.split_whitespace().peekable();
        // env vars
        while let Some(w) = words.peek() {
            if w.contains('=') && !w.starts_with('-') {
                words.next();
            } else {
                break;
            }
        }
        // wrappers
        loop {
            match words.peek().copied() {
                Some("sudo") | Some("nohup") | Some("nice") | Some("env") => {
                    words.next();
                }
                Some("timeout") => {
                    words.next();
                    words.next(); // duration
                }
                _ => break,
            }
        }
        let stripped: Vec<&str> = words.collect();
        let stripped = stripped.join(" ");
        if !stripped.is_empty() && stripped != cand && !out.contains(&stripped) {
            out.push(stripped);
        }
    }
    out
}

fn pattern_matches_command(pattern: &str, command: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(":*") {
        return command == prefix || command.starts_with(&format!("{prefix} "));
    }
    if pattern.contains('*') {
        return wildcard_match(pattern, command);
    }
    pattern == command
}

/// Glob matcher where `*` spans any characters (incl. spaces) — like the
/// reference's matchWildcardPattern on the whole command string.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = pi;
            star_t = ti;
            pi += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Wrap a shell spec in `bwrap` when the sandbox is active and the caller
/// didn't legitimately disable it. Returns the (possibly rewritten)
/// program + args.
pub fn wrap_spec(
    program: &str,
    args: &[String],
    cwd: &Path,
    dangerously_disable_sandbox: bool,
    raw_command: &str,
) -> (String, Vec<String>) {
    if !is_active() || is_command_excluded(raw_command) {
        return (program.to_string(), args.to_vec());
    }
    let cfg = config();
    if dangerously_disable_sandbox && cfg.allow_unsandboxed_commands {
        return (program.to_string(), args.to_vec());
    }
    let mut bargs: Vec<String> = vec![
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];
    // Writable binds: the project root plus filesystem.allowWrite.
    let mut write_paths = vec![cwd.to_path_buf()];
    write_paths.extend(cfg.allow_write.iter().map(|p| {
        if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        }
    }));
    for path in write_paths {
        if path.exists() {
            bargs.push("--bind".to_string());
            bargs.push(path.display().to_string());
            bargs.push(path.display().to_string());
        }
    }
    // Read-only overlays for denyWrite — applied last so they win.
    for path in &cfg.deny_write {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };
        if abs.exists() {
            bargs.push("--ro-bind".to_string());
            bargs.push(abs.display().to_string());
            bargs.push(abs.display().to_string());
        }
    }
    bargs.push("--".to_string());
    bargs.push(program.to_string());
    bargs.extend(args.iter().cloned());
    ("bwrap".to_string(), bargs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_parses_settings_shape() {
        configure(&serde_json::json!({
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
                "filesystem": {
                    "allowWrite": ["/tmp/out", "build/"],
                    "denyWrite": [".env"]
                }
            }
        }));
        let cfg = config();
        assert!(cfg.enabled);
        assert!(cfg.fail_if_unavailable);
        assert!(cfg.auto_allow_bash_if_sandboxed);
        assert!(!cfg.allow_unsandboxed_commands);
        assert_eq!(cfg.allow_write.len(), 2);
        // User denyWrite + the three reference built-ins.
        assert!(cfg.deny_write.iter().any(|p| p.ends_with(".env")));
        assert!(cfg
            .deny_write
            .iter()
            .any(|p| p.ends_with("settings.local.json")));
        configure(&serde_json::json!({}));
        assert!(!config().enabled);
    }

    #[test]
    fn wrap_spec_passthrough_when_inactive() {
        configure(&serde_json::json!({}));
        let (program, args) = wrap_spec(
            "bash",
            &["-lc".to_string(), "echo hi".to_string()],
            Path::new("/tmp"),
            false,
            "echo hi",
        );
        assert_eq!(program, "bash");
        assert_eq!(args.len(), 2);
    }
}
