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

fn discover_skills(cwd: &Path) -> Result<Vec<SkillDescriptor>> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();

    if let Some(home) = claude_home() {
        paths.push((home.join("skills"), false));
    }

    for ancestor in cwd.ancestors() {
        paths.push((ancestor.join(".claude").join("skills"), false));
        paths.push((ancestor.join(".claude").join("commands"), true));
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

    unique.sort_by(|a, b| a.slash_command.cmp(&b.slash_command).then(a.path.cmp(&b.path)));
    Ok(unique)
}

fn discover_memory(cwd: &Path) -> Result<(Vec<PathBuf>, String)> {
    let mut memory_files = Vec::new();
    let mut sections = Vec::new();

    for path in memory_search_paths(cwd, claude_home().as_deref())? {
        if !path.exists() {
            continue;
        }

        let expanded = expand_memory_file(&path, &mut Vec::new(), 0)?;
        if expanded.trim().is_empty() {
            continue;
        }

        memory_files.push(path.clone());
        sections.push(format!("<!-- {} -->\n{}", path.display(), expanded.trim()));
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

fn expand_memory_file(path: &Path, stack: &mut Vec<PathBuf>, depth: usize) -> Result<String> {
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
            let nested = expand_memory_file(&include_path, stack, depth + 1)?;
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
    let (frontmatter, body) = parse_frontmatter_and_body(&raw);
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

fn parse_frontmatter_and_body(raw: &str) -> (SkillFrontmatter, String) {
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return (SkillFrontmatter::default(), raw.trim().to_string());
    }

    let mut yaml = String::new();
    while let Some(line) = lines.next() {
        if line == "---" {
            let frontmatter = serde_yaml::from_str::<SkillFrontmatter>(&yaml).ok();
            let body = lines.collect::<Vec<&str>>().join("\n").trim().to_string();
            return (frontmatter.unwrap_or_default(), body);
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    (SkillFrontmatter::default(), raw.trim().to_string())
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
    let mut servers = parse_settings_mcp_servers(settings);

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
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().expect("env lock")
    }

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

        let expanded = expand_memory_file(&main, &mut Vec::new(), 0).unwrap();
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

        let expanded = expand_memory_file(&a, &mut Vec::new(), 0).unwrap();
        assert!(expanded.contains("from a"));
        assert!(expanded.contains("from b"));
        assert_eq!(expanded.matches("from a").count(), 1);
        assert_eq!(expanded.matches("from b").count(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
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
}
