use serde::{Deserialize, Serialize};

/// Permission modes controlling tool execution behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Default: tools that need_approval require user confirmation.
    Default,
    /// Accept all edits without prompting (auto-approve write-like tools).
    AcceptEdits,
    /// Plan-only: no tools are actually executed; dry-run.
    Plan,
    /// Bypass all permission checks; execute everything.
    Bypass,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Default
    }
}

/// Decision returned by the permission engine for a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Tool may execute.
    Allow,
    /// Tool must wait for user approval (not enforced here; caller handles UX).
    Ask,
    /// Tool is denied in this mode.
    Deny,
}

/// Permission engine that decides whether a tool call may proceed.
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    /// `Cell` so hook-driven `setMode` permission updates can change the
    /// mode through the `&self` tool-execution path.
    mode: std::cell::Cell<PermissionMode>,
    /// `permissions.disableBypassPermissionsMode: "disable"` — Bypass can
    /// never be entered, from flags, settings, hooks, or mode cycling.
    bypass_disabled: std::cell::Cell<bool>,
    /// `--dangerously-skip-permissions`/`--allow-dangerously-skip-permissions`
    /// — the launch-time opt-in that makes Bypass reachable via the
    /// shift+tab mode cycle, like the reference's
    /// isBypassPermissionsModeAvailable.
    bypass_available: std::cell::Cell<bool>,
}

impl PermissionEngine {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode: std::cell::Cell::new(mode),
            bypass_disabled: std::cell::Cell::new(false),
            bypass_available: std::cell::Cell::new(false),
        }
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode.get()
    }

    /// Whether bypassPermissions mode is disabled by settings.
    pub fn bypass_disabled(&self) -> bool {
        self.bypass_disabled.get()
    }

    pub fn bypass_available(&self) -> bool {
        self.bypass_available.get()
    }

    pub fn set_bypass_available(&self, available: bool) {
        self.bypass_available.set(available);
    }

    pub fn set_bypass_disabled(&self, disabled: bool) {
        self.bypass_disabled.set(disabled);
        // If already in bypass when disabled lands, fall back to default.
        if disabled && self.mode.get() == PermissionMode::Bypass {
            self.mode.set(PermissionMode::Default);
        }
    }

    /// Set the mode — `&self` because hook-driven updates apply mid-call.
    pub fn set_mode(&self, mode: PermissionMode) {
        if mode == PermissionMode::Bypass && self.bypass_disabled.get() {
            return;
        }
        self.mode.set(mode);
    }

    /// Decide whether a tool call is allowed.
    ///
    /// - `needs_approval`: whether the tool declares it needs user approval.
    /// - `is_write_like`: whether the tool mutates state (edits, shell, etc.).
    pub fn decide(&self, needs_approval: bool, is_write_like: bool) -> PermissionDecision {
        match self.mode.get() {
            PermissionMode::Bypass => PermissionDecision::Allow,
            PermissionMode::Plan => {
                if is_write_like {
                    PermissionDecision::Deny
                } else {
                    PermissionDecision::Allow
                }
            }
            PermissionMode::AcceptEdits => {
                if is_write_like {
                    PermissionDecision::Allow
                } else if needs_approval {
                    PermissionDecision::Ask
                } else {
                    PermissionDecision::Allow
                }
            }
            PermissionMode::Default => {
                if needs_approval {
                    PermissionDecision::Ask
                } else {
                    PermissionDecision::Allow
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_allows_read_tools() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
    }

    #[test]
    fn default_mode_asks_for_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        assert_eq!(engine.decide(true, false), PermissionDecision::Ask);
    }

    #[test]
    fn bypass_mode_allows_everything() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);
        assert_eq!(engine.decide(true, true), PermissionDecision::Allow);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
        assert_eq!(engine.decide(true, false), PermissionDecision::Allow);
    }

    #[test]
    fn plan_mode_denies_writes_but_allows_reads() {
        let engine = PermissionEngine::new(PermissionMode::Plan);
        assert_eq!(engine.decide(true, true), PermissionDecision::Deny);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
        assert_eq!(engine.decide(true, false), PermissionDecision::Allow);
    }

    #[test]
    fn accept_edits_allows_write_like_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(true, true), PermissionDecision::Allow);
    }

    #[test]
    fn accept_edits_asks_for_non_write_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(true, false), PermissionDecision::Ask);
    }

    #[test]
    fn accept_edits_allows_non_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
    }

    #[test]
    fn default_mode_is_default() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }

    #[test]
    fn engine_mode_accessor() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);
        assert_eq!(engine.mode(), PermissionMode::Bypass);
    }

    #[test]
    fn permission_mode_serializes() {
        let mode = PermissionMode::AcceptEdits;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"accept_edits\"");
    }

    #[test]
    fn permission_mode_deserializes() {
        let mode: PermissionMode = serde_json::from_str("\"bypass\"").unwrap();
        assert_eq!(mode, PermissionMode::Bypass);
    }
}

/// Permission rules from settings files — `permissions.allow`, `.deny`,
/// `.ask` arrays of `"Tool"` or `"Tool(content)"` strings, like the
/// reference's permission rule system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// Load permission rules from the same settings files as hooks, in
/// precedence order (user → project → local → --settings).
pub fn load_permission_rules(cwd: &std::path::Path) -> PermissionRules {
    let mut rules = PermissionRules::default();
    if std::env::var_os("CLAWEDCODE_SIMPLE").is_some() {
        return rules;
    }
    // allowManagedPermissionRulesOnly (managed settings): only managed
    // permission rules apply — user/project/local rules are ignored.
    let managed_only = crate::compat::policy_settings()
        .get("allowManagedPermissionRulesOnly")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let paths = if managed_only {
        crate::compat::managed_settings_paths()
    } else {
        crate::hooks::settings_file_paths(cwd)
    };
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(perms) = json.get("permissions") else {
            continue;
        };
        for (key, out) in [
            ("allow", &mut rules.allow),
            ("deny", &mut rules.deny),
            ("ask", &mut rules.ask),
        ] {
            if let Some(items) = perms.get(key).and_then(|v| v.as_array()) {
                out.extend(
                    items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .map(str::to_string),
                );
            }
        }
    }
    rules
}

/// Maps reference rule tool names to ours (the reference names its tools
/// `Bash`, `Read`, `Edit`, `Write`, `Task`; ours are `shell`, `read_file`,
/// `apply_patch`, `Agent`).
fn canonical_rule_tool(name: &str) -> &str {
    match name {
        "Bash" => "shell",
        "Read" => "read_file",
        "Edit" | "Write" | "MultiEdit" => "apply_patch",
        "Task" => "Agent",
        other => other,
    }
}

/// Parse `"Tool(content)"` → `(tool, Some(content))`, `"Tool"` → `(tool,
/// None)`. Content unescapes `\(` `\)` `\\` like the reference.
fn parse_rule(rule: &str) -> (&str, Option<String>) {
    let bytes = rule.as_bytes();
    let mut open = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'(' => {
                open = Some(i);
                break;
            }
            _ => i += 1,
        }
    }
    let Some(open) = open else {
        return (rule.trim(), None);
    };
    if !rule.ends_with(')') {
        return (rule.trim(), None);
    }
    let tool = rule[..open].trim();
    if tool.is_empty() {
        return (rule.trim(), None);
    }
    let raw = &rule[open + 1..rule.len() - 1];
    let content = raw
        .replace("\\(", "\u{1}")
        .replace("\\)", "\u{2}")
        .replace("\\\\", "\u{3}")
        .replace('\u{1}', "(")
        .replace('\u{2}', ")")
        .replace('\u{3}', "\\");
    (tool, Some(content))
}

/// Wildcard match: `*` → any run, with the reference's rule that a single
/// trailing ` *` also matches the bare prefix (`git *` matches `git`).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(":*") {
        return text == prefix || text.starts_with(&format!("{prefix} "));
    }
    fn inner(pat: &[char], txt: &[char]) -> bool {
        match (pat.first(), txt.first()) {
            (None, None) => true,
            (Some(&'*'), _) => {
                inner(&pat[1..], txt) || (!txt.is_empty() && inner(pat, &txt[1..]))
            }
            (Some(&p), Some(&t)) => p == t && inner(&pat[1..], &txt[1..]),
            _ => false,
        }
    }
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    if inner(&pat, &txt) {
        return true;
    }
    // `git *` also matches bare `git` (single trailing wildcard only).
    if let Some(prefix) = pattern.strip_suffix(" *") {
        if !prefix.contains('*') && text == prefix {
            return true;
        }
    }
    false
}

/// Path-glob match for file tool rules: `**` spans directories, `*`/`?`
/// stay within a segment.
fn path_glob(pattern: &str, path: &str) -> bool {
    fn seg(pat: &[&str], parts: &[&str]) -> bool {
        match (pat.first(), parts.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                seg(&pat[1..], parts) || (!parts.is_empty() && seg(pat, &parts[1..]))
            }
            (Some(p), Some(s)) => wildcard_match(p, s) && seg(&pat[1..], &parts[1..]),
            _ => false,
        }
    }
    seg(
        &pattern.split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
}

/// Normalize a rule path spec (`//abs`, `~/home`, `./rel`, `rel`) against
/// cwd/home, like the reference's pathspec handling.
fn normalize_rule_path<'a>(spec: &'a str, cwd: &std::path::Path) -> String {
    if let Some(rest) = spec.strip_prefix("//") {
        format!("/{rest}")
    } else if let Some(rest) = spec.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/{rest}")
    } else if spec.starts_with('/') {
        spec.to_string()
    } else {
        let rel = spec.strip_prefix("./").unwrap_or(spec);
        format!("{}/{rel}", cwd.display())
    }
}

/// Keys a file-ish tool input can carry its target path under.
fn input_path<'a>(input: &'a serde_json::Value) -> Option<&'a str> {
    for key in [
        "path",
        "file_path",
        "filePath",
        "notebook_path",
        "pattern",
        "filePattern",
    ] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return Some(v);
        }
    }
    None
}

fn is_path_tool(tool: &str) -> bool {
    matches!(
        tool,
        "read_file" | "apply_patch" | "write_file" | "Glob" | "Grep" | "NotebookEdit"
    )
}

/// Does `rule` match `tool_name` invoked with `input` in `cwd`?
pub fn rule_matches(
    rule: &str,
    tool_name: &str,
    input: &serde_json::Value,
    cwd: &std::path::Path,
) -> bool {
    let (rule_tool, content) = parse_rule(rule);
    if canonical_rule_tool(rule_tool) != tool_name {
        return false;
    }
    let Some(spec) = content else {
        return true; // bare tool name matches all uses
    };
    match tool_name {
        "shell" => {
            let cmd = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            wildcard_match(&spec, cmd)
        }
        "WebFetch" => {
            let Some(domain) = spec.strip_prefix("domain:") else {
                return false;
            };
            let host = input
                .get("url")
                .and_then(|v| v.as_str())
                .and_then(|u| u.split("://").nth(1))
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default()
                .split(':')
                .next()
                .unwrap_or_default();
            host == domain || host.ends_with(&format!(".{domain}"))
        }
        "WebSearch" => {
            let Some(domain) = spec.strip_prefix("domain:") else {
                return false;
            };
            input
                .get("allowed_domains")
                .and_then(|v| v.as_array())
                .map(|ds| {
                    ds.iter()
                        .filter_map(|d| d.as_str())
                        .any(|d| d == domain || d.ends_with(&format!(".{domain}")))
                })
                .unwrap_or(false)
        }
        "Agent" => {
            input
                .get("subagent_type")
                .or_else(|| input.get("agent_type"))
                .or_else(|| input.get("description"))
                .and_then(|v| v.as_str())
                .map(|a| a == spec || wildcard_match(&spec, a))
                .unwrap_or(false)
        }
        _ if is_path_tool(tool_name) => {
            let Some(path) = input_path(input) else {
                return false;
            };
            let abs = if std::path::Path::new(path).is_absolute() {
                path.to_string()
            } else {
                format!("{}/{path}", cwd.display())
            };
            path_glob(&normalize_rule_path(&spec, cwd), &abs)
        }
        _ => false,
    }
}

impl PermissionRules {
    /// Evaluate rules for a tool call. Precedence like the reference:
    /// deny wins, then ask, then allow.
    pub fn check(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        cwd: &std::path::Path,
    ) -> Option<PermissionDecision> {
        if self
            .deny
            .iter()
            .any(|r| rule_matches(r, tool_name, input, cwd))
        {
            return Some(PermissionDecision::Deny);
        }
        if self
            .ask
            .iter()
            .any(|r| rule_matches(r, tool_name, input, cwd))
        {
            return Some(PermissionDecision::Ask);
        }
        if self
            .allow
            .iter()
            .any(|r| rule_matches(r, tool_name, input, cwd))
        {
            return Some(PermissionDecision::Allow);
        }
        None
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    fn cwd() -> std::path::PathBuf {
        std::path::PathBuf::from("/proj")
    }

    #[test]
    fn parses_bare_tool_and_content_rules() {
        assert_eq!(parse_rule("Bash"), ("Bash", None));
        assert_eq!(
            parse_rule("Bash(npm run build)"),
            ("Bash", Some("npm run build".to_string()))
        );
        assert_eq!(
            parse_rule("Bash(echo \\(x\\))"),
            ("Bash", Some("echo (x)".to_string()))
        );
        assert_eq!(parse_rule("Read(./src/**)"), ("Read", Some("./src/**".into())));
    }

    #[test]
    fn shell_prefix_and_wildcard_rules() {
        let input = serde_json::json!({"command": "npm run build"});
        assert!(rule_matches("Bash(npm:*)", "shell", &input, &cwd()));
        assert!(rule_matches("shell(npm run build)", "shell", &input, &cwd()));
        assert!(rule_matches("Bash(npm *)", "shell", &input, &cwd()));
        assert!(!rule_matches("Bash(cargo:*)", "shell", &input, &cwd()));
        // `git *` also matches bare `git`.
        assert!(rule_matches(
            "Bash(git *)",
            "shell",
            &serde_json::json!({"command": "git"}),
            &cwd()
        ));
    }

    #[test]
    fn path_rules_glob_against_cwd() {
        let input = serde_json::json!({"path": "src/lib.rs"});
        assert!(rule_matches("Read(./src/**)", "read_file", &input, &cwd()));
        assert!(!rule_matches("Read(./tests/**)", "read_file", &input, &cwd()));
        assert!(rule_matches("Read(//proj/src/**)", "read_file", &input, &cwd()));
        let abs = serde_json::json!({"path": "/proj/src/lib.rs"});
        assert!(rule_matches("Edit(//proj/**)", "apply_patch", &abs, &cwd()));
    }

    #[test]
    fn domain_and_agent_rules() {
        let fetch = serde_json::json!({"url": "https://api.example.com/x"});
        assert!(rule_matches("WebFetch(domain:example.com)", "WebFetch", &fetch, &cwd()));
        assert!(!rule_matches("WebFetch(domain:other.com)", "WebFetch", &fetch, &cwd()));
        let agent = serde_json::json!({"subagent_type": "Explore"});
        assert!(rule_matches("Agent(Explore)", "Agent", &agent, &cwd()));
        assert!(rule_matches("Task(Explore)", "Agent", &agent, &cwd()));
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let rules = PermissionRules {
            allow: vec!["shell(echo:*)".into()],
            deny: vec!["shell(echo secret*)".into()],
            ask: vec!["shell(echo askme*)".into()],
        };
        assert_eq!(
            rules.check("shell", &serde_json::json!({"command": "echo hi"}), &cwd()),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(
            rules.check("shell", &serde_json::json!({"command": "echo secret x"}), &cwd()),
            Some(PermissionDecision::Deny)
        );
        assert_eq!(
            rules.check("shell", &serde_json::json!({"command": "echo askme x"}), &cwd()),
            Some(PermissionDecision::Ask)
        );
        assert_eq!(
            rules.check("read_file", &serde_json::json!({"path": "x"}), &cwd()),
            None
        );
    }
}

/// A `hookSpecificOutput.decision.updatedPermissions` entry — the
/// reference discriminates on `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionUpdate {
    /// addRules/replaceRules/removeRules over `permissions.<behavior>` arrays.
    Rules {
        action: RuleAction,
        rules: Vec<String>,
        behavior: String,
        destination: String,
    },
    /// setMode — `default`, `acceptEdits`, `plan`, `bypassPermissions`.
    SetMode { mode: String, destination: String },
    /// addDirectories/removeDirectories.
    Directories {
        add: bool,
        dirs: Vec<String>,
        destination: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    Add,
    Replace,
    Remove,
}

/// Serialize a {toolName, ruleContent} pair to `Tool(content)` form with
/// the reference's escaping (`\` first, then `\(` `\)`).
pub fn rule_to_string(tool_name: &str, rule_content: Option<&str>) -> String {
    match rule_content {
        None => tool_name.to_string(),
        Some(content) => {
            let escaped = content
                .replace('\\', "\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            format!("{tool_name}({escaped})")
        }
    }
}

/// Parse `updatedPermissions` from a PermissionRequest hook decision.
pub fn parse_permission_updates(value: &serde_json::Value) -> Vec<PermissionUpdate> {
    let mut out = Vec::new();
    let Some(items) = value.as_array() else {
        return out;
    };
    for item in items {
        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let destination = item
            .get("destination")
            .and_then(|d| d.as_str())
            .unwrap_or("localSettings")
            .to_string();
        match ty {
            "addRules" | "replaceRules" | "removeRules" => {
                let rules = item
                    .get("rules")
                    .and_then(|r| r.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| {
                                r.get("toolName").and_then(|t| t.as_str()).map(|name| {
                                    rule_to_string(
                                        name,
                                        r.get("ruleContent").and_then(|c| c.as_str()),
                                    )
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let action = match ty {
                    "addRules" => RuleAction::Add,
                    "replaceRules" => RuleAction::Replace,
                    _ => RuleAction::Remove,
                };
                out.push(PermissionUpdate::Rules {
                    action,
                    rules,
                    behavior: item
                        .get("behavior")
                        .and_then(|b| b.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                    destination,
                });
            }
            "setMode" => {
                if let Some(mode) = item.get("mode").and_then(|m| m.as_str()) {
                    out.push(PermissionUpdate::SetMode {
                        mode: mode.to_string(),
                        destination,
                    });
                }
            }
            "addDirectories" | "removeDirectories" => {
                let dirs = item
                    .get("directories")
                    .and_then(|d| d.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| d.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(PermissionUpdate::Directories {
                    add: ty == "addDirectories",
                    dirs,
                    destination,
                });
            }
            _ => {}
        }
    }
    out
}

/// crate-internal helpers for applying permission updates to settings files.
pub(crate) use self::helpers::{
    destination_path, update_dir_list, update_rule_list, write_settings_value,
};

mod helpers {
    pub fn destination_path(
        destination: &str,
        cwd: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        match destination {
            "userSettings" => crate::compat::claude_home().map(|h| h.join("settings.json")),
            "projectSettings" => Some(cwd.join(".claude").join("settings.json")),
            "localSettings" => Some(cwd.join(".claude").join("settings.local.json")),
            _ => None,
        }
    }

    fn load_or_empty(path: &std::path::Path) -> serde_json::Value {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }

    fn save(path: &std::path::Path, json: &serde_json::Value) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(json) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Ensure `json[key]` (a `a.b` dotted key) exists as an object; returns
    /// a mutable object ref.
    fn ensure_obj<'a>(
        json: &'a mut serde_json::Value,
        dotted: &str,
    ) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
        let mut cur = json;
        for part in dotted.split('.') {
            let obj = cur.as_object_mut()?;
            cur = obj
                .entry(part.to_string())
                .or_insert_with(|| serde_json::json!({}));
        }
        cur.as_object_mut()
    }

    pub fn write_settings_value(
        path: &std::path::Path,
        dotted: &str,
        value: serde_json::Value,
    ) {
        let mut json = load_or_empty(path);
        if let Some((parent, leaf)) = dotted.rsplit_once('.') {
            if let Some(obj) = ensure_obj(&mut json, parent) {
                obj.insert(leaf.to_string(), value);
                save(path, &json);
            }
        }
    }

    pub fn update_rule_list(
        path: &std::path::Path,
        dotted: &str,
        action: crate::permissions::RuleAction,
        rules: &[String],
    ) {
        use crate::permissions::RuleAction;
        let mut json = load_or_empty(path);
        if let Some((parent, leaf)) = dotted.rsplit_once('.') {
            if let Some(obj) = ensure_obj(&mut json, parent) {
                let entry = obj
                    .entry(leaf.to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = entry.as_array_mut() {
                    match action {
                        RuleAction::Add => {
                            for r in rules {
                                if !arr.iter().any(|v| v.as_str() == Some(r.as_str())) {
                                    arr.push(serde_json::Value::String(r.clone()));
                                }
                            }
                        }
                        RuleAction::Replace => {
                            *arr = rules
                                .iter()
                                .map(|r| serde_json::Value::String(r.clone()))
                                .collect();
                        }
                        RuleAction::Remove => {
                            arr.retain(|v| v.as_str().map(|s| !rules.contains(&s.to_string())).unwrap_or(true));
                        }
                    }
                }
                save(path, &json);
            }
        }
    }

    pub fn update_dir_list(path: &std::path::Path, add: bool, dirs: &[String]) {
        let mut json = load_or_empty(path);
        if let Some(obj) = ensure_obj(&mut json, "permissions") {
            let entry = obj
                .entry("additionalDirectories".to_string())
                .or_insert_with(|| serde_json::json!([]));
            if let Some(arr) = entry.as_array_mut() {
                if add {
                    for d in dirs {
                        if !arr.iter().any(|v| v.as_str() == Some(d.as_str())) {
                            arr.push(serde_json::Value::String(d.clone()));
                        }
                    }
                } else {
                    arr.retain(|v| v.as_str().map(|s| !dirs.contains(&s.to_string())).unwrap_or(true));
                }
            }
            save(path, &json);
        }
    }
}
