//! Plan files — the reference persists plans to `<plansDir>/<slug>.md`.
//!
//! Directory resolution like getPlansDirectory:
//!   1. `plansDirectory` in settings — relative to the project root and must
//!      stay inside it, otherwise falls back to the default
//!   2. `<claude_home>/plans/`

use std::path::{Path, PathBuf};

use crate::session::Session;

/// Lexically normalize `.`/`..` components (the dir may not exist yet, so
/// canonicalize is unavailable) — the reference's resolve()-based check.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Resolve the plans directory for this project, honoring
/// `settings.plansDirectory` (must resolve inside the project root like the
/// reference's traversal guard). Does not create the directory.
pub fn plans_dir(cwd: &Path, settings: &serde_json::Value) -> PathBuf {
    if let Some(dir) = settings
        .get("plansDirectory")
        .and_then(|v| v.as_str())
        .filter(|d| !d.is_empty())
    {
        let resolved = normalize(&cwd.join(dir));
        let base = normalize(cwd);
        if resolved.starts_with(&base) || resolved == base {
            return resolved;
        }
    }
    crate::compat::claude_home()
        .unwrap_or_else(|| PathBuf::from(".claude"))
        .join("plans")
}

/// Small word list for reference-style plan slugs.
const SLUG_WORDS: &[&str] = &[
    "amber", "brisk", "cedar", "drift", "ember", "frost", "gleam", "harbor",
    "ivory", "jade", "knot", "lumen", "moss", "north", "onyx", "prism",
    "quill", "ridge", "sable", "thorn", "umber", "vale", "willow", "zephyr",
];

/// A stable, readable slug per session (word-pair derived from the session
/// id so it survives resume, like the reference's cached word slug).
pub fn plan_slug(session: &Session) -> String {
    let raw = session.id.as_u128();
    let a = SLUG_WORDS[(raw % SLUG_WORDS.len() as u128) as usize];
    let b = SLUG_WORDS[((raw >> 7) % SLUG_WORDS.len() as u128) as usize];
    format!("{a}-{b}")
}

/// The session's plan file path — `{plansDir}/{slug}.md`.
pub fn plan_file(session: &Session, settings: &serde_json::Value) -> PathBuf {
    plans_dir(&session.cwd, settings).join(format!("{}.md", plan_slug(session)))
}

/// Ensure the plans directory exists and return the session's plan file,
/// creating an empty stub so the model can write to it directly.
pub fn ensure_plan_file(session: &Session, settings: &serde_json::Value) -> PathBuf {
    let path = plan_file(session, settings);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if !path.exists() {
        let _ = std::fs::write(&path, "");
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_dir_defaults_to_claude_home() {
        let _g = env_lock();
        let cfg = std::env::temp_dir().join(format!("clawed-plans-{}", std::process::id()));
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &cfg) };
        let dir = plans_dir(Path::new("/tmp/proj"), &serde_json::Value::Null);
        assert_eq!(dir, cfg.join("plans"));
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
    }

    #[test]
    fn plans_dir_honors_settings_and_blocks_traversal() {
        let cwd = Path::new("/tmp/proj");
        let inside = serde_json::json!({"plansDirectory": ".claude/plans"});
        assert_eq!(plans_dir(cwd, &inside), cwd.join(".claude/plans"));
        // Traversal outside the project falls back to the default dir.
        let outside = serde_json::json!({"plansDirectory": "../outside"});
        assert_ne!(plans_dir(cwd, &outside), cwd.join("../outside"));
    }

    #[test]
    fn plan_slug_is_stable_per_session() {
        let s1 = Session::new(PathBuf::from("/tmp/a"));
        let s2 = Session::new(PathBuf::from("/tmp/b"));
        assert_eq!(plan_slug(&s1), plan_slug(&s1));
        assert_ne!(plan_slug(&s1), plan_slug(&s2));
        assert!(!plan_slug(&s1).contains('/'));
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}
