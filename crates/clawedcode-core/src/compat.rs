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
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillDescriptor {
    pub name: String,
    pub description: Option<String>,
    pub when_to_use: Option<String>,
    pub path: PathBuf,
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
    let mcp_servers = discover_mcp_servers(cwd, &settings)?;

    Ok(CompatibilitySnapshot {
        settings_files,
        settings,
        skills,
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
    let mut paths = Vec::new();

    if let Some(home) = claude_home() {
        paths.push(home.join("skills"));
    }

    for ancestor in cwd.ancestors() {
        paths.push(ancestor.join(".claude").join("skills"));
        paths.push(ancestor.join(".claude").join("commands"));
    }

    let mut discovered = Vec::new();
    for root in paths {
        if !root.exists() {
            continue;
        }
        discover_skills_in_root(&root, &mut discovered)?;
    }

    discovered.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    discovered.dedup_by(|a, b| a.path == b.path);
    Ok(discovered)
}

fn discover_skills_in_root(root: &Path, out: &mut Vec<SkillDescriptor>) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                out.push(parse_skill(&skill_md)?);
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
                    out.push(parse_skill(&nested_path)?);
                    break;
                }
            }
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            out.push(parse_skill(&path)?);
        }
    }

    Ok(())
}

fn parse_skill(path: &Path) -> Result<SkillDescriptor> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let frontmatter = parse_frontmatter(&raw).unwrap_or_default();
    let fallback_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .or_else(|| path.file_stem().and_then(|name| name.to_str()))
        .unwrap_or("skill")
        .to_string();

    Ok(SkillDescriptor {
        name: frontmatter.name.unwrap_or(fallback_name),
        description: frontmatter.description,
        when_to_use: frontmatter.when_to_use,
        path: path.to_path_buf(),
    })
}

fn parse_frontmatter(raw: &str) -> Option<SkillFrontmatter> {
    let mut lines = raw.lines();
    if lines.next()? != "---" {
        return None;
    }

    let mut yaml = String::new();
    for line in lines {
        if line == "---" {
            return serde_yaml::from_str::<SkillFrontmatter>(&yaml).ok();
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    None
}

fn discover_mcp_servers(cwd: &Path, settings: &Value) -> Result<BTreeMap<String, McpServerConfig>> {
    let mut servers = parse_settings_mcp_servers(settings);

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
