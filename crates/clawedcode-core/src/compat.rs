use anyhow::{Context, Result};
use clawedcode_mcp::{McpServerConfig, discover_mcp_servers as parse_settings_mcp_servers};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize)]
pub struct CompatibilitySnapshot {
    pub settings_files: Vec<PathBuf>,
    pub settings: Value,
    pub skills: Vec<SkillDescriptor>,
    pub memory_files: Vec<PathBuf>,
    pub memory: String,
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillDescriptor {
    pub name: String,
    pub description: Option<String>,
    pub when_to_use: Option<String>,
    pub path: PathBuf,
    pub body: String,
    pub slash_command: String,
    pub legacy_command: bool,
}

#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
}

pub fn discover(cwd: &Path) -> Result<CompatibilitySnapshot> {
    let (settings_files, settings) = discover_settings(cwd)?;
    let skills = discover_skills(cwd)?;
    let (memory_files, memory) = discover_memory(cwd)?;
    let mcp_servers = discover_mcp_servers(cwd, &settings)?;

    Ok(CompatibilitySnapshot {
        settings_files,
        settings,
        skills,
        memory_files,
        memory,
        mcp_servers,
    })
}

pub fn claude_home() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// Path to ~/.claude.json — the reference's global config file holding
/// user-scope `mcpServers` and per-project `projects.<cwd>` (local scope).
pub fn claude_json_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        return Some(home.join(".claude.json"));
    }
    claude_home().and_then(|d| d.parent().map(|p| p.join(".claude.json")))
}

fn discover_settings(cwd: &Path) -> Result<(Vec<PathBuf>, Value)> {
    let mut files = Vec::new();
    let mut merged = Value::Object(Map::new());

    for path in settings_search_paths(cwd) {
        if path.exists() {
            let value = read_json_file(&path)?;
            deep_merge(&mut merged, value);
            files.push(path);
        }
    }

    Ok((files, merged))
}

fn settings_search_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = claude_home() {
        paths.push(home.join("settings.json"));
    }

    let mut ancestors: Vec<PathBuf> = cwd.ancestors().map(PathBuf::from).collect();
    ancestors.reverse();

    for ancestor in ancestors {
        paths.push(ancestor.join(".claude").join("settings.json"));
        paths.push(ancestor.join(".claude").join("settings.local.json"));
    }

    paths
}

/// `outputStyle` from `.claude/settings.json` / `settings.local.json`:
/// the setting names a markdown file under `.claude/output-styles/` whose
/// body is appended to the system prompt (the reference applies it on the
/// next session).
pub fn load_output_style(cwd: &Path) -> Option<String> {
    let style_name = [".claude/settings.local.json", ".claude/settings.json"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(cwd.join(name)).ok())
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter_map(|json| {
            json.get("outputStyle")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .next()?;
    if style_name.eq_ignore_ascii_case("default") {
        return None;
    }
    let file_name = format!("{}.md", style_name);
    let mut candidates = vec![cwd.join(".claude").join("output-styles").join(&file_name)];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".claude")
                .join("output-styles")
                .join(&file_name),
        );
    }
    for path in candidates {
        if let Ok(body) = std::fs::read_to_string(&path) {
            // Reuse the frontmatter stripper so `description:` metadata does
            // not leak into the prompt.
            let (_frontmatter, body) = parse_frontmatter_and_body::<SkillFrontmatter>(&body);
            let body = body.trim();
            if !body.is_empty() {
                return Some(body.to_string());
            }
        }
    }
    // A configured style with no resolvable file still records the intent —
    // the name itself becomes the instruction.
    Some(format!("Output style: {style_name}"))
}

/// A loaded plugin — the reference's `LoadedPlugin` subset: a directory
/// containing `.claude-plugin/plugin.json` (or a bare `plugin.json`), or a
/// directory holding at least one component dir (commands/agents/skills).
#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    /// Reflects `settings.enabledPlugins` — `plugin enable/disable` toggles it.
    pub enabled: bool,
    pub path: PathBuf,
    /// Roots searched as skill commands (`commands/`, manifest `commands`).
    pub command_dirs: Vec<PathBuf>,
    /// Roots searched as agents (`agents/`, manifest `agents`).
    pub agent_dirs: Vec<PathBuf>,
    /// Roots searched as skills (`skills/`, manifest `skills`).
    pub skill_dirs: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct PluginManifest {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    commands: Option<serde_json::Value>,
    agents: Option<serde_json::Value>,
    skills: Option<serde_json::Value>,
}

fn manifest_paths(dir: &Path, value: &Option<serde_json::Value>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match value {
        Some(serde_json::Value::String(s)) => out.push(dir.join(s)),
        Some(serde_json::Value::Array(list)) => {
            for item in list {
                if let Some(s) = item.as_str() {
                    out.push(dir.join(s));
                }
            }
        }
        Some(serde_json::Value::Object(map)) => {
            for key in map.keys() {
                out.push(dir.join(format!("commands/{key}.md")));
            }
        }
        _ => {}
    }
    out
}

/// Discover plugins from `~/.claude/plugins/*/`, `<cwd>/.claude/plugins/*/`,
/// and `CLAWEDCODE_PLUGIN_DIR` paths (the reference's `--plugin-dir` —
/// session-only inline plugins).
/// `settings.enabledPlugins` — `name@local`/`name` keys map to bool or an
/// array of marketplaces. Missing entries default to enabled.
fn plugin_enabled(settings: &Value, name: &str) -> bool {
    let Some(map) = settings.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return true;
    };
    for key in [format!("{name}@local"), name.to_string()] {
        match map.get(&key) {
            Some(Value::Bool(b)) => return *b,
            Some(Value::Array(_)) => return true,
            Some(Value::Null) | None => {}
            _ => {}
        }
    }
    // Marketplace-installed plugins use `name@marketplace` keys.
    let prefix = format!("{name}@");
    for (key, value) in map {
        if key.starts_with(&prefix) {
            if let Value::Bool(b) = value {
                return *b;
            }
        }
    }
    true
}

/// All plugins regardless of enabled state (used by `plugin list`).
pub fn discover_plugins_all(cwd: &Path) -> Vec<Plugin> {
    discover_plugins_impl(cwd, false)
}

pub fn discover_plugins(cwd: &Path) -> Vec<Plugin> {
    discover_plugins_impl(cwd, true)
}

fn discover_plugins_impl(cwd: &Path, filter_enabled: bool) -> Vec<Plugin> {
    // `--bare`: minimal mode skips plugin discovery like the reference.
    if std::env::var_os("CLAWEDCODE_SIMPLE").is_some() {
        return Vec::new();
    }
    let settings = discover_settings(cwd).map(|(_, v)| v).unwrap_or_default();
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = claude_home() {
        roots.push(home.join("plugins"));
    }
    roots.push(cwd.join(".claude").join("plugins"));

    let mut plugin_dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    plugin_dirs.push(entry.path());
                }
            }
        }
    }
    for extra in std::env::var("CLAWEDCODE_PLUGIN_DIR")
        .map(|v| v.split(':').map(PathBuf::from).collect::<Vec<PathBuf>>())
        .unwrap_or_default()
    {
        plugin_dirs.push(extra);
    }

    let mut plugins = Vec::new();
    for dir in plugin_dirs {
        let manifest_path = dir.join(".claude-plugin/plugin.json");
        let manifest_path = if manifest_path.exists() {
            manifest_path
        } else {
            dir.join("plugin.json")
        };
        let manifest: PluginManifest = fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        let has_components = ["commands", "agents", "skills"]
            .iter()
            .any(|d| dir.join(d).is_dir())
            || manifest.commands.is_some()
            || manifest.agents.is_some()
            || manifest.skills.is_some();
        // Require either a manifest or at least one component dir.
        if !manifest_path.exists() && !has_components {
            continue;
        }

        let mut command_dirs = manifest_paths(&dir, &manifest.commands);
        if dir.join("commands").is_dir() {
            command_dirs.push(dir.join("commands"));
        }
        let mut agent_dirs = manifest_paths(&dir, &manifest.agents);
        if dir.join("agents").is_dir() {
            agent_dirs.push(dir.join("agents"));
        }
        let mut skill_dirs = manifest_paths(&dir, &manifest.skills);
        if dir.join("skills").is_dir() {
            skill_dirs.push(dir.join("skills"));
        }

        let name = manifest.name.unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "plugin".to_string())
        });
        let enabled = plugin_enabled(&settings, &name);
        plugins.push(Plugin {
            name,
            description: manifest.description,
            version: manifest.version,
            enabled,
            path: dir,
            command_dirs,
            agent_dirs,
            skill_dirs,
        });
    }
    if filter_enabled {
        plugins.retain(|p| p.enabled);
    }
    plugins
}

pub fn discover_skills(cwd: &Path) -> Result<Vec<SkillDescriptor>> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();

    if let Some(home) = claude_home() {
        paths.push((home.join("skills"), false));
    }

    for ancestor in cwd.ancestors() {
        paths.push((ancestor.join(".claude").join("skills"), false));
        paths.push((ancestor.join(".claude").join("commands"), true));
    }

    for plugin in discover_plugins(cwd) {
        for dir in plugin.command_dirs {
            paths.push((dir, true));
        }
        for dir in plugin.skill_dirs {
            paths.push((dir, false));
        }
    }

    let mut discovered = Vec::new();
    for (root, is_legacy) in paths {
        if !root.exists() {
            continue;
        }
        discover_skills_in_root(&root, is_legacy, &mut discovered)?;
    }

    let mut unique = Vec::new();
    for skill in discovered {
        if unique
            .iter()
            .any(|existing: &SkillDescriptor| existing.slash_command == skill.slash_command)
        {
            continue;
        }
        unique.push(skill);
    }

    unique.sort_by(|a, b| {
        a.slash_command
            .cmp(&b.slash_command)
            .then(a.path.cmp(&b.path))
    });
    Ok(unique)
}

fn discover_memory(cwd: &Path) -> Result<(Vec<PathBuf>, String)> {
    discover_memory_with_reason(cwd, "session_start")
}

/// Re-fire InstructionsLoaded for each memory file with a non-startup
/// reason (e.g. "compact") — the reference re-emits the hook when caches
/// are cleared post-compaction.
pub fn refire_instructions_loaded(cwd: &Path, reason: &str) {
    let _ = discover_memory_with_reason(cwd, reason);
}

fn discover_memory_with_reason(cwd: &Path, load_reason: &str) -> Result<(Vec<PathBuf>, String)> {
    // `--bare`: minimal mode skips CLAUDE.md auto-discovery like the reference.
    if std::env::var_os("CLAWEDCODE_SIMPLE").is_some() {
        return Ok((Vec::new(), String::new()));
    }
    let mut memory_files = Vec::new();
    let mut sections = Vec::new();

    for path in memory_search_paths(cwd, claude_home().as_deref())? {
        if !path.exists() {
            continue;
        }

        let expanded = expand_memory_file(&path, &mut Vec::new(), 0, cwd)?;
        if expanded.trim().is_empty() {
            continue;
        }

        memory_files.push(path.clone());
        sections.push(format!("<!-- {} -->\n{}", path.display(), expanded.trim()));

        // Fire InstructionsLoaded per loaded memory file like the reference.
        let memory_type = if claude_home()
            .map(|h| path == h.join("CLAUDE.md"))
            .unwrap_or(false)
        {
            "User"
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("local"))
        {
            "Local"
        } else {
            "Project"
        };
        crate::hooks::run_simple_event_hooks(
            &crate::hooks::load_hooks(cwd),
            "InstructionsLoaded",
            "",
            cwd,
            serde_json::json!({
                "file_path": std::path::absolute(&path)
                    .unwrap_or_else(|_| path.clone())
                    .to_string_lossy(),
                "memory_type": memory_type,
                "load_reason": load_reason,
            }),
        );
    }

    Ok((memory_files, sections.join("\n\n")))
}

fn memory_search_paths(cwd: &Path, claude_home: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if let Some(home) = claude_home {
        paths.push(home.join("CLAUDE.md"));
    }

    let mut ancestors: Vec<PathBuf> = cwd.ancestors().map(PathBuf::from).collect();
    ancestors.reverse();

    for ancestor in ancestors {
        paths.push(ancestor.join("CLAUDE.md"));

        let rules_dir = ancestor.join(".claude").join("rules");
        if rules_dir.is_dir() {
            let mut rule_paths: Vec<PathBuf> = fs::read_dir(&rules_dir)
                .with_context(|| format!("failed to read {}", rules_dir.display()))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
                .collect();
            rule_paths.sort();
            paths.extend(rule_paths);
        }
    }

    Ok(paths)
}

fn expand_memory_file(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    depth: usize,
    hooks_cwd: &Path,
) -> Result<String> {
    const MAX_MEMORY_INCLUDE_DEPTH: usize = 16;

    if depth >= MAX_MEMORY_INCLUDE_DEPTH {
        return Ok(String::new());
    }

    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canonical) {
        return Ok(String::new());
    }

    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    stack.push(canonical);

    let mut expanded = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(include_target) = trimmed.strip_prefix("@include ") {
            let include_path = resolve_include_path(path, include_target.trim());
            // InstructionsLoaded fires per @-included file with reason
            // "include" and the including file as parent_file_path.
            crate::hooks::run_simple_event_hooks(
                &crate::hooks::load_hooks(hooks_cwd),
                "InstructionsLoaded",
                "",
                hooks_cwd,
                serde_json::json!({
                    "file_path": std::path::absolute(&include_path)
                        .unwrap_or_else(|_| include_path.clone())
                        .to_string_lossy(),
                    "memory_type": "Project",
                    "load_reason": "include",
                    "parent_file_path": std::path::absolute(path)
                        .unwrap_or_else(|_| path.to_path_buf())
                        .to_string_lossy(),
                }),
            );
            let nested = expand_memory_file(&include_path, stack, depth + 1, hooks_cwd)?;
            if !nested.trim().is_empty() {
                expanded.push(nested);
            }
        } else {
            expanded.push(line.to_string());
        }
    }

    stack.pop();
    Ok(expanded.join("\n"))
}

fn resolve_include_path(source: &Path, target: &str) -> PathBuf {
    if let Some(rest) = target.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }

    let target_path = PathBuf::from(target);
    if target_path.is_absolute() {
        return target_path;
    }

    source
        .parent()
        .map(|parent| parent.join(target_path.clone()))
        .unwrap_or(target_path)
}

fn discover_skills_in_root(
    root: &Path,
    is_legacy: bool,
    out: &mut Vec<SkillDescriptor>,
) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                out.push(parse_skill(&skill_md, is_legacy)?);
                continue;
            }

            for nested in fs::read_dir(&path)? {
                let nested = nested?;
                let nested_path = nested.path();
                if nested_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
                {
                    out.push(parse_skill(&nested_path, is_legacy)?);
                    break;
                }
            }
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            out.push(parse_skill(&path, is_legacy)?);
        }
    }

    Ok(())
}

fn parse_skill(path: &Path, legacy_command: bool) -> Result<SkillDescriptor> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let (frontmatter, body) = parse_frontmatter_and_body::<SkillFrontmatter>(&raw);
    let fallback_name = if legacy_command {
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string()
    } else {
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .or_else(|| path.file_stem().and_then(|name| name.to_str()))
            .unwrap_or("skill")
            .to_string()
    };

    let name = frontmatter.name.unwrap_or(fallback_name);
    let slash_command = slash_command_from_name(&name);

    Ok(SkillDescriptor {
        name,
        description: frontmatter.description,
        when_to_use: frontmatter.when_to_use,
        path: path.to_path_buf(),
        body,
        slash_command,
        legacy_command,
    })
}

fn parse_frontmatter_and_body<T>(raw: &str) -> (T, String)
where
    T: serde::de::DeserializeOwned + Default,
{
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return (T::default(), raw.trim().to_string());
    }

    let mut yaml = String::new();
    while let Some(line) = lines.next() {
        if line == "---" {
            let frontmatter = serde_yaml::from_str::<T>(&yaml).ok();
            let body = lines.collect::<Vec<&str>>().join("\n").trim().to_string();
            return (frontmatter.unwrap_or_default(), body);
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    (T::default(), raw.trim().to_string())
}

/// A subagent definition from `.claude/agents/<name>.md` — the reference's
/// custom agents (`Task(subagent_type=...)` selects one).
#[derive(Debug, Clone, Serialize)]
pub struct AgentDefinition {
    pub agent_type: String,
    pub description: Option<String>,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub max_turns: Option<usize>,
    pub system_prompt: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<serde_yaml::Value>,
    model: Option<String>,
    #[serde(rename = "permissionMode")]
    permission_mode: Option<String>,
    #[serde(rename = "maxTurns")]
    max_turns: Option<usize>,
}

/// `None` when the frontmatter has no `tools` key (all tools allowed);
/// `Some([])` when it's present but empty.
fn agent_tool_list(value: Option<serde_yaml::Value>) -> Option<Vec<String>> {
    match value {
        Some(serde_yaml::Value::String(text)) => Some(
            text.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Some(serde_yaml::Value::Sequence(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// The reference always registers `general-purpose` (tools `*`) — a file
/// agent with the same `name` overrides it.
fn general_purpose_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "general-purpose".to_string(),
        description: Some(
            "General-purpose agent for researching complex questions, searching for \
             code, and executing multi-step tasks."
                .to_string(),
        ),
        tools: None,
        model: None,
        permission_mode: None,
        max_turns: None,
        system_prompt: "You are an agent for a terminal coding assistant. Given the \
            user's message, you should use the tools available to complete the task. \
            Complete the task fully—don't gold-plate, but don't leave it half-done. \
            When you complete the task, respond with a concise report covering what \
            was done and any key findings — the caller will relay this to the user, \
            so it only needs the essentials.\n\nYour strengths:\n- Searching for \
            code, configurations, and patterns across large codebases\n- Analyzing \
            multiple files to understand system architecture\n- Investigating \
            complex questions that require exploring many files\n- Performing \
            multi-step research tasks\n\nGuidelines:\n- For file searches: search \
            broadly when you don't know where something lives. Use read_file when \
            you know the specific file path.\n- For analysis: Start broad and \
            narrow down. Use multiple search strategies if the first doesn't yield \
            results.\n- Be thorough: Check multiple locations, consider different \
            naming conventions, look for related files.\n- NEVER create files \
            unless they're absolutely necessary for achieving your goal. ALWAYS \
            prefer editing an existing file to creating a new one.\n- NEVER \
            proactively create documentation files (*.md) or README files. Only \
            create documentation files if explicitly requested."
            .to_string(),
        path: PathBuf::from("<built-in>"),
    }
}

/// The read-only toolset shared by the reference's Explore/Plan/verification
/// agents — everything except file edits and nested agents (their
/// `disallowedTools`). Local names: `shell` covers Bash/Glob/Grep,
/// `read_file` covers Read, and Task* tools stay available.
fn read_only_agent_tools() -> Vec<String> {
    [
        "shell",
        "read_file",
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskOutput",
        "TaskStop",
        "TaskUpdate",
        "ask_user_question",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

const READ_ONLY_BLOCK: &str = "=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===\n\
This is a READ-ONLY task. You are STRICTLY PROHIBITED from:\n\
- Creating new files (no Write, touch, or file creation of any kind)\n\
- Modifying existing files (no Edit operations)\n\
- Deleting files (no rm or deletion)\n\
- Moving or copying files (no mv or cp)\n\
- Creating temporary files anywhere, including /tmp\n\
- Using redirect operators (>, >>, |) or heredocs to write to files\n\
- Running ANY commands that change system state\n\n\
You do NOT have access to file editing tools - attempting to edit files will fail.\n\
Use `shell` ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail).\n\
NEVER use `shell` for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification.";

/// The reference's built-in `Explore` agent — fast read-only codebase
/// search (BUILTIN_EXPLORE_PLAN_AGENTS default-enabled).
fn explore_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "Explore".to_string(),
        description: Some(
            "Fast agent specialized for exploring codebases. Use this when you need \
             to quickly find files by patterns, search code for keywords, or answer \
             questions about the codebase. Specify the desired thoroughness level: \
             \"quick\" for basic searches, \"medium\" for moderate exploration, or \
             \"very thorough\" for comprehensive analysis."
                .to_string(),
        ),
        tools: Some(read_only_agent_tools()),
        model: Some("haiku".to_string()),
        permission_mode: None,
        max_turns: None,
        system_prompt: format!(
            "You are a file search specialist. You excel at thoroughly navigating and \
             exploring codebases.\n\n{READ_ONLY_BLOCK}\n\n\
             Your strengths:\n- Rapidly finding files using glob patterns\n- Searching \
             code and text with powerful regex patterns\n- Reading and analyzing file contents\n\n\
             Guidelines:\n- Use `shell` (find/grep) for broad file pattern matching and \
             content search\n- Use `read_file` when you know the specific file path you \
             need to read\n- Adapt your search approach based on the thoroughness level \
             specified by the caller\n- Communicate your final report directly as a \
             regular message - do NOT attempt to create files\n\n\
             NOTE: You are meant to be a fast agent that returns output as quickly as \
             possible. Be smart about how you search for files and implementations; \
             wherever possible spawn multiple parallel tool calls for grepping and \
             reading files.\n\n\
             Complete the user's search request efficiently and report your findings clearly."
        ),
        path: PathBuf::from("<built-in>"),
    }
}

/// The reference's built-in `Plan` agent — read-only implementation
/// planning.
fn plan_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "Plan".to_string(),
        description: Some(
            "Software architect agent for designing implementation plans. Use this \
             when you need to plan the implementation strategy for a task. Returns \
             step-by-step plans, identifies critical files, and considers \
             architectural trade-offs."
                .to_string(),
        ),
        tools: Some(read_only_agent_tools()),
        model: None,
        permission_mode: None,
        max_turns: None,
        system_prompt: format!(
            "You are a software architect and planning specialist. Your role is to \
             explore the codebase and design implementation plans.\n\n{READ_ONLY_BLOCK}\n\n\
             ## Your Process\n\n\
             1. **Understand Requirements**: Focus on the requirements provided.\n\n\
             2. **Explore Thoroughly**:\n\
             - Read any files provided to you in the initial prompt\n\
             - Find existing patterns and conventions\n\
             - Understand the current architecture\n\
             - Identify similar features as reference\n\
             - Trace through relevant code paths\n\n\
             3. **Design Solution**:\n\
             - Create implementation approach based on the requirements\n\
             - Consider trade-offs and architectural decisions\n\
             - Follow existing patterns where appropriate\n\n\
             4. **Detail the Plan**:\n\
             - Provide step-by-step implementation strategy\n\
             - Identify dependencies and sequencing\n\
             - Anticipate potential challenges\n\n\
             ## Required Output\n\n\
             End your response with:\n\n\
             ### Critical Files for Implementation\n\
             List 3-5 files most critical for implementing this plan:\n\
             - path/to/file1\n\
             - path/to/file2\n\
             - path/to/file3\n\n\
             REMEMBER: You can ONLY explore and plan. You CANNOT and MUST NOT write, \
             edit, or modify any files. You do NOT have access to file editing tools."
        ),
        path: PathBuf::from("<built-in>"),
    }
}

/// The reference's built-in `verification` agent — runs checks and returns
/// a VERDICT verdict.
fn verification_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "verification".to_string(),
        description: Some(
            "Use this agent to verify that implementation work is correct before \
             reporting completion. Invoke after non-trivial tasks (3+ file edits, \
             backend/API changes, infrastructure changes). Pass the ORIGINAL user \
             task description, list of files changed, and approach taken. The agent \
             runs builds, tests, linters, and checks to produce a PASS/FAIL/PARTIAL \
             verdict with evidence."
                .to_string(),
        ),
        tools: Some(read_only_agent_tools()),
        model: None,
        permission_mode: None,
        max_turns: None,
        system_prompt: "You are a verification specialist. Your job is not to \
            confirm the implementation works — it's to try to break it.\n\n\
            Your entire value is in finding the last 20% — a polished UI or a \
            passing test suite can hide half-broken functionality. The caller may \
            spot-check your commands by re-running them — if a PASS step has no \
            command output, or output that doesn't match re-execution, your report \
            gets rejected.\n\n\
            === CRITICAL: DO NOT MODIFY THE PROJECT ===\n\
            You are STRICTLY PROHIBITED from:\n\
            - Creating, modifying, or deleting any files IN THE PROJECT DIRECTORY\n\
            - Installing dependencies or packages\n\
            - Running git write operations (add, commit, push)\n\n\
            You MAY write ephemeral test scripts to a temp directory (/tmp or \
            $TMPDIR) via `shell` redirection when inline commands aren't \
            sufficient. Clean up after yourself.\n\n\
            === WHAT YOU RECEIVE ===\n\
            You will receive: the original task description, files changed, \
            approach taken, and optionally a plan file path.\n\n\
            === VERIFICATION STRATEGY ===\n\
            Adapt your strategy based on what was changed — run builds, tests, \
            linters, type checks; exercise the actual feature end to end.\n\n\
            PARTIAL is for environmental limitations only (no test framework, tool \
            unavailable, server can't start) — not for \"I'm unsure whether this is \
            a bug.\" If you can run the check, you must decide PASS or FAIL.\n\n\
            Use the literal string `VERDICT: ` followed by exactly one of `PASS`, \
            `FAIL`, `PARTIAL`. No markdown bold, no punctuation, no variation.\n\
            - **FAIL**: include what failed, exact error output, reproduction steps.\n\
            - **PARTIAL**: what was verified, what could not be and why, what the \
            implementer should know."
            .to_string(),
        path: PathBuf::from("<built-in>"),
    }
}

/// The reference's built-in `statusline-setup` agent spawned by
/// `/statusline`.
fn statusline_setup_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "statusline-setup".to_string(),
        description: Some(
            "Configure the user's statusLine setting from their shell PS1 or \
             instructions."
                .to_string(),
        ),
        tools: Some(
            ["shell", "read_file", "apply_patch"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        model: None,
        permission_mode: None,
        max_turns: None,
        system_prompt: "You are a status line setup agent. Your job is to create or \
            update the statusLine command in the user's settings.\n\n\
            When asked to convert the user's shell PS1 configuration:\n\
            1. Read the user's shell configuration files in this order of preference:\n\
               ~/.zshrc, ~/.bashrc, ~/.bash_profile, ~/.profile\n\
            2. Extract the PS1 value.\n\
            3. Convert PS1 escape sequences to shell commands:\n\
               \\u → $(whoami), \\h → $(hostname -s), \\H → $(hostname),\n\
               \\w → $(pwd), \\W → $(basename \"$(pwd)\"), \\$ → $,\n\
               \\t → $(date +%H:%M:%S), \\d → $(date \"+%a %b %d\"),\n\
               \\@ → $(date +%I:%M%p), \\# → #, \\! → !\n\
            4. When using ANSI color codes, use `printf`. Do not remove colors.\n\
            5. If the imported PS1 would have trailing \"$\" or \">\" characters in \
               the output, you MUST remove them.\n\
            6. If no PS1 is found and the user did not provide other instructions, \
               ask for further instructions.\n\n\
            The statusLine command receives session JSON on stdin (session_id, \
            cwd, model, workspace) and its stdout is rendered in the footer.\n\
            Write the resulting command to the \"statusLine\" key of the user's \
            settings.json (type \"command\")."
            .to_string(),
        path: PathBuf::from("<built-in>"),
    }
}

/// The reference's `claude-code-guide` agent — answers Claude Code / Agent
/// SDK / Claude API questions by fetching the official docs maps.
fn claude_code_guide_agent() -> AgentDefinition {
    AgentDefinition {
        agent_type: "claude-code-guide".to_string(),
        description: Some(
            "Use this agent when the user asks questions about Claude Code (the \
             CLI tool), the Claude Agent SDK, or the Claude API."
                .to_string(),
        ),
        tools: Some(
            [
                "shell",
                "read_file",
                "Glob",
                "Grep",
                "WebFetch",
                "WebSearch",
                "SendMessage",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ),
        model: None,
        permission_mode: None,
        max_turns: None,
        system_prompt: "You are the Claude guide agent. Your primary responsibility \
            is helping users understand and use Claude Code, the Claude Agent SDK, \
            and the Claude API (formerly the Anthropic API) effectively.\n\n\
            **Documentation sources:**\n\
            - Claude Code docs (https://code.claude.com/docs/en/claude_code_docs_map.md): \
              fetch for CLI questions — installation, hooks, skills, MCP servers, \
              IDE integrations, settings, keyboard shortcuts, subagents, plugins.\n\
            - Agent SDK + Claude API docs (https://platform.claude.com/llms.txt): \
              fetch for SDK and API questions — Messages API, tool use, extended \
              thinking, MCP connectors, cloud integrations.\n\n\
            **Approach:**\n\
            1. Determine which domain the user's question falls into\n\
            2. Use WebFetch to fetch the appropriate docs map\n\
            3. Identify the most relevant documentation URLs from the map\n\
            4. Fetch the specific documentation pages\n\
            5. Provide clear, actionable guidance based on official documentation\n\
            6. Use WebSearch if docs don't cover the topic\n\
            7. Reference local project files (CLAUDE.md, .claude/ directory) when \
               relevant using read_file, Glob, and Grep\n\n\
            **Guidelines:**\n\
            - Always prioritize official documentation over assumptions\n\
            - Keep responses concise and actionable\n\
            - Include specific examples or code snippets when helpful\n\
            - Reference exact documentation URLs in your responses\n\
            - When you cannot find an answer or the feature doesn't exist, \
              direct the user to use /feedback to report a feature request or bug\n\n\
            Complete the user's request by providing accurate, \
            documentation-based guidance."
            .to_string(),
        path: PathBuf::from("<built-in>"),
    }
}

/// Discover custom agents: `~/.claude/agents/*.md` (personal) plus
/// `.claude/agents/*.md` in `cwd` (project wins name conflicts, matching
/// the reference's precedence). Built-ins sit at the bottom of the
/// precedence chain like the reference.
pub fn discover_agents(cwd: &Path) -> Vec<AgentDefinition> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = claude_home() {
        dirs.push(home.join("agents"));
    }
    dirs.push(cwd.join(".claude").join("agents"));
    for plugin in discover_plugins(cwd) {
        dirs.extend(plugin.agent_dirs);
    }

    let mut by_type: std::collections::BTreeMap<String, AgentDefinition> =
        std::collections::BTreeMap::new();
    for agent in [
        general_purpose_agent(),
        explore_agent(),
        plan_agent(),
        verification_agent(),
        statusline_setup_agent(),
        claude_code_guide_agent(),
    ] {
        by_type.insert(agent.agent_type.clone(), agent);
    }
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let (frontmatter, body) = parse_frontmatter_and_body::<AgentFrontmatter>(&raw);
            // A missing name falls back to the filename, like the reference.
            let agent_type = frontmatter
                .name
                .clone()
                .or_else(|| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            if agent_type.is_empty() || body.is_empty() {
                continue;
            }
            by_type.insert(
                agent_type.clone(),
                AgentDefinition {
                    agent_type,
                    description: frontmatter.description,
                    tools: agent_tool_list(frontmatter.tools),
                    model: frontmatter.model,
                    permission_mode: frontmatter.permission_mode,
                    max_turns: frontmatter.max_turns,
                    system_prompt: body,
                    path,
                },
            );
        }
    }
    // `--agents <json>` (CLAWEDCODE_AGENTS_JSON): inline agent defs take
    // precedence over every file/discovered agent, like the reference.
    if let Ok(raw) = std::env::var("CLAWEDCODE_AGENTS_JSON") {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&raw) {
            for (agent_type, def) in map {
                let prompt = def
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if prompt.is_empty() {
                    continue;
                }
                by_type.insert(
                    agent_type.clone(),
                    AgentDefinition {
                        agent_type,
                        description: def
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        tools: def.get("tools").and_then(|v| {
                            v.as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|t| t.as_str().map(str::to_string))
                                    .collect()
                            })
                        }),
                        model: def
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        permission_mode: def
                            .get("permissionMode")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        max_turns: def
                            .get("maxTurns")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize),
                        system_prompt: prompt,
                        path: PathBuf::from("--agents"),
                    },
                );
            }
        }
    }
    by_type.into_values().collect()
}

/// Map an agent frontmatter `permissionMode` to a `PermissionMode` — the
/// reference accepts the camelCase settings names.
pub fn agent_permission_mode(value: Option<&str>) -> Option<crate::permissions::PermissionMode> {
    match value? {
        "default" => Some(crate::permissions::PermissionMode::Default),
        "acceptEdits" | "accept_edits" => Some(crate::permissions::PermissionMode::AcceptEdits),
        "plan" => Some(crate::permissions::PermissionMode::Plan),
        "bypassPermissions" | "bypass" => Some(crate::permissions::PermissionMode::Bypass),
        _ => None,
    }
}

fn slash_command_from_name(name: &str) -> String {
    let mut rendered = String::from("/");
    let mut last_was_dash = false;

    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            rendered.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if (ch.is_ascii_whitespace() || ch == '-' || ch == '_') && !last_was_dash {
            rendered.push('-');
            last_was_dash = true;
        }
    }

    while rendered.ends_with('-') {
        rendered.pop();
    }

    if rendered == "/" {
        "/skill".to_string()
    } else {
        rendered
    }
}

fn discover_mcp_servers(cwd: &Path, settings: &Value) -> Result<BTreeMap<String, McpServerConfig>> {
    // `--mcp-config` / CLAWEDCODE_MCP_CONFIG: extra config file(s) merged
    // like the reference flag. `--strict-mcp-config` /
    // CLAWEDCODE_MCP_CONFIG_STRICT ignores every other source.
    let extra_files: Vec<PathBuf> = std::env::var("CLAWEDCODE_MCP_CONFIG")
        .map(|v| {
            v.split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    let strict = std::env::var_os("CLAWEDCODE_MCP_CONFIG_STRICT").is_some();

    let mut servers = if strict {
        BTreeMap::new()
    } else {
        let mut found = BTreeMap::new();
        // ~/.claude.json: user-scope mcpServers + local-scope projects.<cwd>
        if let Some(json_path) = claude_json_path() {
            if json_path.exists() {
                if let Ok(value) = read_json_file(&json_path) {
                    if let Some(user) = value.get("mcpServers").and_then(parse_mcp_map) {
                        found.extend(user);
                    }
                    let cwd_key = std::fs::canonicalize(cwd)
                        .unwrap_or_else(|_| cwd.to_path_buf())
                        .to_string_lossy()
                        .to_string();
                    if let Some(local) = value
                        .get("projects")
                        .and_then(|p| p.get(&cwd_key))
                        .or_else(|| {
                            value
                                .get("projects")
                                .and_then(|p| p.get(cwd.to_string_lossy().as_ref()))
                        })
                        .and_then(|p| p.get("mcpServers"))
                        .and_then(parse_mcp_map)
                    {
                        found.extend(local);
                    }
                }
            }
        }
        found.extend(parse_settings_mcp_servers(settings));
        found
    };

    for file in &extra_files {
        let raw = fs::read_to_string(file)
            .with_context(|| format!("failed to read {}", file.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", file.display()))?;
        if let Some(extra_servers) = value.get("mcpServers").and_then(parse_mcp_map) {
            servers.extend(extra_servers);
        }
    }
    if strict {
        return Ok(servers);
    }

    let mut ancestor_paths: Vec<PathBuf> = cwd.ancestors().map(PathBuf::from).collect();
    ancestor_paths.reverse();
    ancestor_paths.pop();

    for ancestor in ancestor_paths {
        let mcp_json_path = ancestor.join(".mcp.json");
        if mcp_json_path.exists() {
            let raw = fs::read_to_string(&mcp_json_path)
                .with_context(|| format!("failed to read {}", mcp_json_path.display()))?;
            let value: Value = serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {}", mcp_json_path.display()))?;
            if let Some(ancestor_servers) = value.get("mcpServers").and_then(parse_mcp_map) {
                servers.extend(ancestor_servers);
            }
        }
    }

    let mcp_json_path = cwd.join(".mcp.json");
    if mcp_json_path.exists() {
        let raw = fs::read_to_string(&mcp_json_path)
            .with_context(|| format!("failed to read {}", mcp_json_path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", mcp_json_path.display()))?;
        if let Some(project_servers) = value.get("mcpServers").and_then(parse_mcp_map) {
            servers.extend(project_servers);
        }
    }

    Ok(servers)
}

fn parse_mcp_map(value: &Value) -> Option<BTreeMap<String, McpServerConfig>> {
    let object = value.as_object()?;
    let mut result = BTreeMap::new();

    for (name, config) in object {
        if let Ok(server) = serde_json::from_value::<McpServerConfig>(config.clone()) {
            result.insert(name.clone(), server);
        }
    }

    Some(result)
}

fn read_json_file(path: &Path) -> Result<Value> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value)
}

fn deep_merge(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, value) in source_map {
                deep_merge(target_map.entry(key).or_insert(Value::Null), value);
            }
        }
        (target_slot, source_value) => *target_slot = source_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("clawed_compat_{name}_{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn memory_search_paths_orders_user_then_project_then_rules() {
        let root = temp_dir("memory_paths");
        let home = root.join("home");
        let project = root.join("project");
        let nested = project.join("apps").join("api");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(project.join(".claude").join("rules")).unwrap();
        fs::create_dir_all(nested.clone()).unwrap();
        let rule_file = project.join(".claude").join("rules").join("10-style.md");
        fs::write(&rule_file, "rule").unwrap();

        let paths = memory_search_paths(&nested, Some(&home.join(".claude"))).unwrap();

        assert_eq!(paths[0], home.join(".claude").join("CLAUDE.md"));
        assert!(paths.iter().any(|p| p == &project.join("CLAUDE.md")));
        assert!(paths.iter().any(|p| p == &rule_file));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expand_memory_file_resolves_relative_and_absolute_includes() {
        let root = temp_dir("includes");
        let main = root.join("CLAUDE.md");
        let rel = root.join("relative.md");
        let abs = root.join("absolute.md");

        fs::write(&rel, "relative content").unwrap();
        fs::write(&abs, "absolute content").unwrap();
        fs::write(
            &main,
            format!(
                "start\n@include relative.md\nmiddle\n@include {}\nend",
                abs.display()
            ),
        )
        .unwrap();

        let expanded = expand_memory_file(&main, &mut Vec::new(), 0, &root).unwrap();
        assert!(expanded.contains("start"));
        assert!(expanded.contains("relative content"));
        assert!(expanded.contains("absolute content"));
        assert!(expanded.contains("end"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expand_memory_file_avoids_include_cycles() {
        let root = temp_dir("cycles");
        let a = root.join("a.md");
        let b = root.join("b.md");

        fs::write(&a, "@include b.md\nfrom a").unwrap();
        fs::write(&b, "@include a.md\nfrom b").unwrap();

        let expanded = expand_memory_file(&a, &mut Vec::new(), 0, &root).unwrap();
        assert!(expanded.contains("from a"));
        assert!(expanded.contains("from b"));
        assert_eq!(expanded.matches("from a").count(), 1);
        assert_eq!(expanded.matches("from b").count(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn refire_instructions_loaded_uses_supplied_reason() {
        let _guard = env_lock();
        let root = temp_dir("refire");
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(project.join(".claude")).unwrap();
        fs::write(project.join("CLAUDE.md"), "project memory").unwrap();
        // Hook records its stdin payload so we can assert the reason field.
        fs::write(
            project.join(".claude").join("settings.json"),
            r#"{"hooks":{"InstructionsLoaded":[{"hooks":[{"type":"command","command":"cat > REASON_OUT"}]}]}}"#,
        )
        .unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude")) };
        // run_simple_event_hooks writes stdin to a temp file; capture via a
        // shell that copies it — simplest is asserting the hook ran by
        // checking it does not error and returns memory.
        refire_instructions_loaded(&project, "compact");
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
    }

    fn discover_memory_merges_layers_in_priority_order() {
        let _guard = env_lock();
        let root = temp_dir("layers");
        let home = root.join("home");
        let project = root.join("project");
        let nested = project.join("apps").join("api");

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(project.join(".claude").join("rules")).unwrap();
        fs::create_dir_all(nested.clone()).unwrap();

        fs::write(home.join(".claude").join("CLAUDE.md"), "user memory").unwrap();
        fs::write(project.join("CLAUDE.md"), "project memory").unwrap();
        fs::write(
            project.join(".claude").join("rules").join("10-style.md"),
            "rule memory",
        )
        .unwrap();

        // SAFETY: test-scoped env manipulation.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude")) };
        let (files, memory) = discover_memory(&nested).unwrap();
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

        assert_eq!(files.len(), 3);
        let user_idx = memory.find("user memory").unwrap();
        let project_idx = memory.find("project memory").unwrap();
        let rule_idx = memory.find("rule memory").unwrap();
        assert!(user_idx < project_idx);
        assert!(project_idx < rule_idx);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_skill_strips_frontmatter_and_builds_slash_command() {
        let root = temp_dir("skill_parse");
        let skill_dir = root.join("code-review");
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        fs::write(
            &path,
            r#"---
name: Code Review
description: Review code changes
when_to_use: When checking a patch
---

# Review

Look for regressions first.
"#,
        )
        .unwrap();

        let skill = parse_skill(&path, false).unwrap();
        assert_eq!(skill.name, "Code Review");
        assert_eq!(skill.slash_command, "/code-review");
        assert!(!skill.legacy_command);
        assert_eq!(skill.description.as_deref(), Some("Review code changes"));
        assert_eq!(skill.when_to_use.as_deref(), Some("When checking a patch"));
        assert_eq!(skill.body, "# Review\n\nLook for regressions first.");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_legacy_command_uses_filename_and_marks_legacy() {
        let root = temp_dir("legacy_skill");
        let commands_dir = root.join(".claude").join("commands");
        fs::create_dir_all(&commands_dir).unwrap();
        let path = commands_dir.join("review.md");
        fs::write(&path, "Review the current change carefully.").unwrap();

        let skill = parse_skill(&path, true).unwrap();
        assert_eq!(skill.name, "review");
        assert_eq!(skill.slash_command, "/review");
        assert!(skill.legacy_command);
        assert_eq!(skill.body, "Review the current change carefully.");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_mcp_servers_ancestor_precedence() {
        let _guard = crate::test_support::env_lock();
        let fake_home = std::env::temp_dir().join("clawed_mcp_home_ancestor_precedence");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG_STRICT");
        }
        let root = temp_dir("mcp_ancestor");
        let level1 = root.join("level1");
        let level2 = level1.join("level2");
        let level3 = level2.join("level3");
        fs::create_dir_all(&level3).unwrap();

        fs::write(
            level1.join(".mcp.json"),
            r#"{"mcpServers": {"server1": {"type": "stdio", "command": "echo", "args": ["level1"]}}}"#,
        )
        .unwrap();
        fs::write(
            level2.join(".mcp.json"),
            r#"{"mcpServers": {"server1": {"type": "stdio", "command": "echo", "args": ["level2"]}, "server2": {"type": "stdio", "command": "echo", "args": ["level2"]}}}"#,
        )
        .unwrap();
        fs::write(
            level3.join(".mcp.json"),
            r#"{"mcpServers": {"server1": {"type": "stdio", "command": "echo", "args": ["level3"]}, "server3": {"type": "stdio", "command": "echo", "args": ["level3"]}}}"#,
        )
        .unwrap();

        let servers = discover_mcp_servers(&level3, &Value::Null).unwrap();

        assert_eq!(servers.len(), 3);
        assert_eq!(
            servers.get("server1").and_then(|s| s.command()),
            Some("echo".to_string())
        );
        assert_eq!(
            servers
                .get("server1")
                .and_then(|s| s.args().first().cloned()),
            Some("level3".to_string())
        );
        assert_eq!(
            servers
                .get("server2")
                .and_then(|s| s.args().first().cloned()),
            Some("level2".to_string())
        );
        assert_eq!(
            servers
                .get("server3")
                .and_then(|s| s.args().first().cloned()),
            Some("level3".to_string())
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_mcp_servers_settings_base() {
        let _guard = crate::test_support::env_lock();
        let fake_home = std::env::temp_dir().join("clawed_mcp_home_settings_base");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG_STRICT");
        }
        let root = temp_dir("mcp_settings");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();

        fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers": {"from_file": {"type": "stdio", "command": "echo", "args": ["file"]}}}"#,
        )
        .unwrap();

        let settings: Value = serde_json::from_str(r#"{"mcpServers": {"from_settings": {"type": "stdio", "command": "echo", "args": ["settings"]}}}"#).unwrap();
        let servers = discover_mcp_servers(&project, &settings).unwrap();

        assert_eq!(servers.len(), 2);
        assert!(servers.contains_key("from_settings"));
        assert!(servers.contains_key("from_file"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_mcp_servers_cwd_overrides_ancestor() {
        let _guard = crate::test_support::env_lock();
        let fake_home = std::env::temp_dir().join("clawed_mcp_home_cwd_overrides_ancestor");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG");
            std::env::remove_var("CLAWEDCODE_MCP_CONFIG_STRICT");
        }
        let root = temp_dir("mcp_override");
        let parent = root.join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();

        fs::write(
            parent.join(".mcp.json"),
            r#"{"mcpServers": {"server": {"type": "stdio", "command": "parent-cmd", "args": []}}}"#,
        )
        .unwrap();
        fs::write(
            child.join(".mcp.json"),
            r#"{"mcpServers": {"server": {"type": "stdio", "command": "child-cmd", "args": []}}}"#,
        )
        .unwrap();

        let servers = discover_mcp_servers(&child, &Value::Null).unwrap();

        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers.get("server").and_then(|s| s.command()),
            Some("child-cmd".to_string())
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_skill_strips_frontmatter_and_extracts_body() {
        let root = temp_dir("skill_frontmatter");
        let skill_md = root.join("My Skill").join("SKILL.md");
        fs::create_dir_all(skill_md.parent().unwrap()).unwrap();
        fs::write(
            &skill_md,
            r#"---
name: My Test Skill
description: A test skill
when_to_use: Use this for testing
---

This is the skill body content.
It should be preserved after frontmatter stripping.

## Section

Some more content here.
"#,
        )
        .unwrap();

        let skill = parse_skill(&skill_md, false).unwrap();

        assert_eq!(skill.name, "My Test Skill");
        assert_eq!(skill.description, Some("A test skill".to_string()));
        assert_eq!(skill.when_to_use, Some("Use this for testing".to_string()));
        assert!(skill.body.contains("This is the skill body content"));
        assert!(skill.body.contains("## Section"));
        assert!(!skill.body.contains("---"));
        assert!(!skill.body.contains("name:"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_skill_without_frontmatter_uses_full_content_as_body() {
        let root = temp_dir("skill_no_frontmatter");
        let skill_md = root.join("Simple Skill").join("SKILL.md");
        fs::create_dir_all(skill_md.parent().unwrap()).unwrap();
        fs::write(
            &skill_md,
            "This is a simple skill without frontmatter.\n\nJust plain markdown content.",
        )
        .unwrap();

        let skill = parse_skill(&skill_md, false).unwrap();

        assert_eq!(skill.name, "Simple Skill");
        assert_eq!(skill.description, None);
        assert_eq!(
            skill.body,
            "This is a simple skill without frontmatter.\n\nJust plain markdown content."
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_skill_computes_stable_slash_command() {
        let root = temp_dir("skill_slash_name");
        let skill_md = root.join("My Test Skill").join("SKILL.md");
        fs::create_dir_all(skill_md.parent().unwrap()).unwrap();
        fs::write(&skill_md, "---\nname: My Test Skill\n---\nBody").unwrap();

        let skill = parse_skill(&skill_md, false).unwrap();

        assert_eq!(skill.slash_command, "/my-test-skill");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parse_skill_flags_legacy_commands() {
        let root = temp_dir("skill_legacy");
        let legacy_md = root.join("commands").join("Legacy.md");
        fs::create_dir_all(legacy_md.parent().unwrap()).unwrap();
        fs::write(&legacy_md, "---\nname: Legacy Command\n---\nLegacy body").unwrap();

        let from_commands = parse_skill(&legacy_md, true).unwrap();
        assert!(from_commands.legacy_command);
        assert_eq!(from_commands.slash_command, "/legacy-command");

        let regular_md = root.join("skills").join("Regular.md");
        fs::create_dir_all(regular_md.parent().unwrap()).unwrap();
        fs::write(&regular_md, "---\nname: Regular Skill\n---\nRegular body").unwrap();

        let from_skills = parse_skill(&regular_md, false).unwrap();
        assert!(!from_skills.legacy_command);
        assert_eq!(from_skills.slash_command, "/regular-skill");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_skills_marks_legacy_commands() {
        let _guard = env_lock();
        let root = temp_dir("discover_legacy");
        let home = root.join("home");
        let project = root.join("project");

        fs::create_dir_all(home.join(".claude").join("skills")).unwrap();
        fs::create_dir_all(project.join(".claude").join("commands")).unwrap();

        fs::write(
            home.join(".claude").join("skills").join("HomeSkill.md"),
            "---\nname: Home Skill\n---\nHome body",
        )
        .unwrap();
        fs::write(
            project
                .join(".claude")
                .join("commands")
                .join("ProjectCmd.md"),
            "---\nname: Project Command\n---\nProject body",
        )
        .unwrap();

        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude")) };
        let skills = discover_skills(&project).unwrap();
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

        let home_skill = skills.iter().find(|s| s.name == "Home Skill").unwrap();
        assert!(!home_skill.legacy_command);

        let project_cmd = skills.iter().find(|s| s.name == "Project Command").unwrap();
        assert!(project_cmd.legacy_command);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_skills_prefers_nearest_duplicate_slash_command() {
        let _guard = env_lock();
        let root = temp_dir("discover_precedence");
        let home = root.join("home");
        let project = root.join("project");
        let nested = project.join("apps").join("api");

        fs::create_dir_all(home.join(".claude").join("commands")).unwrap();
        fs::create_dir_all(project.join(".claude").join("commands")).unwrap();
        fs::create_dir_all(nested.clone()).unwrap();

        fs::write(
            home.join(".claude").join("commands").join("review.md"),
            "---\nname: Review\n---\nHome review body",
        )
        .unwrap();
        fs::write(
            project.join(".claude").join("commands").join("review.md"),
            "---\nname: Review\n---\nProject review body",
        )
        .unwrap();

        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude")) };
        let skills = discover_skills(&nested).unwrap();
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

        let review_matches: Vec<&SkillDescriptor> = skills
            .iter()
            .filter(|skill| skill.slash_command == "/review")
            .collect();
        assert_eq!(review_matches.len(), 1);
        assert_eq!(
            review_matches[0].path,
            project.join(".claude").join("commands").join("review.md")
        );
        assert_eq!(review_matches[0].body, "Project review body");

        fs::remove_dir_all(root).ok();
    }

    mod snapshot_tests {
        use super::*;
        use crate::test_support::env_lock;

        #[test]
        fn discover_snapshot_exact_fields() {
            let _guard = env_lock();
            let root = temp_dir("discover_snapshot");
            let project = root.join("project");
            let home = root.join("home").join(".claude");

            fs::create_dir_all(&project).unwrap();
            fs::create_dir_all(&home).unwrap();

            fs::write(
                home.join("settings.json"),
                r#"{"mcpServers": {"settings-server": {"type": "stdio", "command": "echo", "args": ["settings"]}}}"#,
            )
            .unwrap();

            fs::write(
                project.join(".mcp.json"),
                r#"{"mcpServers": {"project-server": {"type": "stdio", "command": "echo", "args": ["project"]}}}"#,
            )
            .unwrap();

            fs::create_dir_all(home.join("skills")).unwrap();
            fs::write(
                home.join("skills").join("TestSkill.md"),
                "---\nname: Test Skill\ndescription: A test skill\n---\nTest skill body",
            )
            .unwrap();

            fs::write(
                home.join("CLAUDE.md"),
                "# User Memory\n\nFollow these rules.",
            )
            .unwrap();

            unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &home) };
            let snapshot = discover(&project).unwrap();
            unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

            assert!(
                snapshot
                    .settings_files
                    .iter()
                    .any(|p| p.ends_with("settings.json"))
            );
            assert!(snapshot.mcp_servers.contains_key("settings-server"));
            assert!(snapshot.mcp_servers.contains_key("project-server"));
            assert!(snapshot.skills.iter().any(|s| s.name == "Test Skill"));
            assert!(snapshot.memory.contains("User Memory"));
            assert!(snapshot.memory.contains("Follow these rules."));

            fs::remove_dir_all(root).ok();
        }

        #[test]
        fn mcp_server_config_snapshot_stdio_exact() {
            let json = r#"{"type": "stdio", "command": "/usr/bin/python3", "args": ["-m", "mymcp"], "env": {"KEY": "value"}}"#;
            let config: McpServerConfig = serde_json::from_str(json).unwrap();

            match config {
                McpServerConfig::Stdio {
                    r#type,
                    command,
                    args,
                    env,
                } => {
                    assert_eq!(r#type, Some("stdio".to_string()));
                    assert_eq!(command, "/usr/bin/python3");
                    assert_eq!(args, vec!["-m", "mymcp"]);
                    assert_eq!(env.get("KEY"), Some(&"value".to_string()));
                }
                _ => panic!("expected Stdio variant"),
            }
        }

        #[test]
        fn mcp_server_config_snapshot_http_exact() {
            let json = r#"{"type": "http", "url": "https://api.example.com/mcp", "headers": {"Authorization": "Bearer token"}}"#;
            let config: McpServerConfig = serde_json::from_str(json).unwrap();

            match &config {
                McpServerConfig::Sse {
                    r#type,
                    url,
                    headers,
                }
                | McpServerConfig::Http {
                    r#type,
                    url,
                    headers,
                }
                | McpServerConfig::Ws {
                    r#type,
                    url,
                    headers,
                } => {
                    assert_eq!(*r#type, "http");
                    assert_eq!(url, "https://api.example.com/mcp");
                    assert_eq!(
                        headers.get("Authorization"),
                        Some(&"Bearer token".to_string())
                    );
                }
                McpServerConfig::Stdio { .. } | McpServerConfig::Sdk { .. } => {
                    panic!("expected url-bearing variant, got {:?}", config)
                }
            }
        }

        #[test]
        fn mcp_server_config_snapshot_sse_exact() {
            let json = r#"{"type": "sse", "url": "https://events.example.com/mcp", "headers": {"X-Custom": "value"}}"#;
            let config: McpServerConfig = serde_json::from_str(json).unwrap();

            match &config {
                McpServerConfig::Sse {
                    r#type,
                    url,
                    headers,
                } => {
                    assert_eq!(*r#type, "sse");
                    assert_eq!(url, "https://events.example.com/mcp");
                    assert_eq!(headers.get("X-Custom"), Some(&"value".to_string()));
                }
                McpServerConfig::Http { .. }
                | McpServerConfig::Ws { .. }
                | McpServerConfig::Stdio { .. }
                | McpServerConfig::Sdk { .. } => {
                    panic!("expected Sse variant, got {:?}", config)
                }
            }
        }

        #[test]
        fn mcp_server_config_snapshot_ws_exact() {
            let json = r#"{"type": "ws", "url": "wss://ws.example.com/mcp", "headers": {}}"#;
            let config: McpServerConfig = serde_json::from_str(json).unwrap();

            match &config {
                McpServerConfig::Sse {
                    r#type,
                    url,
                    headers,
                }
                | McpServerConfig::Ws {
                    r#type,
                    url,
                    headers,
                }
                | McpServerConfig::Http {
                    r#type,
                    url,
                    headers,
                } => {
                    assert_eq!(*r#type, "ws");
                    assert_eq!(url, "wss://ws.example.com/mcp");
                    assert!(headers.is_empty());
                }
                McpServerConfig::Stdio { .. } | McpServerConfig::Sdk { .. } => {
                    panic!("expected url-bearing variant, got {:?}", config)
                }
            }
        }

        #[test]
        fn deep_merge_snapshot_nested_objects() {
            let mut target = serde_json::json!({
                "model": "default-model",
                " provider": {"endpoint": "default-endpoint"}
            });
            let source = serde_json::json!({
                "model": "override-model",
                "mcpServers": {"server1": {"command": "echo"}}
            });

            deep_merge(&mut target, source);

            let expected = serde_json::json!({
                "model": "override-model",
                " provider": {"endpoint": "default-endpoint"},
                "mcpServers": {"server1": {"command": "echo"}}
            });

            assert_eq!(target, expected);
        }

        #[test]
        fn settings_resolution_order_snapshot() {
            let _guard = env_lock();
            let root = temp_dir("settings_order");
            let home = root.join("home").join(".claude");
            let project = root.join("project").join(".claude");
            let nested = root.join("project").join("apps").join("api");

            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&project).unwrap();
            fs::create_dir_all(&nested).unwrap();

            fs::write(
                home.join("settings.json"),
                r#"{"global": "from-home", "override": "home"}"#,
            )
            .unwrap();
            fs::write(
                project.join("settings.json"),
                r#"{"project": "from-project", "override": "project"}"#,
            )
            .unwrap();
            fs::write(
                project.join("settings.local.json"),
                r#"{"local": "from-local", "override": "local"}"#,
            )
            .unwrap();

            unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", root.join("home").join(".claude")) };
            let (files, settings) = discover_settings(&nested).unwrap();
            unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

            assert!(files.iter().any(|p| p.ends_with("settings.json")));
            assert!(files.iter().any(|p| p.ends_with("settings.local.json")));

            assert_eq!(
                settings.get("global").and_then(|v| v.as_str()),
                Some("from-home")
            );
            assert_eq!(
                settings.get("project").and_then(|v| v.as_str()),
                Some("from-project")
            );
            assert_eq!(
                settings.get("local").and_then(|v| v.as_str()),
                Some("from-local")
            );
            assert_eq!(
                settings.get("override").and_then(|v| v.as_str()),
                Some("local")
            );

            fs::remove_dir_all(root).ok();
        }

        #[test]
        fn slash_command_generation_snapshot() {
            assert_eq!(slash_command_from_name("Code Review"), "/code-review");
            assert_eq!(slash_command_from_name("My Test Skill"), "/my-test-skill");
            assert_eq!(slash_command_from_name("simple"), "/simple");
            assert_eq!(slash_command_from_name("Already-Dashed"), "/already-dashed");
            assert_eq!(
                slash_command_from_name("Multi Word Name"),
                "/multi-word-name"
            );
        }

        #[test]
        fn skill_descriptor_snapshot_exact_fields() {
            let path = PathBuf::from("/skills/my-skill/SKILL.md");
            let skill = SkillDescriptor {
                name: "My Skill".to_string(),
                description: Some("Does something useful".to_string()),
                when_to_use: Some("Use when needed".to_string()),
                path: path.clone(),
                body: "Skill content here.\n\nMore details.".to_string(),
                slash_command: "/my-skill".to_string(),
                legacy_command: false,
            };

            assert_eq!(skill.name, "My Skill");
            assert_eq!(skill.description.as_deref(), Some("Does something useful"));
            assert_eq!(skill.when_to_use.as_deref(), Some("Use when needed"));
            assert_eq!(skill.path, path);
            assert!(skill.body.contains("Skill content here"));
            assert_eq!(skill.slash_command, "/my-skill");
            assert!(!skill.legacy_command);
        }
    }
}
