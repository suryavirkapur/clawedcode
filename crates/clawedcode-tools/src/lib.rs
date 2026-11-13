use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    str::FromStr,
};

pub const AGENT_TOOL_NAME: &str = "Agent";
pub const LEGACY_AGENT_TOOL_NAME: &str = "Task";

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

/// A tool that can be invoked by the runtime.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn needs_approval(&self) -> bool;
    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult;
}

static EXTRA_ALLOWED_ROOTS: std::sync::RwLock<Vec<PathBuf>> = std::sync::RwLock::new(Vec::new());

/// Set the extra directories file tools may access for the current session
/// (`/add-dir` in the TUI). Called by the runtime before each tool execution.
pub fn set_extra_allowed_roots(roots: Vec<PathBuf>) {
    if let Ok(mut guard) = EXTRA_ALLOWED_ROOTS.write() {
        *guard = roots;
    }
}

/// Paths added via `/add-dir`: canonicalized when the session recorded them.
pub fn extra_allowed_roots() -> Vec<PathBuf> {
    EXTRA_ALLOWED_ROOTS
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn is_under_allowed_root(canonical: &Path, canonical_cwd: &Path) -> bool {
    canonical.starts_with(canonical_cwd)
        || extra_allowed_roots()
            .iter()
            .any(|root| canonical.starts_with(root))
}

fn resolve_under_cwd(cwd: &Path, rel: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from_str(rel).map_err(|e| format!("Invalid path '{rel}': {e}"))?;
    if path.is_absolute() {
        // Absolute paths are only permitted under cwd or a directory the
        // session explicitly allowed via /add-dir.
        if let Ok(canonical) = path.canonicalize() {
            let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
            if is_under_allowed_root(&canonical, &canonical_cwd) {
                return Ok(canonical);
            }
        }
        return Err("Path must be relative".into());
    }

    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(part) => clean.push(part),
            Component::ParentDir => return Err("Path must not contain '..'".into()),
            Component::Prefix(_) | Component::RootDir => return Err("Path must be relative".into()),
        }
    }

    if clean.as_os_str().is_empty() {
        return Err("Path must not be empty".into());
    }

    Ok(cwd.join(clean))
}

// --- ReadFile ---

/// Reads a file under cwd with a size limit.
pub struct ReadFile;

const READ_FILE_MAX_BYTES: usize = 256 * 1024; // 256 KB

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file under the working directory"
    }

    fn needs_approval(&self) -> bool {
        false
    }

    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolResult {
                    content: "Missing 'path' parameter".into(),
                    is_error: true,
                };
            }
        };

        let full_path = match resolve_under_cwd(cwd, path_str) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    content: e,
                    is_error: true,
                };
            }
        };

        // Security: ensure the resolved path is under cwd
        let canonical_cwd = match cwd.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    content: format!("Cannot resolve cwd: {e}"),
                    is_error: true,
                };
            }
        };
        let canonical_path = match full_path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    content: format!("Cannot resolve path: {e}"),
                    is_error: true,
                };
            }
        };
        if !is_under_allowed_root(&canonical_path, &canonical_cwd) {
            return ToolResult {
                content: "Path escapes working directory".into(),
                is_error: true,
            };
        }

        let metadata = match std::fs::metadata(&canonical_path) {
            Ok(m) => m,
            Err(e) => {
                return ToolResult {
                    content: format!("Cannot read file: {e}"),
                    is_error: true,
                };
            }
        };

        if metadata.len() as usize > READ_FILE_MAX_BYTES {
            return ToolResult {
                content: format!(
                    "File too large ({} bytes, limit {})",
                    metadata.len(),
                    READ_FILE_MAX_BYTES
                ),
                is_error: true,
            };
        }

        match std::fs::read_to_string(&canonical_path) {
            Ok(contents) => ToolResult {
                content: contents,
                is_error: false,
            },
            Err(e) => ToolResult {
                content: format!("Read error: {e}"),
                is_error: true,
            },
        }
    }
}

// --- Shell ---

/// Runs a shell command in cwd.
pub struct Shell;

const SHELL_MAX_OUTPUT_BYTES: usize = 128 * 1024; // 128 KB

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellCommandSpec {
    program: String,
    args: Vec<String>,
}

fn bash_is_available() -> bool {
    std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_ok()
}

fn shell_command_spec(command: &str, bash_available: bool) -> ShellCommandSpec {
    if cfg!(windows) {
        ShellCommandSpec {
            program: "cmd".to_string(),
            args: vec!["/C".to_string(), command.to_string()],
        }
    } else if bash_available {
        ShellCommandSpec {
            program: "bash".to_string(),
            args: vec!["-lc".to_string(), command.to_string()],
        }
    } else {
        ShellCommandSpec {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
        }
    }
}

fn build_shell_command(spec: &ShellCommandSpec, cwd: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(&spec.program);
    cmd.args(&spec.args).current_dir(cwd);
    cmd
}

impl Tool for Shell {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Run a shell command in the working directory"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolResult {
                    content: "Missing 'command' parameter".into(),
                    is_error: true,
                };
            }
        };

        let spec = shell_command_spec(command, bash_is_available());
        let output = build_shell_command(&spec, cwd).output();

        match output {
            Ok(out) => {
                let mut content = String::new();
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);

                if !stdout.is_empty() {
                    content.push_str(&truncate(&stdout, SHELL_MAX_OUTPUT_BYTES));
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push_str("\n--- stderr ---\n");
                    }
                    content.push_str(&truncate(&stderr, SHELL_MAX_OUTPUT_BYTES));
                }
                if content.is_empty() {
                    content = format!("(exit {})", out.status.code().unwrap_or(-1));
                }

                ToolResult {
                    content,
                    is_error: !out.status.success(),
                }
            }
            Err(e) => ToolResult {
                content: format!("Command execution failed: {e}"),
                is_error: true,
            },
        }
    }
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let mut end = max_bytes;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}... [truncated]", &s[..end])
    }
}

// --- ApplyPatch ---

/// Applies structured edits to files under cwd.
///
/// Input schema:
/// `{ "patch": "*** Begin Patch\n...*** End Patch\n" }`
pub struct ApplyPatch;

impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply structured file edits under the working directory"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let patch = match input.get("patch").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolResult {
                    content: "Missing 'patch' parameter".into(),
                    is_error: true,
                };
            }
        };

        match apply_patch_text(cwd, patch) {
            Ok(summary) => ToolResult {
                content: summary,
                is_error: false,
            },
            Err(e) => ToolResult {
                content: format!("apply_patch failed: {e}"),
                is_error: true,
            },
        }
    }
}

/// The reference's AskUserQuestionTool — prompts the user with a
/// multiple-choice question. The interactive surface answers it through
/// the approval channel; this execute is the non-interactive fallback.
pub struct AskUserQuestion;

pub const ASK_USER_QUESTION_TOOL_NAME: &str = "ask_user_question";

impl Tool for AskUserQuestion {
    fn name(&self) -> &str {
        ASK_USER_QUESTION_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Prompt the user with a multiple-choice question"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn execute(&self, _input: serde_json::Value, _cwd: &Path) -> ToolResult {
        ToolResult {
            content: "User declined to answer questions".into(),
            is_error: false,
        }
    }
}

#[derive(Debug, Clone)]
enum PatchHunk {
    AddFile {
        path: String,
        lines: Vec<String>,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Debug, Clone)]
struct UpdateChunk {
    before: Vec<String>,
    after: Vec<String>,
}

fn apply_patch_text(cwd: &Path, patch: &str) -> Result<String, String> {
    let mut lines: Vec<&str> = patch.lines().collect();

    // Tolerate trailing newline that creates an empty last line when using split_inclusive elsewhere.
    if lines.last().copied() == Some("") {
        lines.pop();
    }

    if lines.first().copied() != Some("*** Begin Patch") {
        return Err("Patch must start with '*** Begin Patch'".into());
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err("Patch must end with '*** End Patch'".into());
    }

    let hunks = parse_hunks(&lines[1..lines.len() - 1])?;
    let mut applied = Vec::new();

    for h in hunks {
        match h {
            PatchHunk::AddFile { path, lines } => {
                let dest = resolve_under_cwd(cwd, &path)?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
                }
                let mut content = lines.join("\n");
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                std::fs::write(&dest, content).map_err(|e| format!("{e}"))?;
                applied.push(format!("add {path}"));
            }
            PatchHunk::DeleteFile { path } => {
                let dest = resolve_under_cwd(cwd, &path)?;
                std::fs::remove_file(&dest).map_err(|e| format!("{e}"))?;
                applied.push(format!("delete {path}"));
            }
            PatchHunk::UpdateFile {
                path,
                move_to,
                chunks,
            } => {
                let src = resolve_under_cwd(cwd, &path)?;
                let raw = std::fs::read_to_string(&src).map_err(|e| format!("{e}"))?;
                let mut file_lines: Vec<String> = raw.lines().map(|s| s.to_string()).collect();

                for chunk in &chunks {
                    apply_update_chunk(&mut file_lines, chunk)?;
                }

                let mut new_content = file_lines.join("\n");
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                std::fs::write(&src, new_content).map_err(|e| format!("{e}"))?;

                if let Some(to) = move_to {
                    let dest = resolve_under_cwd(cwd, &to)?;
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
                    }
                    std::fs::rename(&src, &dest).map_err(|e| format!("{e}"))?;
                    applied.push(format!("update {path} -> {to}"));
                } else {
                    applied.push(format!("update {path}"));
                }
            }
        }
    }

    Ok(applied.join("\n"))
}

fn parse_hunks(lines: &[&str]) -> Result<Vec<PatchHunk>, String> {
    let mut i = 0usize;
    let mut hunks = Vec::new();

    while i < lines.len() {
        let line = lines[i];
        if let Some(rest) = line.strip_prefix("*** Add File: ") {
            let path = rest.trim().to_string();
            i += 1;
            let mut add_lines = Vec::new();
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let l = lines[i];
                if let Some(content) = l.strip_prefix('+') {
                    add_lines.push(content.to_string());
                } else {
                    return Err(format!("Add File lines must start with '+': {l}"));
                }
                i += 1;
            }
            hunks.push(PatchHunk::AddFile {
                path,
                lines: add_lines,
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix("*** Delete File: ") {
            let path = rest.trim().to_string();
            i += 1;
            hunks.push(PatchHunk::DeleteFile { path });
            continue;
        }

        if let Some(rest) = line.strip_prefix("*** Update File: ") {
            let path = rest.trim().to_string();
            i += 1;

            let mut move_to: Option<String> = None;
            if i < lines.len() {
                if let Some(rest) = lines[i].strip_prefix("*** Move to: ") {
                    move_to = Some(rest.trim().to_string());
                    i += 1;
                }
            }

            let mut chunks: Vec<UpdateChunk> = Vec::new();
            let mut current = UpdateChunk {
                before: Vec::new(),
                after: Vec::new(),
            };

            while i < lines.len() && !lines[i].starts_with("*** ") {
                let l = lines[i];
                if l.starts_with("@@") {
                    if !current.before.is_empty() || !current.after.is_empty() {
                        chunks.push(current);
                        current = UpdateChunk {
                            before: Vec::new(),
                            after: Vec::new(),
                        };
                    }
                    i += 1;
                    continue;
                }
                if l == "*** End of File" {
                    i += 1;
                    continue;
                }

                if l.is_empty() {
                    return Err("Empty line without prefix in update hunk".into());
                }

                let (prefix, content) = l.split_at(1);
                match prefix {
                    " " => {
                        current.before.push(content.to_string());
                        current.after.push(content.to_string());
                    }
                    "-" => {
                        current.before.push(content.to_string());
                    }
                    "+" => {
                        current.after.push(content.to_string());
                    }
                    _ => return Err(format!("Invalid update line: {l}")),
                }
                i += 1;
            }

            if !current.before.is_empty() || !current.after.is_empty() {
                chunks.push(current);
            }

            if chunks.is_empty() {
                return Err(format!("Update File hunk for '{path}' had no changes"));
            }

            hunks.push(PatchHunk::UpdateFile {
                path,
                move_to,
                chunks,
            });
            continue;
        }

        return Err(format!("Unexpected line in patch: {line}"));
    }

    Ok(hunks)
}

fn apply_update_chunk(file_lines: &mut Vec<String>, chunk: &UpdateChunk) -> Result<(), String> {
    if chunk.before.is_empty() {
        return Err("Chunk has empty 'before' context; refusing to apply ambiguous patch".into());
    }

    let mut matches = Vec::new();
    for start in 0..=file_lines.len().saturating_sub(chunk.before.len()) {
        if file_lines[start..start + chunk.before.len()] == chunk.before[..] {
            matches.push(start);
        }
    }

    match matches.as_slice() {
        [] => Err("Chunk context not found in file".into()),
        [start] => {
            let start = *start;
            file_lines.splice(start..start + chunk.before.len(), chunk.after.clone());
            Ok(())
        }
        _ => Err("Chunk context matched multiple locations; refusing to apply".into()),
    }
}

// --- Registry ---

/// Returns all built-in tool instances.
pub fn builtin_tool_instances() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadFile),
        Box::new(Shell),
        Box::new(ApplyPatch),
        Box::new(AskUserQuestion),
        Box::new(Glob),
        Box::new(Grep),
        Box::new(NotebookEdit),
    ]
}

/// Returns the ToolSpec list for API compatibility (kept for existing code).
pub fn builtin_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "shell".to_string(),
            description: "Run local commands inside the working directory. Set run_in_background to true to run long-running commands without blocking.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Run the command in the background without blocking (default: false)",
                        "default": false
                    }
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: "read_file".to_string(),
            description: "Read the contents of a file under the working directory".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "apply_patch".to_string(),
            description: "Apply structured file edits".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "The patch content in structured format"
                    }
                },
                "required": ["patch"]
            }),
        },
        ToolSpec {
            name: AGENT_TOOL_NAME.to_string(),
            description: "Launch a new agent to handle a bounded task. Use this when the work is complex enough to benefit from a forked sub-agent; omit subagent_type to fork with the current conversation context.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A short 3-5 word description of the delegated task"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task for the spawned agent to perform"
                    },
                    "subagent_type": {
                        "type": "string",
                        "description": "Specialized agent type — `general-purpose` or a `.claude/agents/<name>.md` name. Omit to fork with the current conversation context."
                    },
                    "name": {
                        "type": "string",
                        "description": "Name for the spawned agent. Makes it addressable via SendMessage({to: name}) later."
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Run this agent in the background and return its agent id immediately."
                    }
                },
                "required": ["description", "prompt"]
            }),
        },
        ToolSpec {
            name: "Skill".to_string(),
            description: "Execute a skill (slash-command file or SKILL.md) as a forked sub-agent and return its result.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "The skill name. E.g., \"commit\", \"review-pr\", or \"pdf\""
                    },
                    "args": {
                        "type": "string",
                        "description": "Optional arguments for the skill"
                    }
                },
                "required": ["skill"]
            }),
        },
        ToolSpec {
            name: "Glob".to_string(),
            description: "Find files by glob pattern (supports ** and *)".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern, e.g. **/*.rs"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search (defaults to cwd)"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "Grep".to_string(),
            description: "Search file contents for a regex pattern (file:line:match output)".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Extended regex to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (defaults to cwd)"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "WebFetch".to_string(),
            description: "Fetch a URL and process its content with the given prompt.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch content from"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt to run on the fetched content"
                    }
                },
                "required": ["url", "prompt"]
            }),
        },
        ToolSpec {
            name: "NotebookEdit".to_string(),
            description: "Replace, insert, or delete a cell in a Jupyter notebook (.ipynb)".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "notebook_path": {
                        "type": "string",
                        "description": "Absolute path to the .ipynb file"
                    },
                    "cell_id": {
                        "type": "string",
                        "description": "Cell id or index; insert places the new cell after it"
                    },
                    "new_source": {
                        "type": "string",
                        "description": "The new source for the cell"
                    },
                    "cell_type": {
                        "type": "string",
                        "enum": ["code", "markdown"]
                    },
                    "edit_mode": {
                        "type": "string",
                        "enum": ["replace", "insert", "delete"]
                    }
                },
                "required": ["notebook_path", "new_source"]
            }),
        },
        ToolSpec {
            name: "WebSearch".to_string(),
            description: "Search the web for current information.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query to use"
                    },
                    "allowed_domains": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Only include results from these domains"
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Never include results from these domains"
                    }
                },
                "required": ["query"]
            }),
        },
        ToolSpec {
            name: "EnterPlanMode".to_string(),
            description: "Transition into plan mode: research freely but make no edits until the user approves a plan.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "ExitPlanMode".to_string(),
            description: "Present the finished plan to the user for approval and exit plan mode.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The plan text to present for approval"
                    }
                },
                "required": ["plan"]
            }),
        },
        ToolSpec {
            name: "EnterWorktree".to_string(),
            description: "Create an isolated git worktree (.claude/worktrees/<name>) and switch the session into it.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Optional name; each /-separated segment may contain letters, digits, dots, underscores, dashes"
                    }
                }
            }),
        },
        ToolSpec {
            name: "ExitWorktree".to_string(),
            description: "Leave the session's isolated git worktree and return to the main repository.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "Sleep".to_string(),
            description: "Wait for a specified duration (seconds). Prefer this over `shell` sleep — it doesn't hold a shell process.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "duration_seconds": {
                        "type": "number",
                        "description": "Seconds to wait"
                    }
                },
                "required": ["duration_seconds"]
            }),
        },
        ToolSpec {
            name: "SendMessage".to_string(),
            description: "Send a message to a previously spawned agent (by id or name) and return its reply. The agent resumes with its full context preserved.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Recipient: agent name, agent id, or a unique id prefix"
                    },
                    "message": {
                        "type": "string",
                        "description": "Plain text message content"
                    },
                    "summary": {
                        "type": "string",
                        "description": "A 5-10 word summary shown as a preview in the UI"
                    }
                },
                "required": ["to", "message"]
            }),
        },
        ToolSpec {
            name: LEGACY_AGENT_TOOL_NAME.to_string(),
            description: "Legacy alias for Agent. Launch a new agent to handle a bounded task.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A short 3-5 word description of the delegated task"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task for the spawned agent to perform"
                    },
                    "subagent_type": {
                        "type": "string",
                        "description": "Specialized agent type — `general-purpose` or a `.claude/agents/<name>.md` name. Omit to fork with the current conversation context."
                    }
                },
                "required": ["description", "prompt"]
            }),
        },
        ToolSpec {
            name: "TaskCreate".to_string(),
            description: "Create a new task in the task list. Use this tool when you need to create a task with a subject and description.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "A brief title for the task"
                    },
                    "description": {
                        "type": "string",
                        "description": "What needs to be done"
                    },
                    "activeForm": {
                        "type": "string",
                        "description": "Present continuous form shown in spinner when in_progress (e.g., 'Running tests')"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Arbitrary metadata to attach to the task"
                    }
                },
                "required": ["subject", "description"]
            }),
        },
        ToolSpec {
            name: "TaskList".to_string(),
            description: "List all tasks in the task list. Use this tool to see all tasks and their current status.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolSpec {
            name: "TaskGet".to_string(),
            description: "Get a specific task by ID. Use this tool when you need to retrieve detailed information about a single task.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "taskId": {
                        "type": "string",
                        "description": "The ID of the task to retrieve"
                    }
                },
                "required": ["taskId"]
            }),
        },
        ToolSpec {
            name: "TaskUpdate".to_string(),
            description: "Update a task by ID. Use this tool to modify task status, owner, subject, description, or blocking relationships.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "taskId": {
                        "type": "string",
                        "description": "The ID of the task to update"
                    },
                    "subject": {
                        "type": "string",
                        "description": "New subject for the task"
                    },
                    "description": {
                        "type": "string",
                        "description": "New description for the task"
                    },
                    "activeForm": {
                        "type": "string",
                        "description": "Present continuous form shown in spinner when in_progress"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "deleted"],
                        "description": "New status for the task"
                    },
                    "owner": {
                        "type": "string",
                        "description": "New owner for the task"
                    },
                    "addBlocks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Task IDs that this task blocks"
                    },
                    "addBlockedBy": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Task IDs that block this task"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Metadata keys to merge into the task"
                    }
                },
                "required": ["taskId"]
            }),
        },
        ToolSpec {
            name: "TaskOutput".to_string(),
            description: "Get output from a background task by ID. Use this tool to read the output file and status of a previously started background shell command.".to_string(),
            needs_approval: false,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The ID of the background task"
                    },
                    "block": {
                        "type": "boolean",
                        "description": "Whether to wait for task completion (default: true)",
                        "default": true
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Max wait time in ms (default: 30000)",
                        "default": 30000
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolSpec {
            name: ASK_USER_QUESTION_TOOL_NAME.to_string(),
            description: "Ask the user a multiple-choice question during a turn and use their answer to continue. Questions have a short header chip, the question text, and 2-4 options.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {"type": "string"},
                                "header": {"type": "string"},
                                "options": {
                                    "type": "array",
                                    "minItems": 2,
                                    "maxItems": 4,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {"type": "string"},
                                            "description": {"type": "string"},
                                            "preview": {"type": "string"}
                                        },
                                        "required": ["label", "description"]
                                    }
                                },
                                "multiSelect": {"type": "boolean"}
                            },
                            "required": ["question", "header", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        },
        ToolSpec {
            name: "TaskStop".to_string(),
            description: "Stop a running background task by ID. Use this tool to kill a background shell command that is still running.".to_string(),
            needs_approval: true,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The ID of the background task to stop"
                    },
                    "shell_id": {
                        "type": "string",
                        "description": "Deprecated compatibility alias for task_id"
                    }
                },
                "anyOf": [
                    { "required": ["task_id"] },
                    { "required": ["shell_id"] }
                ]
            }),
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub needs_approval: bool,
    pub input_schema: serde_json::Value,
}

/// Dedicated file-pattern search, like the reference's Glob tool.
pub struct Glob;

/// Dedicated content search, like the reference's Grep tool.
pub struct Grep;

/// Jupyter notebook cell editing, like the reference's NotebookEdit tool.
pub struct NotebookEdit;

impl Tool for NotebookEdit {
    fn name(&self) -> &str {
        "NotebookEdit"
    }
    fn description(&self) -> &str {
        "Replace, insert, or delete a cell in a Jupyter notebook (.ipynb)"
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let Some(path_str) = input.get("notebook_path").and_then(|v| v.as_str()) else {
            return ToolResult {
                content: "Missing 'notebook_path' parameter".into(),
                is_error: true,
            };
        };
        let path = match resolve_under_cwd(cwd, path_str) {
            Ok(p) => p,
            Err(err) => {
                return ToolResult {
                    content: err,
                    is_error: true,
                };
            }
        };
        let mode = input
            .get("edit_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("replace");
        let new_source = input
            .get("new_source")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let cell_type = input.get("cell_type").and_then(|v| v.as_str());
        let cell_id = input.get("cell_id").and_then(|v| v.as_str());

        let Ok(raw) = std::fs::read_to_string(&path) else {
            return ToolResult {
                content: format!("Failed to read {}", path.display()),
                is_error: true,
            };
        };
        let Ok(mut nb) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return ToolResult {
                content: "Invalid notebook JSON".into(),
                is_error: true,
            };
        };
        let language = nb
            .get("metadata")
            .and_then(|m| m.get("language_info"))
            .and_then(|l| l.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("python")
            .to_string();
        let Some(cells) = nb.get_mut("cells").and_then(|c| c.as_array_mut()) else {
            return ToolResult {
                content: "Notebook has no cells".into(),
                is_error: true,
            };
        };
        // Cells can be addressed by their `id` field or a numeric index.
        let index = cell_id.and_then(|id| {
            cells
                .iter()
                .position(|c| c.get("id").and_then(|i| i.as_str()) == Some(id))
                .or_else(|| id.parse::<usize>().ok().filter(|i| *i < cells.len()))
        });

        let result = match mode {
            "insert" => {
                let Some(ct) = cell_type else {
                    return ToolResult {
                        content: "cell_type is required when edit_mode=insert".into(),
                        is_error: true,
                    };
                };
                let at = index.map(|i| i + 1).unwrap_or(0);
                let mut cell = serde_json::json!({
                    "cell_type": ct,
                    "id": uuid::Uuid::new_v4().to_string(),
                    "metadata": {},
                    "source": new_source,
                });
                if ct == "code" {
                    cell["outputs"] = serde_json::json!([]);
                    cell["execution_count"] = serde_json::Value::Null;
                }
                cells.insert(at.min(cells.len()), cell);
                Ok(format!("Inserted {ct} cell at index {at}"))
            }
            "delete" => {
                let Some(i) = index else {
                    return ToolResult {
                        content: "Cell not found for delete".into(),
                        is_error: true,
                    };
                };
                cells.remove(i);
                Ok(format!("Deleted cell {i}"))
            }
            _ => {
                let Some(i) = index else {
                    return ToolResult {
                        content: "Cell not found — provide cell_id".into(),
                        is_error: true,
                    };
                };
                let cell = &mut cells[i];
                if let Some(ct) = cell_type {
                    cell["cell_type"] = serde_json::json!(ct);
                }
                cell["source"] = serde_json::json!(new_source);
                if cell.get("cell_type").and_then(|t| t.as_str()) == Some("code") {
                    cell["outputs"] = serde_json::json!([]);
                    cell["execution_count"] = serde_json::Value::Null;
                }
                Ok(format!("Replaced cell {i} (language {language})"))
            }
        };
        match result {
            Ok(msg) => match std::fs::write(&path, serde_json::to_string_pretty(&nb).unwrap()) {
                Ok(()) => ToolResult {
                    content: msg,
                    is_error: false,
                },
                Err(e) => ToolResult {
                    content: format!("Failed to write notebook: {e}"),
                    is_error: true,
                },
            },
            Err(res) => res,
        }
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    // `**` spans directories; `*`/`?` stay within a segment.
    fn seg_match(pat: &[&str], parts: &[&str]) -> bool {
        match (pat.first(), parts.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                seg_match(&pat[1..], parts) || (!parts.is_empty() && seg_match(pat, &parts[1..]))
            }
            (Some(p), Some(s)) => wildcard_match(p, s) && seg_match(&pat[1..], &parts[1..]),
            _ => false,
        }
    }
    fn wildcard_match(pat: &str, s: &str) -> bool {
        let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
        let (mut i, mut j, mut star, mut mark) = (0usize, 0usize, usize::MAX, 0usize);
        while j < s.len() {
            if i < p.len() && (p[i] == '?' || p[i] == s[j]) {
                i += 1;
                j += 1;
            } else if i < p.len() && p[i] == '*' {
                star = i;
                i += 1;
                mark = j;
            } else if star != usize::MAX {
                i = star + 1;
                mark += 1;
                j = mark;
            } else {
                return false;
            }
        }
        while i < p.len() && p[i] == '*' {
            i += 1;
        }
        i == p.len()
    }
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    seg_match(&pat_parts, &path_parts)
}

fn walk_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

impl Tool for Glob {
    fn name(&self) -> &str {
        "Glob"
    }
    fn description(&self) -> &str {
        "Find files by glob pattern (supports ** and *)"
    }
    fn needs_approval(&self) -> bool {
        false
    }
    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) else {
            return ToolResult {
                content: "Missing 'pattern' parameter".into(),
                is_error: true,
            };
        };
        let base = input
            .get("path")
            .and_then(|v| v.as_str())
            .and_then(|p| resolve_under_cwd(cwd, p).ok())
            .unwrap_or_else(|| cwd.to_path_buf());
        let mut files = Vec::new();
        walk_files(&base, &mut files);
        let mut matches: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let rel = f.strip_prefix(&base).ok()?.to_string_lossy().to_string();
                let rel_abs = f
                    .strip_prefix(cwd)
                    .unwrap_or(f)
                    .to_string_lossy()
                    .to_string();
                if glob_match(pattern, &rel) || glob_match(pattern, &rel_abs) {
                    Some(rel_abs)
                } else {
                    None
                }
            })
            .collect();
        matches.sort();
        matches.truncate(500);
        ToolResult {
            content: if matches.is_empty() {
                "No files matched".to_string()
            } else {
                matches.join("\n")
            },
            is_error: false,
        }
    }
}

impl Tool for Grep {
    fn name(&self) -> &str {
        "Grep"
    }
    fn description(&self) -> &str {
        "Search file contents for a regex pattern (file:line:match output)"
    }
    fn needs_approval(&self) -> bool {
        false
    }
    fn execute(&self, input: serde_json::Value, cwd: &Path) -> ToolResult {
        let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) else {
            return ToolResult {
                content: "Missing 'pattern' parameter".into(),
                is_error: true,
            };
        };
        let base = input
            .get("path")
            .and_then(|v| v.as_str())
            .and_then(|p| resolve_under_cwd(cwd, p).ok())
            .unwrap_or_else(|| cwd.to_path_buf());
        // Prefer system grep/ripgrep for real regex support.
        let output = std::process::Command::new("grep")
            .args(["-rInE", "--color=never", pattern, "."])
            .current_dir(&base)
            .output();
        match output {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                if text.len() > 100_000 {
                    text.truncate(100_000);
                    text.push_str("\n[truncated]");
                }
                ToolResult {
                    content: if text.trim().is_empty() {
                        "No matches found".to_string()
                    } else {
                        text
                    },
                    is_error: false,
                }
            }
            Err(_) => ToolResult {
                content: "grep not available".into(),
                is_error: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("clawed_tools_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_file_reads_existing_file() {
        let dir = temp_dir();
        let file = dir.join("hello.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = ReadFile;
        let result = tool.execute(serde_json::json!({"path": "hello.txt"}), &dir);

        assert!(!result.is_error);
        assert_eq!(result.content, "hello world");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_file_errors_on_missing_file() {
        let dir = temp_dir();
        let tool = ReadFile;
        let result = tool.execute(serde_json::json!({"path": "nonexistent.txt"}), &dir);

        assert!(result.is_error);
        assert!(result.content.contains("Cannot resolve path"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_file_errors_on_missing_param() {
        let dir = temp_dir();
        let tool = ReadFile;
        let result = tool.execute(serde_json::json!({}), &dir);

        assert!(result.is_error);
        assert!(result.content.contains("Missing 'path'"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_file_blocks_path_escape() {
        let dir = temp_dir();
        let tool = ReadFile;
        let result = tool.execute(serde_json::json!({"path": "../../../etc/passwd"}), &dir);

        assert!(result.is_error);
        assert!(
            result.content.contains("escapes working directory")
                || result.content.contains("must not contain '..'")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shell_runs_echo() {
        let dir = temp_dir();
        let tool = Shell;
        let result = tool.execute(serde_json::json!({"command": "echo hello"}), &dir);

        assert!(!result.is_error);
        assert!(result.content.contains("hello"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shell_command_spec_prefers_bash_when_available() {
        let spec = shell_command_spec("echo hello", true);

        if cfg!(windows) {
            assert_eq!(spec.program, "cmd");
            assert_eq!(spec.args, vec!["/C".to_string(), "echo hello".to_string()]);
        } else {
            assert_eq!(spec.program, "bash");
            assert_eq!(spec.args, vec!["-lc".to_string(), "echo hello".to_string()]);
        }
    }

    #[test]
    fn shell_command_spec_falls_back_to_sh_when_bash_is_unavailable() {
        let spec = shell_command_spec("echo hello", false);

        if cfg!(windows) {
            assert_eq!(spec.program, "cmd");
            assert_eq!(spec.args, vec!["/C".to_string(), "echo hello".to_string()]);
        } else {
            assert_eq!(spec.program, "sh");
            assert_eq!(spec.args, vec!["-c".to_string(), "echo hello".to_string()]);
        }
    }

    #[test]
    fn shell_errors_on_bad_command() {
        let dir = temp_dir();
        let tool = Shell;
        let result = tool.execute(serde_json::json!({"command": "exit 42"}), &dir);

        assert!(result.is_error);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shell_errors_on_missing_param() {
        let dir = temp_dir();
        let tool = Shell;
        let result = tool.execute(serde_json::json!({}), &dir);

        assert!(result.is_error);
        assert!(result.content.contains("Missing 'command'"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builtin_tool_instances_returns_tools() {
        let tools = builtin_tool_instances();
        assert!(tools.len() >= 3);
        let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"apply_patch"));
    }

    #[test]
    fn truncate_respects_byte_limit() {
        let s = "hello world";
        assert_eq!(truncate(s, 100), "hello world");
        let truncated = truncate(s, 5);
        assert!(truncated.len() > 5); // includes "... [truncated]"
        assert!(truncated.contains("... [truncated]"));
    }

    #[test]
    fn apply_patch_add_update_delete_roundtrip() {
        let dir = temp_dir();

        let tool = ApplyPatch;

        let add_patch = r#"*** Begin Patch
*** Add File: hello.txt
+hello
*** End Patch"#;
        let res = tool.execute(serde_json::json!({ "patch": add_patch }), &dir);
        assert!(!res.is_error, "{:?}", res.content);
        assert!(dir.join("hello.txt").exists());

        let update_patch = r#"*** Begin Patch
*** Update File: hello.txt
@@
-hello
+hello world
*** End Patch"#;
        let res = tool.execute(serde_json::json!({ "patch": update_patch }), &dir);
        assert!(!res.is_error, "{:?}", res.content);
        let contents = std::fs::read_to_string(dir.join("hello.txt")).unwrap();
        assert!(contents.contains("hello world"));

        let delete_patch = r#"*** Begin Patch
*** Delete File: hello.txt
*** End Patch"#;
        let res = tool.execute(serde_json::json!({ "patch": delete_patch }), &dir);
        assert!(!res.is_error, "{:?}", res.content);
        assert!(!dir.join("hello.txt").exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
