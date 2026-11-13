//! Auto-memory (the reference's `memdir`) — a persistent, file-based memory
//! directory whose `MEMORY.md` index is injected into the system prompt.
//!
//! Resolution order like the reference's getAutoMemPath:
//!   1. `autoMemoryDirectory` in settings
//!   2. `<claude_home>/projects/<sanitized-root>/memory/`
//!
//! Enablement like isAutoMemoryEnabled:
//!   1. `CLAUDE_CODE_DISABLE_AUTO_MEMORY` (1/true → OFF, 0/false → ON)
//!   2. `--bare` (CLAWEDCODE_SIMPLE) → OFF
//!   3. `autoMemoryEnabled` in settings
//!   4. default ON

use std::path::{Path, PathBuf};

pub const ENTRYPOINT_NAME: &str = "MEMORY.md";
pub const MAX_ENTRYPOINT_LINES: usize = 200;
pub const MAX_ENTRYPOINT_BYTES: usize = 25_000;

fn truthy(v: &str) -> bool {
    matches!(v, "1" | "true" | "yes" | "on")
}

fn falsy(v: &str) -> bool {
    matches!(v, "0" | "false" | "no" | "off")
}

/// Whether auto-memory features are enabled this session.
pub fn auto_memory_enabled(settings: &serde_json::Value) -> bool {
    if let Ok(val) = std::env::var("CLAUDE_CODE_DISABLE_AUTO_MEMORY") {
        if truthy(&val) {
            return false;
        }
        if falsy(&val) {
            return true;
        }
    }
    if std::env::var_os("CLAWEDCODE_SIMPLE").is_some() {
        return false;
    }
    if let Some(v) = settings.get("autoMemoryEnabled").and_then(|v| v.as_bool()) {
        return v;
    }
    true
}

/// `[^a-zA-Z0-9]` → `-`, like the reference's sanitizePath.
fn sanitize_slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The canonical git root so all worktrees of a repo share one memory dir.
fn canonical_git_root(cwd: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    Some(path.canonicalize().unwrap_or(path))
}

/// Resolve the auto-memory directory.
pub fn auto_memory_dir(cwd: &Path, settings: &serde_json::Value) -> PathBuf {
    if let Some(dir) = settings
        .get("autoMemoryDirectory")
        .and_then(|v| v.as_str())
        .filter(|d| !d.is_empty())
    {
        let expanded = dir.strip_prefix("~/").and_then(|rest| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
        });
        let path = expanded.unwrap_or_else(|| PathBuf::from(dir));
        return if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
    }
    let base = canonical_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let slug = sanitize_slug(&base.to_string_lossy());
    crate::compat::claude_home()
        .unwrap_or_else(|| PathBuf::from(".claude"))
        .join("projects")
        .join(slug)
        .join("memory")
}

/// Truncate MEMORY.md to the line AND byte caps, warning which cap fired —
/// the reference's truncateEntrypointContent.
pub fn truncate_entrypoint(raw: &str) -> String {
    let trimmed = raw.trim();
    let lines: Vec<&str> = trimmed.split('\n').collect();
    let line_count = lines.len();
    let byte_count = trimmed.len();
    let line_trunc = line_count > MAX_ENTRYPOINT_LINES;
    let byte_trunc = byte_count > MAX_ENTRYPOINT_BYTES;
    if !line_trunc && !byte_trunc {
        return trimmed.to_string();
    }
    let mut truncated = if line_trunc {
        lines[..MAX_ENTRYPOINT_LINES].join("\n")
    } else {
        trimmed.to_string()
    };
    if truncated.len() > MAX_ENTRYPOINT_BYTES {
        let cut = truncated[..MAX_ENTRYPOINT_BYTES]
            .rfind('\n')
            .unwrap_or(MAX_ENTRYPOINT_BYTES);
        truncated.truncate(cut.max(1));
    }
    let reason = match (line_trunc, byte_trunc) {
        (true, true) => format!("{line_count} lines and {byte_count} bytes"),
        (true, false) => format!("{line_count} lines (limit: {MAX_ENTRYPOINT_LINES})"),
        (false, true) => {
            format!("{byte_count} bytes (limit: {MAX_ENTRYPOINT_BYTES}) — index entries are too long")
        }
        _ => unreachable!(),
    };
    format!(
        "{truncated}\n\n> WARNING: {ENTRYPOINT_NAME} is {reason}. Only part of it was loaded. Keep index entries to one line under ~200 chars; move detail into topic files."
    )
}

const DIR_EXISTS_GUIDANCE: &str =
    "This directory already exists — write to it directly with the apply_patch tool (do not run mkdir or check for its existence).";

const MEMORY_FRONTMATTER_EXAMPLE: &[&str] = &[
    "```markdown",
    "---",
    "name: {{memory name}}",
    "description: {{one-line description — used to decide relevance in future conversations, so be specific}}",
    "type: {{user, feedback, project, reference}}",
    "---",
    "",
    "{{memory content — for feedback/project types, structure as: rule/fact, then **Why:** and **How to apply:** lines}}",
    "```",
];

const TYPES_SECTION: &[&str] = &[
    "## Types of memory",
    "",
    "There are several discrete types of memory that you can store in your memory system:",
    "",
    "<types>",
    "<type>",
    "    <name>user</name>",
    "    <description>Contain information about the user's role, goals, responsibilities, and knowledge. Great user memories help you tailor your future behavior to the user's preferences and perspective. Your goal in reading and writing these memories is to build up an understanding of who the user is and how you can be most helpful to them specifically. For example, you should collaborate with a senior software engineer differently than a student who is coding for the very first time. Keep in mind, that the aim here is to be helpful to the user. Avoid writing memories about the user that could be viewed as a negative judgement or that are not relevant to the work you're trying to accomplish together.</description>",
    "    <when_to_save>When you learn any details about the user's role, preferences, responsibilities, or knowledge</when_to_save>",
    "    <how_to_use>When your work should be informed by the user's profile or perspective. For example, if the user is asking you to explain a part of the code, you should answer that question in a way that is tailored to the specific details that they will find most valuable or that helps them build their mental model in relation to domain knowledge they already have.</how_to_use>",
    "</type>",
    "<type>",
    "    <name>feedback</name>",
    "    <description>Guidance the user has given you about how to approach work — both what to avoid and what to keep doing. These are a very important type of memory to read and write as they allow you to remain coherent and responsive to the way you should approach work in the project. Record from failure AND success: if you only save corrections, you will avoid past mistakes but drift away from approaches the user has already validated, and may grow overly cautious.</description>",
    "    <when_to_save>Any time the user corrects your approach or confirms a non-obvious approach worked. In both cases, save what is applicable to future conversations, especially if surprising or not obvious from the code. Include *why* so you can judge edge cases later.</when_to_save>",
    "    <how_to_use>Let these memories guide your behavior so that the user does not need to offer the same guidance twice.</how_to_use>",
    "    <body_structure>Lead with the rule itself, then a **Why:** line and a **How to apply:** line. Knowing *why* lets you judge edge cases instead of blindly following the rule.</body_structure>",
    "</type>",
    "<type>",
    "    <name>project</name>",
    "    <description>Information that you learn about ongoing work, goals, initiatives, bugs, or incidents within the project that is not otherwise derivable from the code or git history. Project memories help you understand the broader context and motivation behind the work the user is doing within this working directory.</description>",
    "    <when_to_save>When you learn who is doing what, why, or by when. These states change relatively quickly so try to keep your understanding of this up to date. Always convert relative dates in user messages to absolute dates when saving (e.g., \"Thursday\" → \"2026-03-05\"), so the memory remains interpretable after time passes.</when_to_save>",
    "    <how_to_use>Use these memories to more fully understand the details and nuance behind the user's request and make better informed suggestions.</how_to_use>",
    "    <body_structure>Lead with the fact or decision, then a **Why:** line and a **How to apply:** line. Project memories decay fast, so the why helps future-you judge whether the memory is still load-bearing.</body_structure>",
    "</type>",
    "<type>",
    "    <name>reference</name>",
    "    <description>Stores pointers to where information can be found in external systems. These memories allow you to remember where to look to find up-to-date information outside of the project directory.</description>",
    "    <when_to_save>When you learn about resources in external systems and their purpose. For example, that bugs are tracked in a specific project in Linear or that feedback can be found in a specific Slack channel.</when_to_save>",
    "    <how_to_use>When the user references an external system or information that may be in an external system.</how_to_use>",
    "</type>",
    "</types>",
    "",
];

const WHAT_NOT_TO_SAVE: &[&str] = &[
    "## What NOT to save in memory",
    "",
    "- Code patterns, conventions, architecture, file paths, or project structure — these can be derived by reading the current project state.",
    "- Git history, recent changes, or who-changed-what — `git log` / `git blame` are authoritative.",
    "- Debugging solutions or fix recipes — the fix is in the code; the commit message has the context.",
    "- Anything already documented in CLAUDE.md files.",
    "- Ephemeral task details: in-progress work, temporary state, current conversation context.",
    "",
    "These exclusions apply even when the user explicitly asks you to save. If they ask you to save a PR list or activity summary, ask what was *surprising* or *non-obvious* about it — that is the part worth keeping.",
];

const WHEN_TO_ACCESS: &[&str] = &[
    "## When to access memories",
    "- When memories seem relevant, or the user references prior-conversation work.",
    "- You MUST access memory when the user explicitly asks you to check, recall, or remember.",
    "- If the user says to *ignore* or *not use* memory: proceed as if MEMORY.md were empty. Do not apply remembered facts, cite, compare against, or mention memory content.",
    "- Memory records can become stale over time. Use memory as context for what was true at a given point in time. Before answering the user or building assumptions based solely on information in memory records, verify that the memory is still correct and up-to-date by reading the current state of the files or resources. If a recalled memory conflicts with current information, trust what you observe now — and update or remove the stale memory rather than acting on it.",
];

const TRUSTING_RECALL: &[&str] = &[
    "## Before recommending from memory",
    "",
    "A memory that names a specific function, file, or flag is a claim that it existed *when the memory was written*. It may have been renamed, removed, or never merged. Before recommending it:",
    "",
    "- If the memory names a file path: check the file exists.",
    "- If the memory names a function or flag: grep for it.",
    "- If the user is about to act on your recommendation (not just asking about history), verify first.",
    "",
    "\"The memory says X exists\" is not the same as \"X exists now.\"",
    "",
    "A memory that summarizes repo state (activity logs, architecture snapshots) is frozen in time. If the user asks about *recent* or *current* state, prefer `git log` or reading the code over recalling the snapshot.",
];

/// Build the `# auto memory` system-prompt section and ensure the memory
/// directory exists — the reference's loadMemoryPrompt (auto-only path).
/// Returns None when auto memory is disabled.
pub fn load_memory_prompt(cwd: &Path, settings: &serde_json::Value) -> Option<String> {
    if !auto_memory_enabled(settings) {
        return None;
    }
    let dir = auto_memory_dir(cwd, settings);
    let _ = std::fs::create_dir_all(&dir);
    let dir_display = dir.display().to_string();

    let mut lines: Vec<String> = vec![
        "# auto memory".to_string(),
        String::new(),
        format!("You have a persistent, file-based memory system at `{dir_display}`. {DIR_EXISTS_GUIDANCE}"),
        String::new(),
        "You should build up this memory system over time so that future conversations can have a complete picture of who the user is, how they'd like to collaborate with you, what behaviors to avoid or repeat, and the context behind the work the user gives you.".to_string(),
        String::new(),
        "If the user explicitly asks you to remember something, save it immediately as whichever type fits best. If they ask you to forget something, find and remove the relevant entry.".to_string(),
        String::new(),
    ];
    lines.extend(TYPES_SECTION.iter().map(|s| s.to_string()));
    lines.extend(WHAT_NOT_TO_SAVE.iter().map(|s| s.to_string()));
    lines.push(String::new());
    lines.extend([
        "## How to save memories".to_string(),
        String::new(),
        "Saving a memory is a two-step process:".to_string(),
        String::new(),
        "**Step 1** — write the memory to its own file (e.g., `user_role.md`, `feedback_testing.md`) using this frontmatter format:".to_string(),
        String::new(),
    ]);
    lines.extend(MEMORY_FRONTMATTER_EXAMPLE.iter().map(|s| s.to_string()));
    lines.extend([
        String::new(),
        format!("**Step 2** — add a pointer to that file in `{ENTRYPOINT_NAME}`. `{ENTRYPOINT_NAME}` is an index, not a memory — each entry should be one line, under ~150 characters: `- [Title](file.md) — one-line hook`. It has no frontmatter. Never write memory content directly into `{ENTRYPOINT_NAME}`."),
        String::new(),
        format!("- `{ENTRYPOINT_NAME}` is always loaded into your conversation context — lines after {MAX_ENTRYPOINT_LINES} will be truncated, so keep the index concise"),
        "- Keep the name, description, and type fields in memory files up-to-date with the content".to_string(),
        "- Organize memory semantically by topic, not chronologically".to_string(),
        "- Update or remove memories that turn out to be wrong or outdated".to_string(),
        "- Do not write duplicate memories. First check if there is an existing memory you can update before writing a new one.".to_string(),
        String::new(),
    ]);
    lines.extend(WHEN_TO_ACCESS.iter().map(|s| s.to_string()));
    lines.push(String::new());
    lines.extend(TRUSTING_RECALL.iter().map(|s| s.to_string()));
    lines.extend([
        String::new(),
        "## Memory and other forms of persistence".to_string(),
        "Memory is one of several persistence mechanisms available to you as you assist the user in a given conversation. The distinction is often that memory can be recalled in future conversations and should not be used for persisting information that is only useful within the scope of the current conversation.".to_string(),
        "- When to use or update a plan instead of memory: If you are about to start a non-trivial implementation task and would like to reach alignment with the user on your approach you should use a Plan rather than saving this information to memory. Similarly, if you already have a plan within the conversation and you have changed your approach persist that change by updating the plan rather than saving a memory.".to_string(),
        "- When to use or update tasks instead of memory: When you need to break your work in current conversation into discrete steps or keep track of your progress use tasks instead of saving to memory. Tasks are great for persisting information about the work that needs to be done in the current conversation, but memory should be reserved for information that will be useful in future conversations.".to_string(),
    ]);

    // The entrypoint index itself — always loaded, like the reference's
    // user-context injection of MEMORY.md.
    let entrypoint = dir.join(ENTRYPOINT_NAME);
    if let Ok(raw) = std::fs::read_to_string(&entrypoint) {
        let content = truncate_entrypoint(&raw);
        if !content.is_empty() {
            lines.push(String::new());
            lines.push(format!("Contents of `{entrypoint_display}` (your memory index):", entrypoint_display = entrypoint.display()));
            lines.push(content);
        }
    }

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn truncate_entrypoint_caps_lines() {
        let raw = (0..300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let out = truncate_entrypoint(&raw);
        assert!(out.contains("WARNING: MEMORY.md"));
        assert!(out.contains("line 199"));
        assert!(!out.contains("line 200"));
    }

    #[test]
    fn truncate_entrypoint_caps_bytes() {
        let raw = "x".repeat(30_000);
        let out = truncate_entrypoint(&raw);
        assert!(out.contains("index entries are too long"));
    }

    #[test]
    fn enabled_respects_settings_and_env() {
        let _guard = env_lock();
        assert!(auto_memory_enabled(&serde_json::Value::Null));
        assert!(!auto_memory_enabled(&serde_json::json!({"autoMemoryEnabled": false})));
        unsafe { std::env::set_var("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1") };
        assert!(!auto_memory_enabled(&serde_json::json!({"autoMemoryEnabled": true})));
        unsafe { std::env::set_var("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "0") };
        assert!(auto_memory_enabled(&serde_json::json!({"autoMemoryEnabled": false})));
        unsafe { std::env::remove_var("CLAUDE_CODE_DISABLE_AUTO_MEMORY") };
    }

    #[test]
    fn dir_respects_setting_override() {
        let _guard = env_lock();
        let cwd = PathBuf::from("/tmp/proj");
        let settings = serde_json::json!({"autoMemoryDirectory": "/tmp/custom-mem"});
        assert_eq!(
            auto_memory_dir(&cwd, &settings),
            PathBuf::from("/tmp/custom-mem")
        );
        let settings = serde_json::json!({"autoMemoryDirectory": "rel-mem"});
        assert_eq!(auto_memory_dir(&cwd, &settings), cwd.join("rel-mem"));
    }

    #[test]
    fn memory_prompt_creates_dir_and_injects_entrypoint() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!("clawed-mem-{}", std::process::id()));
        let memdir = dir.join("mem");
        let settings = serde_json::json!({
            "autoMemoryDirectory": memdir.to_string_lossy()
        });
        std::fs::create_dir_all(&memdir).unwrap();
        std::fs::write(memdir.join(ENTRYPOINT_NAME), "- [User](user.md) — data scientist\n").unwrap();
        let prompt = load_memory_prompt(&dir, &settings).unwrap();
        assert!(prompt.contains("# auto memory"));
        assert!(prompt.contains(&memdir.display().to_string()));
        assert!(prompt.contains("data scientist"));
        assert!(prompt.contains("How to save memories"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_prompt_none_when_disabled() {
        let _guard = env_lock();
        let settings = serde_json::json!({"autoMemoryEnabled": false});
        assert!(load_memory_prompt(Path::new("/tmp"), &settings).is_none());
    }
}
