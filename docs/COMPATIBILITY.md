# Compatibility Specification

## Overview

The agent shell must discover and respect existing filesystem conventions that operators rely on. This document specifies the discovery paths and merge semantics for settings, skills, and MCP servers.

---

## 1. Settings Discovery

### 1.1 Search Paths

Settings files are discovered in this order (earlier files havelower precedence, later files override):

```
Priority (Low→ High):

1. ~/.claude/settings.json                    User global settings
2. <project>/.claude/settings.json             Project settings
3. <project>/.claude/settings.local.json       Local overrides (git-ignored)
```

### 1.2 Merge Semantics

```
deep_merge(target, source):
    for key in source:
        if key in target AND both are objects:
            deep_merge(target[key], source[key])
        else:
            target[key] = source[key]
```

Example:
```json
// User settings
{ "model": "gpt-4", "ui": { "theme": "dark" } }

// Project settings  
{ "model": "gpt-5", "runtime": { "maxTurns": 100 } }

// Merged result
{ "model": "gpt-5", "ui": { "theme": "dark" }, "runtime": { "maxTurns": 100 } }
```

### 1.3 Settings Schema

```json
{
    "model": "string",
    "provider": {
        "endpoint": "string?",
        "api_key_env": "string"
    },
    "ui": {
        "theme": "string",
        "show_thinking": "boolean",
        "vim_mode": "boolean"
    },
    "runtime": {
        "max_turns": "integer",
        "session_history_limit": "integer",
        "auto_compact_threshold": "number"
    },
    "permissions": {
        "mode": "string",
        "allow": ["string"],
        "deny": ["string"]
    },
    "mcpServers": {
        "<name>": { "..." }
    }
}
```

---

## 2. Skills Discovery

### 2.1 Search Paths

```
Priority (Low → High):

1. ~/.claude/skills/<name>/SKILL.md      User global skills
2. <project>/.claude/skills/<name>/SKILL.md   Project skills
3. <project>/.claude/commands/<name>.md        Legacy command files
```

### 2.2 Directory Structure

```
skills/
├── code-review/
│   └── SKILL.md
├── testing/
│   └── SKILL.md
└── deployment/
    └── SKILL.md

Legacy (commands/):
commands/
├── commit.md
└── pr.md
```

###2.3 SKILL.md Format

```markdown
---
name: Skill Name
description: One-line description for UI
when_to_use: Context hint for automatic invocation
---

# Skill Instructions

Detailed instructions for the AI model...

## Subsection

More details...
```

### 2.4 Frontmatter Schema

```yaml
name: string           # Required: Display name
description: string    # Optional: One-line summary
when_to_use: string    # Optional: Auto-invocation hint
```

### 2.5 Discovery Algorithm

```
discover_skills(cwd) → Skill[]:
    roots = []
    
    // User global
    if CLAUDE_CONFIG_DIR env:
        roots.push(CLAUDE_CONFIG_DIR/skills)
    else if HOME env:
        roots.push(HOME/.claude/skills)
    
    // Project-local (walkup)
    for ancestor in cwd.ancestors():
        roots.push(ancestor/.claude/skills)
        roots.push(ancestor/.claude/commands)  // Legacy
    
    skills = []
    for root in roots:
        if root.exists():
            for entry in root.entries():
                if entry.is_dir():
                    check entry/SKILL.md
                elif entry.ext == ".md":
                    parse as skill
    
    return deduplicate(skills, by: path)
```

### 2.6 Parsing

```
parse_skill(path) → Skill:
    content = read_file(path)
    
    // Extract frontmatter
    if content.starts_with("---"):
        [frontmatter, body] = split_yaml_frontmatter(content)
        metadata = parse_yaml(frontmatter)
    else:
        metadata = {}
        body = content
    
    name = metadata.name ?? parent_dir_name(path) ?? stem(path)
    
    return Skill(
        name: name,
        description: metadata.description,
        when_to_use: metadata.when_to_use,
        path: path,
        content: body
    )
```

---

## 3. MCP Server Discovery

### 3.1 Sources

```
Priority (Low → High):

1. ~/.claude/settings.json → mcpServers key
2. <project>/.claude/settings.json → mcpServers key  
3. <project>/.claude/settings.local.json → mcpServers key
4. <project>/.mcp.json → mcpServers key
```

### 3.2 MCP Configuration Schema

```
McpServerConfig:
├── Stdio
│   ├── command: string          Executable path
│   ├── args: string[]           Command arguments
│   └── env: Map<string, string> Environment variables
│
├── Http
│   ├── type: "http"
│   ├── url: string              Server URL
│   └── headers: Map<string, string>
│
├── Sse
│   ├── type: "sse"
│   ├── url: string              SSE endpoint
│   └── headers: Map<string, string>
│
├── WebSocket
│   ├── type: "ws"
│   ├── url: string              WebSocket URL
│   └── headers: Map<string, string>
│
└── Sdk
    ├── type: "sdk"
    └── name: string             Built-in server name
```

### 3.3 Discovery Algorithm

```
discover_mcp_servers(cwd) → Map<string, McpServerConfig>:
    servers = {}
    
    // From settings files
    for path in settings_search_paths(cwd):
        if path.exists():
            settings = parse_json(path)
            if settings.mcpServers:
                servers.merge(settings.mcpServers)
    
    // From .mcp.json
    mcp_json = cwd/.mcp.json
    if mcp_json.exists():
        content = parse_json(mcp_json)
        if content.mcpServers:
            servers.merge(content.mcpServers)
    
    return servers
```

### 3.4 MCP JSON Format

```
// .mcp.json
{
    "mcpServers": {
        "filesystem": {
            "command": "/path/to/mcp-filesystem",
            "args": ["--root", "/project"]
        },
        "github": {
            "type": "http",
            "url": "https://mcp.github.com/api",
            "headers": {
                "Authorization": "Bearer ${GITHUB_TOKEN}"
            }
        }
    }
}
```

---

## 4. Environment Variables

### 4.1 Recognized Variables

```
CLAUDE_CONFIG_DIR     Override config directory
                      Default: ~/.claude/

CLAUDE_DATA_DIR       Override data directory  
                      Default: ~/.local/share/claude/

CLAUDE_API_KEY        API key for provider
                      (Provider-specific env takes precedence)

OPENAI_API_KEY        OpenAI API key
ANTHROPIC_API_KEY     Anthropic API key
```

### 4.2 Variable Substitution

Settings files support environment variable substitution:

```json
{
    "provider": {
        "api_key_env": "OPENAI_API_KEY"// Reference by name
    }
}

// Or inline substitution (future):
{
    "provider": {
        "endpoint": "${OPENAI_ENDPOINT}"
    }
}
```

---

## 5. Legacy Compatibility

### 5.1 Commands Directory

The `commands/` directory is a legacy format. Each markdown file is treated as a skill:

```
.claude/commands/
├── commit.md        → Skill with name "commit"
└── review.md        → Skill with name "review"
```

### 5.2 Migration Path

```
if commands/ exists AND skills/ does not exist:
    warn "commands/ is deprecated, migrate to skills/"
    // Still discover, mark as legacy
```

### 5.3 Frontmatter Compatibility

Legacy files may lack frontmatter. Use filename as skill name:

```
parse_legacy_command(path) → Skill:
    content = read_file(path)
    name = stem(path)  // filename without extension
    return Skill(name: name, content: content)
```

---

## 6. Project Detection

### 6.1 Project Root

```
find_project_root(cwd) → Path?:
    markers = [
        ".git",
        ".hg",
        ".svn",
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        ".claude"// Our own marker
    ]
    
    for ancestor in cwd.ancestors():
        for marker in markers:
            if ancestor/marker exists:
                return ancestor
    
    return cwd  // Fallback to cwd
```

### 6.2 Project ID

Used for session storage:

```
project_id(root) → string:
    // Use git remote if available
    if root/.git exists:
        remotes = git_remote_urls(root)
        if remotes:
            return hash(remotes.first())
    
    // Fallback to path hash
    return hash(root.absolute_path())
```

---

## 7. Compatibility Snapshot

For debugging and inspection:

```
struct CompatibilitySnapshot:
    settingsFiles: Path[]              // Resolved settings paths
    settings: Json                      // Merged settings
    skills: Skill[]                     // Discovered skills
    mcpServers: Map<string, McpConfig>  // Discovered MCP servers

discover_all(cwd) → CompatibilitySnapshot:
    settings = discover_settings(cwd)
    skills = discover_skills(cwd)
    mcp = discover_mcp_servers(cwd, settings.merged)
    
    return CompatibilitySnapshot(
        settingsFiles: settings.paths,
        settings: settings.merged,
        skills: skills,
        mcpServers: mcp
    )
```

---

## 8. CLI Integration

```
compat subcommand:

$ app compat

Output:
{
    "settingsFiles": [
        "/home/user/.claude/settings.json",
        "/project/.claude/settings.json"
    ],
    "settings": { "... merged ..." },
    "skills": [
        {
            "name": "code-review",
            "description": "...",
            "path": "/project/.claude/skills/code-review/SKILL.md"
        }
    ],
    "mcpServers": {
        "filesystem": { "command": "...", "args": [...] },
        "github": { "type": "http", "url": "..." }
    }
}
```