use crate::config::default_data_dir;
use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

const MAX_ONBOARDING_SEEN_COUNT: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingStep {
    pub key: &'static str,
    pub text: &'static str,
    pub is_complete: bool,
    pub is_completable: bool,
    pub is_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectOnboardingState {
    pub has_completed_project_onboarding: bool,
    pub project_onboarding_seen_count: u32,
}

fn onboarding_state_dir() -> Option<PathBuf> {
    std::env::var_os("CLAWEDCODE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(default_data_dir)
        .map(|dir| dir.join("project-onboarding"))
}

fn project_state_path(cwd: &Path) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    cwd.hash(&mut hasher);
    let key = hasher.finish();
    onboarding_state_dir().map(|dir| dir.join(format!("{key:016x}.json")))
}

fn load_state(cwd: &Path) -> ProjectOnboardingState {
    let Some(path) = project_state_path(cwd) else {
        return ProjectOnboardingState::default();
    };

    let Ok(raw) = fs::read_to_string(path) else {
        return ProjectOnboardingState::default();
    };

    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_state(cwd: &Path, state: &ProjectOnboardingState) {
    let Some(path) = project_state_path(cwd) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(raw) = serde_json::to_string_pretty(state) else {
        return;
    };
    let _ = fs::write(path, raw);
}

fn workspace_has_claude_md(cwd: &Path) -> bool {
    cwd.join("CLAUDE.md").exists()
}

fn is_workspace_dir_empty(cwd: &Path) -> bool {
    let Ok(mut entries) = fs::read_dir(cwd) else {
        return false;
    };
    entries.next().is_none()
}

pub fn onboarding_steps(cwd: &Path) -> Vec<OnboardingStep> {
    let has_claude_md = workspace_has_claude_md(cwd);
    let is_empty = is_workspace_dir_empty(cwd);

    vec![
        OnboardingStep {
            key: "workspace",
            text: "Ask ClawedCode to create a new app or clone a repository",
            is_complete: false,
            is_completable: true,
            is_enabled: is_empty,
        },
        OnboardingStep {
            key: "claudemd",
            text: "Run /init to create a CLAUDE.md file with instructions for ClawedCode",
            is_complete: has_claude_md,
            is_completable: true,
            is_enabled: !is_empty,
        },
    ]
}

pub fn is_project_onboarding_complete(cwd: &Path) -> bool {
    onboarding_steps(cwd)
        .into_iter()
        .filter(|step| step.is_completable && step.is_enabled)
        .all(|step| step.is_complete)
}

pub fn maybe_mark_project_onboarding_complete(cwd: &Path) {
    let mut state = load_state(cwd);
    if state.has_completed_project_onboarding {
        return;
    }
    if is_project_onboarding_complete(cwd) {
        state.has_completed_project_onboarding = true;
        save_state(cwd, &state);
    }
}

pub fn should_show_project_onboarding(cwd: &Path) -> bool {
    let state = load_state(cwd);
    if state.has_completed_project_onboarding
        || state.project_onboarding_seen_count >= MAX_ONBOARDING_SEEN_COUNT
    {
        return false;
    }

    !is_project_onboarding_complete(cwd)
}

pub fn increment_project_onboarding_seen_count(cwd: &Path) {
    let mut state = load_state(cwd);
    state.project_onboarding_seen_count = state.project_onboarding_seen_count.saturating_add(1);
    save_state(cwd, &state);
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
        let dir = std::env::temp_dir().join(format!("clawed_onboarding_{name}_{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_workspace_shows_workspace_step() {
        let cwd = temp_dir("empty_workspace");
        let steps = onboarding_steps(&cwd);
        assert!(steps[0].is_enabled);
        assert!(!steps[1].is_enabled);
        fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn claudemd_marks_second_step_complete() {
        let cwd = temp_dir("claudemd");
        fs::write(cwd.join("CLAUDE.md"), "rules").unwrap();
        let steps = onboarding_steps(&cwd);
        assert!(steps[1].is_enabled);
        assert!(steps[1].is_complete);
        fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn onboarding_seen_count_hides_after_limit() {
        let _guard = env_lock();
        let data_dir = temp_dir("data");
        let cwd = temp_dir("cwd");
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &data_dir) };

        for _ in 0..MAX_ONBOARDING_SEEN_COUNT {
            increment_project_onboarding_seen_count(&cwd);
        }

        assert!(!should_show_project_onboarding(&cwd));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
        fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn completed_onboarding_stays_hidden() {
        let _guard = env_lock();
        let data_dir = temp_dir("completed_data");
        let cwd = temp_dir("completed_cwd");
        fs::write(cwd.join("CLAUDE.md"), "rules").unwrap();
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &data_dir) };

        maybe_mark_project_onboarding_complete(&cwd);

        assert!(!should_show_project_onboarding(&cwd));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
        fs::remove_dir_all(cwd).ok();
    }
}
