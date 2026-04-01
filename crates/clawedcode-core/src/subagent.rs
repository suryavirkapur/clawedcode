use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    content::ContentBlock,
    permissions::PermissionMode,
    prompt::PromptSpec,
    runtime::Runtime,
    session::{Role, Session},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SubAgentConfig {
    pub prompt: String,
    pub max_turns: usize,
    pub permission_mode: PermissionMode,
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            max_turns: 10,
            permission_mode: PermissionMode::Bypass,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub child_session_id: Uuid,
    pub summary: String,
    pub tools_executed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubAgentTaskStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct SubAgentTaskState {
    pub child_session_id: Uuid,
    pub parent_session_id: Uuid,
    pub prompt: String,
    pub status: SubAgentTaskStatus,
    pub summary: Option<String>,
    pub tools_executed: usize,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub surfaced: bool,
}

#[derive(Debug, Clone)]
pub struct CompletedSubAgentTask {
    pub child_session_id: Uuid,
    pub summary: String,
    pub tools_executed: usize,
    pub status: SubAgentTaskStatus,
}

static SUBAGENT_TASKS: Lazy<Mutex<HashMap<Uuid, SubAgentTaskState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub struct SubAgentRuntime {
    config: AppConfig,
    system_prompt: PromptSpec,
    compatibility: CompatibilitySnapshot,
    sessions_dir: PathBuf,
    cwd: PathBuf,
}

impl SubAgentRuntime {
    pub fn new(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: CompatibilitySnapshot,
        sessions_dir: PathBuf,
        cwd: PathBuf,
    ) -> Self {
        Self {
            config,
            system_prompt,
            compatibility,
            sessions_dir,
            cwd,
        }
    }

    pub fn spawn(&self, subagent: SubAgentConfig) -> Result<SubAgentResult> {
        let runtime = self.build_runtime(&subagent);
        let mut session = runtime.start_session(self.cwd.clone());
        let output = runtime.submit(&mut session, &subagent.prompt);
        session.save(&self.sessions_dir)?;

        Ok(SubAgentResult {
            child_session_id: session.id,
            summary: summarize_output(&output.response, output.tools_executed),
            tools_executed: output.tools_executed,
        })
    }

    pub fn spawn_and_link_to_parent(
        &self,
        parent_session: &mut Session,
        subagent: SubAgentConfig,
    ) -> Result<SubAgentResult> {
        let result = self.spawn_fork_from_parent(parent_session, subagent)?;

        parent_session.add_child(result.child_session_id);
        parent_session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::subagent_summary(
                result.child_session_id.to_string(),
                result.summary.clone(),
            )],
        );

        Ok(result)
    }

    pub fn spawn_and_link_to_parent_without_summary(
        &self,
        parent_session: &mut Session,
        subagent: SubAgentConfig,
    ) -> Result<SubAgentResult> {
        let result = self.spawn_fork_from_parent(parent_session, subagent)?;
        parent_session.add_child(result.child_session_id);
        Ok(result)
    }

    pub fn spawn_fork_from_parent(
        &self,
        parent_session: &Session,
        subagent: SubAgentConfig,
    ) -> Result<SubAgentResult> {
        let runtime = self.build_runtime(&subagent);
        let mut session = self.build_child_session_from_parent(parent_session);
        let output = runtime.submit(&mut session, &subagent.prompt);
        session.save(&self.sessions_dir)?;

        Ok(SubAgentResult {
            child_session_id: session.id,
            summary: summarize_output(&output.response, output.tools_executed),
            tools_executed: output.tools_executed,
        })
    }

    pub fn spawn_in_background_and_link_to_parent(
        &self,
        parent_session: &mut Session,
        subagent: SubAgentConfig,
    ) -> Result<SubAgentTaskState> {
        let mut child_session = self.build_child_session_from_parent(parent_session);
        child_session.save(&self.sessions_dir)?;

        parent_session.add_child(child_session.id);

        let state = SubAgentTaskState {
            child_session_id: child_session.id,
            parent_session_id: parent_session.id,
            prompt: subagent.prompt.clone(),
            status: SubAgentTaskStatus::Running,
            summary: None,
            tools_executed: 0,
            error: None,
            created_at: Utc::now(),
            ended_at: None,
            surfaced: false,
        };
        upsert_subagent_task(state.clone());

        let config = self.config.clone();
        let system_prompt = self.system_prompt.clone();
        let compatibility = self.compatibility.clone();
        let sessions_dir = self.sessions_dir.clone();

        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut config = config;
                config.runtime.max_turns = subagent.max_turns.min(u16::MAX as usize) as u16;
                let runtime = Runtime::with_mode(
                    config,
                    system_prompt,
                    compatibility,
                    subagent.permission_mode,
                );
                let output = runtime.submit(&mut child_session, &subagent.prompt);
                let summary = summarize_output(&output.response, output.tools_executed);
                let _ = child_session.save(&sessions_dir);
                (summary, output.tools_executed)
            }));

            match result {
                Ok((summary, tools_executed)) => {
                    finish_subagent_task(
                        state.child_session_id,
                        SubAgentTaskStatus::Completed,
                        Some(summary),
                        tools_executed,
                        None,
                    );
                }
                Err(_) => {
                    finish_subagent_task(
                        state.child_session_id,
                        SubAgentTaskStatus::Failed,
                        None,
                        0,
                        Some("Sub-agent panicked".to_string()),
                    );
                }
            }
        });

        Ok(state)
    }

    fn build_runtime(&self, subagent: &SubAgentConfig) -> Runtime {
        let mut config = self.config.clone();
        config.runtime.max_turns = subagent.max_turns.min(u16::MAX as usize) as u16;
        Runtime::with_mode(
            config,
            self.system_prompt.clone(),
            self.compatibility.clone(),
            subagent.permission_mode,
        )
    }

    fn build_child_session_from_parent(&self, parent_session: &Session) -> Session {
        let mut session = Session::new_child_with_task_list(
            parent_session.cwd.clone(),
            parent_session.id,
            parent_session.task_list_id.clone(),
            parent_session.execution_mode.clone(),
        );
        session.messages = parent_session.messages.clone();
        session.updated_at = Utc::now();
        session
    }
}

fn upsert_subagent_task(task: SubAgentTaskState) {
    let mut registry = SUBAGENT_TASKS.lock().unwrap();
    registry.insert(task.child_session_id, task);
}

fn finish_subagent_task(
    child_session_id: Uuid,
    status: SubAgentTaskStatus,
    summary: Option<String>,
    tools_executed: usize,
    error: Option<String>,
) {
    let mut registry = SUBAGENT_TASKS.lock().unwrap();
    if let Some(task) = registry.get_mut(&child_session_id) {
        task.status = status;
        task.summary = summary;
        task.tools_executed = tools_executed;
        task.error = error;
        task.ended_at = Some(Utc::now());
    }
}

pub fn list_subagent_tasks_for_parent(parent_session_id: Uuid) -> Vec<SubAgentTaskState> {
    let registry = SUBAGENT_TASKS.lock().unwrap();
    let mut tasks: Vec<_> = registry
        .values()
        .filter(|task| task.parent_session_id == parent_session_id)
        .cloned()
        .collect();
    tasks.sort_by_key(|task| task.created_at);
    tasks
}

pub fn drain_completed_subagent_tasks_for_parent(
    parent_session_id: Uuid,
) -> Vec<CompletedSubAgentTask> {
    let mut registry = SUBAGENT_TASKS.lock().unwrap();
    let mut drained = Vec::new();

    for task in registry.values_mut() {
        if task.parent_session_id != parent_session_id || task.surfaced {
            continue;
        }
        if matches!(
            task.status,
            SubAgentTaskStatus::Completed | SubAgentTaskStatus::Failed
        ) {
            task.surfaced = true;
            drained.push(CompletedSubAgentTask {
                child_session_id: task.child_session_id,
                summary: task
                    .summary
                    .clone()
                    .unwrap_or_else(|| task.error.clone().unwrap_or_else(|| "Sub-agent finished".to_string())),
                tools_executed: task.tools_executed,
                status: task.status.clone(),
            });
        }
    }

    drained.sort_by_key(|task| task.child_session_id);
    drained
}

fn summarize_output(response: &str, tools_executed: usize) -> String {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return format!("Sub-agent completed with {tools_executed} tool call(s).");
    }

    let summary: String = trimmed.chars().take(500).collect();
    if trimmed.chars().count() > 500 {
        format!("{summary}...")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::PromptSpec;
    use std::fs;

    fn make_subagent_runtime() -> (SubAgentRuntime, PathBuf) {
        let config = AppConfig::default();
        let system_prompt = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let compatibility = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let sessions_dir =
            std::env::temp_dir().join(format!("clawed_subagent_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&sessions_dir).unwrap();
        let cwd = PathBuf::from("/tmp");
        (
            SubAgentRuntime::new(config, system_prompt, compatibility, sessions_dir.clone(), cwd),
            sessions_dir,
        )
    }

    #[test]
    fn spawn_creates_child_session() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let result = runtime
            .spawn(SubAgentConfig {
                prompt: "Say hello in one word".to_string(),
                max_turns: 5,
                permission_mode: PermissionMode::Bypass,
            })
            .unwrap();

        let child_path = sessions_dir.join(format!("{}.json", result.child_session_id));
        assert!(child_path.exists());

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn spawn_result_contains_summary() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let result = runtime
            .spawn(SubAgentConfig {
                prompt: "Say hello in one word".to_string(),
                max_turns: 5,
                permission_mode: PermissionMode::Bypass,
            })
            .unwrap();

        assert!(!result.summary.is_empty());
        assert_ne!(result.child_session_id, Uuid::nil());

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn spawn_and_link_creates_parent_child_linkage() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));
        parent_session.push(Role::User, "Parent prompt");

        let result = runtime
            .spawn_and_link_to_parent(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Say hello in one word".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        assert!(parent_session.child_sessions.contains(&result.child_session_id));
        assert!(parent_session.messages.iter().any(|message| {
            message
                .content_blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::SubAgentSummary { .. }))
        }));

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn child_session_has_parent_id() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));

        runtime
            .spawn_and_link_to_parent(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Say hello in one word".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        let child_id = parent_session.child_sessions[0];
        let child_session = Session::load(&sessions_dir, child_id).unwrap();
        assert_eq!(child_session.parent_session_id, Some(parent_session.id));

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn forked_child_inherits_parent_messages() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));
        parent_session.push(Role::User, "Parent context");
        parent_session.push(Role::Assistant, "Parent reply");

        let result = runtime
            .spawn_and_link_to_parent_without_summary(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Continue from the inherited context".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        let child_session = Session::load(&sessions_dir, result.child_session_id).unwrap();
        assert!(child_session.parent_session_id == Some(parent_session.id));
        assert!(child_session
            .messages
            .iter()
            .any(|message| message.primary_text() == Some("Parent context")));
        assert!(child_session
            .messages
            .iter()
            .any(|message| message.primary_text() == Some("Parent reply")));
        assert_eq!(
            child_session.last_user_text(),
            Some("Continue from the inherited context")
        );

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn sync_link_without_summary_preserves_child_link_only() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));
        parent_session.push(Role::User, "Parent prompt");

        let result = runtime
            .spawn_and_link_to_parent_without_summary(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Do one thing".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        assert!(parent_session.child_sessions.contains(&result.child_session_id));
        assert!(!parent_session.messages.iter().any(|message| {
            message
                .content_blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::SubAgentSummary { .. }))
        }));

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn spawn_respects_max_turns() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let result = runtime
            .spawn(SubAgentConfig {
                prompt: "Count to 100".to_string(),
                max_turns: 1,
                permission_mode: PermissionMode::Bypass,
            })
            .unwrap();

        assert!(result.tools_executed <= 1);

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn background_spawn_registers_running_task() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));

        let task = runtime
            .spawn_in_background_and_link_to_parent(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Say hello in one word".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        let tasks = list_subagent_tasks_for_parent(parent_session.id);
        assert!(tasks.iter().any(|item| item.child_session_id == task.child_session_id));
        assert!(parent_session.child_sessions.contains(&task.child_session_id));

        fs::remove_dir_all(&sessions_dir).ok();
    }

    #[test]
    fn completed_background_task_drains_once() {
        let (runtime, sessions_dir) = make_subagent_runtime();
        let mut parent_session = Session::new(PathBuf::from("/tmp"));

        let task = runtime
            .spawn_in_background_and_link_to_parent(
                &mut parent_session,
                SubAgentConfig {
                    prompt: "Say hello in one word".to_string(),
                    max_turns: 5,
                    permission_mode: PermissionMode::Bypass,
                },
            )
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let tasks = list_subagent_tasks_for_parent(parent_session.id);
            if tasks.iter().any(|item| {
                item.child_session_id == task.child_session_id
                    && item.status == SubAgentTaskStatus::Completed
            }) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "background task timed out");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let first = drain_completed_subagent_tasks_for_parent(parent_session.id);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].child_session_id, task.child_session_id);

        let second = drain_completed_subagent_tasks_for_parent(parent_session.id);
        assert!(second.is_empty());

        fs::remove_dir_all(&sessions_dir).ok();
    }
}
