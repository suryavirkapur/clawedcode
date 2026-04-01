use crate::{config::default_data_dir, session::Session};
use clawedcode_tools::ToolResult;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{fs, path::PathBuf};

pub const TASK_CREATE_TOOL_NAME: &str = "TaskCreate";
pub const TASK_LIST_TOOL_NAME: &str = "TaskList";
pub const TASK_GET_TOOL_NAME: &str = "TaskGet";
pub const TASK_UPDATE_TOOL_NAME: &str = "TaskUpdate";
const HIGH_WATER_MARK_FILE: &str = ".highwatermark";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    fn from_input(value: &str) -> Result<Option<Self>, String> {
        match value {
            "pending" => Ok(Some(Self::Pending)),
            "in_progress" => Ok(Some(Self::InProgress)),
            "completed" => Ok(Some(Self::Completed)),
            "deleted" => Ok(None),
            other => Err(format!("Invalid task status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub id: String,
    pub subject: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub status: Option<Option<TaskStatus>>,
    pub owner: Option<String>,
    pub metadata_merge: Option<Map<String, Value>>,
    pub add_blocks: Vec<String>,
    pub add_blocked_by: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskUpdateOutcome {
    pub success: bool,
    pub task_id: String,
    pub updated_fields: Vec<String>,
    pub error: Option<String>,
}

pub struct TaskStore {
    root: PathBuf,
    task_list_id: String,
}

impl TaskStore {
    pub fn for_session(session: &Session) -> Result<Self, String> {
        let root = std::env::var_os("CLAWEDCODE_DATA_DIR")
            .map(PathBuf::from)
            .or_else(default_data_dir)
            .map(|dir| dir.join("tasks"))
            .ok_or_else(|| "No task data directory available".to_string())?;
        Ok(Self::new(root, session.task_list_id.clone()))
    }

    pub fn new(root: PathBuf, task_list_id: impl Into<String>) -> Self {
        Self {
            root,
            task_list_id: task_list_id.into(),
        }
    }

    pub fn create_task(
        &self,
        subject: String,
        description: String,
        active_form: Option<String>,
        metadata: Option<Map<String, Value>>,
    ) -> Result<TaskRecord, String> {
        self.ensure_dir()?;
        let id = (self.find_highest_task_id()? + 1).to_string();
        let task = TaskRecord {
            id,
            subject,
            description,
            active_form,
            owner: None,
            status: TaskStatus::Pending,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata,
        };
        self.write_task(&task)?;
        Ok(task)
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRecord>, String> {
        let path = self.task_path(task_id);
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map(Some)
                .map_err(|err| format!("Failed to parse {}: {err}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("Failed to read {}: {err}", path.display())),
        }
    }

    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>, String> {
        let entries = match fs::read_dir(self.tasks_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(format!(
                    "Failed to read {}: {err}",
                    self.tasks_dir().display()
                ));
            }
        };

        let mut tasks = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("Failed to read task entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(&path)
                .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
            let task: TaskRecord = serde_json::from_str(&raw)
                .map_err(|err| format!("Failed to parse {}: {err}", path.display()))?;
            tasks.push(task);
        }
        tasks.sort_by(|a, b| numeric_task_key(&a.id).cmp(&numeric_task_key(&b.id)));
        Ok(tasks)
    }

    pub fn update_task(
        &self,
        task_id: &str,
        patch: TaskPatch,
    ) -> Result<TaskUpdateOutcome, String> {
        let Some(mut task) = self.get_task(task_id)? else {
            return Ok(TaskUpdateOutcome {
                success: false,
                task_id: task_id.to_string(),
                updated_fields: Vec::new(),
                error: Some("Task not found".to_string()),
            });
        };

        let mut updated_fields = Vec::new();

        if let Some(subject) = patch.subject {
            if task.subject != subject {
                task.subject = subject;
                updated_fields.push("subject".to_string());
            }
        }
        if let Some(description) = patch.description {
            if task.description != description {
                task.description = description;
                updated_fields.push("description".to_string());
            }
        }
        if let Some(active_form) = patch.active_form {
            if task.active_form.as_deref() != Some(active_form.as_str()) {
                task.active_form = Some(active_form);
                updated_fields.push("activeForm".to_string());
            }
        }
        if let Some(owner) = patch.owner {
            if task.owner.as_deref() != Some(owner.as_str()) {
                task.owner = Some(owner);
                updated_fields.push("owner".to_string());
            }
        }
        if let Some(status) = patch.status {
            match status {
                Some(status) => {
                    if task.status != status {
                        task.status = status;
                        updated_fields.push("status".to_string());
                    }
                }
                None => {
                    let deleted = self.delete_task(task_id)?;
                    return Ok(TaskUpdateOutcome {
                        success: deleted,
                        task_id: task_id.to_string(),
                        updated_fields: if deleted {
                            vec!["deleted".to_string()]
                        } else {
                            Vec::new()
                        },
                        error: if deleted {
                            None
                        } else {
                            Some("Failed to delete task".to_string())
                        },
                    });
                }
            }
        }
        if let Some(metadata_merge) = patch.metadata_merge {
            let mut merged = task.metadata.unwrap_or_default();
            for (key, value) in metadata_merge {
                if value.is_null() {
                    merged.remove(&key);
                } else {
                    merged.insert(key, value);
                }
            }
            task.metadata = if merged.is_empty() { None } else { Some(merged) };
            updated_fields.push("metadata".to_string());
        }

        if !updated_fields.is_empty() {
            self.write_task(&task)?;
        }

        let mut blocks_changed = false;
        for blocked in patch.add_blocks {
            if self.block_task(task_id, &blocked)? {
                blocks_changed = true;
            }
        }
        if blocks_changed {
            updated_fields.push("blocks".to_string());
        }

        let mut blocked_by_changed = false;
        for blocker in patch.add_blocked_by {
            if self.block_task(&blocker, task_id)? {
                blocked_by_changed = true;
            }
        }
        if blocked_by_changed {
            updated_fields.push("blockedBy".to_string());
        }

        Ok(TaskUpdateOutcome {
            success: true,
            task_id: task_id.to_string(),
            updated_fields,
            error: None,
        })
    }

    pub fn delete_task(&self, task_id: &str) -> Result<bool, String> {
        if let Ok(numeric_id) = task_id.parse::<u64>() {
            let current_mark = self.read_high_water_mark()?;
            if numeric_id > current_mark {
                self.write_high_water_mark(numeric_id)?;
            }
        }

        match fs::remove_file(self.task_path(task_id)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(format!("Failed to delete task {task_id}: {err}")),
        }

        let tasks = self.list_tasks()?;
        for mut task in tasks {
            let new_blocks: Vec<_> = task
                .blocks
                .iter()
                .filter(|id| id.as_str() != task_id)
                .cloned()
                .collect();
            let new_blocked_by: Vec<_> = task
                .blocked_by
                .iter()
                .filter(|id| id.as_str() != task_id)
                .cloned()
                .collect();
            if new_blocks != task.blocks || new_blocked_by != task.blocked_by {
                task.blocks = new_blocks;
                task.blocked_by = new_blocked_by;
                self.write_task(&task)?;
            }
        }
        Ok(true)
    }

    pub fn block_task(&self, from_task_id: &str, to_task_id: &str) -> Result<bool, String> {
        let Some(mut from_task) = self.get_task(from_task_id)? else {
            return Ok(false);
        };
        let Some(mut to_task) = self.get_task(to_task_id)? else {
            return Ok(false);
        };

        let mut changed = false;
        if !from_task.blocks.iter().any(|id| id == to_task_id) {
            from_task.blocks.push(to_task_id.to_string());
            changed = true;
        }
        if !to_task.blocked_by.iter().any(|id| id == from_task_id) {
            to_task.blocked_by.push(from_task_id.to_string());
            changed = true;
        }
        if changed {
            self.write_task(&from_task)?;
            self.write_task(&to_task)?;
        }
        Ok(changed)
    }

    fn ensure_dir(&self) -> Result<(), String> {
        fs::create_dir_all(self.tasks_dir())
            .map_err(|err| format!("Failed to create {}: {err}", self.tasks_dir().display()))
    }

    fn tasks_dir(&self) -> PathBuf {
        self.root.join(sanitize_path_component(&self.task_list_id))
    }

    fn task_path(&self, task_id: &str) -> PathBuf {
        self.tasks_dir()
            .join(format!("{}.json", sanitize_path_component(task_id)))
    }

    fn high_water_mark_path(&self) -> PathBuf {
        self.tasks_dir().join(HIGH_WATER_MARK_FILE)
    }

    fn read_high_water_mark(&self) -> Result<u64, String> {
        match fs::read_to_string(self.high_water_mark_path()) {
            Ok(raw) => Ok(raw.trim().parse::<u64>().unwrap_or(0)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(format!(
                "Failed to read {}: {err}",
                self.high_water_mark_path().display()
            )),
        }
    }

    fn write_high_water_mark(&self, value: u64) -> Result<(), String> {
        self.ensure_dir()?;
        fs::write(self.high_water_mark_path(), value.to_string()).map_err(|err| {
            format!(
                "Failed to write {}: {err}",
                self.high_water_mark_path().display()
            )
        })
    }

    fn find_highest_task_id(&self) -> Result<u64, String> {
        let from_files = self
            .list_tasks()?
            .into_iter()
            .filter_map(|task| task.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Ok(from_files.max(self.read_high_water_mark()?))
    }

    fn write_task(&self, task: &TaskRecord) -> Result<(), String> {
        self.ensure_dir()?;
        let raw = serde_json::to_string_pretty(task)
            .map_err(|err| format!("Failed to serialize task {}: {err}", task.id))?;
        fs::write(self.task_path(&task.id), raw)
            .map_err(|err| format!("Failed to write task {}: {err}", task.id))
    }
}

pub fn execute_task_tool(
    tool_name: &str,
    input: Value,
    session: &Session,
) -> Result<Option<ToolResult>, String> {
    match tool_name {
        TASK_CREATE_TOOL_NAME => execute_task_create(TaskStore::for_session(session)?, input).map(Some),
        TASK_LIST_TOOL_NAME => execute_task_list(TaskStore::for_session(session)?).map(Some),
        TASK_GET_TOOL_NAME => execute_task_get(TaskStore::for_session(session)?, input).map(Some),
        TASK_UPDATE_TOOL_NAME => {
            execute_task_update(TaskStore::for_session(session)?, input).map(Some)
        }
        _ => Ok(None),
    }
}

pub fn sanitize_path_component(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn execute_task_create(store: TaskStore, input: Value) -> Result<ToolResult, String> {
    let Some(subject) = input.get("subject").and_then(Value::as_str) else {
        return Ok(missing_param("subject"));
    };
    let Some(description) = input.get("description").and_then(Value::as_str) else {
        return Ok(missing_param("description"));
    };
    let active_form = input
        .get("activeForm")
        .and_then(Value::as_str)
        .map(str::to_string);
    let metadata = input.get("metadata").and_then(Value::as_object).cloned();
    let task = store.create_task(
        subject.to_string(),
        description.to_string(),
        active_form,
        metadata,
    )?;
    Ok(ToolResult {
        content: format!("Task #{} created successfully: {}", task.id, task.subject),
        is_error: false,
    })
}

fn execute_task_list(store: TaskStore) -> Result<ToolResult, String> {
    let tasks = store.list_tasks()?;
    if tasks.is_empty() {
        return Ok(ToolResult {
            content: "No tasks found".to_string(),
            is_error: false,
        });
    }

    let resolved_task_ids: Vec<String> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .map(|task| task.id.clone())
        .collect();

    let lines: Vec<String> = tasks
        .into_iter()
        .map(|task| {
            let owner = task
                .owner
                .as_deref()
                .map(|owner| format!(" ({owner})"))
                .unwrap_or_default();
            let blocked_by: Vec<String> = task
                .blocked_by
                .iter()
                .filter(|id| !resolved_task_ids.iter().any(|resolved| resolved == *id))
                .map(|id| format!("#{id}"))
                .collect();
            let blocked = if blocked_by.is_empty() {
                String::new()
            } else {
                format!(" [blocked by {}]", blocked_by.join(", "))
            };
            format!(
                "#{} [{}] {}{}{}",
                task.id,
                task.status.as_str(),
                task.subject,
                owner,
                blocked
            )
        })
        .collect();

    Ok(ToolResult {
        content: lines.join("\n"),
        is_error: false,
    })
}

fn execute_task_get(store: TaskStore, input: Value) -> Result<ToolResult, String> {
    let Some(task_id) = input.get("taskId").and_then(Value::as_str) else {
        return Ok(missing_param("taskId"));
    };
    let Some(task) = store.get_task(task_id)? else {
        return Ok(ToolResult {
            content: "Task not found".to_string(),
            is_error: false,
        });
    };

    let mut lines = vec![
        format!("Task #{}: {}", task.id, task.subject),
        format!("Status: {}", task.status.as_str()),
        format!("Description: {}", task.description),
    ];
    if !task.blocked_by.is_empty() {
        lines.push(format!(
            "Blocked by: {}",
            task.blocked_by
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !task.blocks.is_empty() {
        lines.push(format!(
            "Blocks: {}",
            task.blocks
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(ToolResult {
        content: lines.join("\n"),
        is_error: false,
    })
}

fn execute_task_update(store: TaskStore, input: Value) -> Result<ToolResult, String> {
    let Some(task_id) = input.get("taskId").and_then(Value::as_str) else {
        return Ok(missing_param("taskId"));
    };
    let status = match input.get("status").and_then(Value::as_str) {
        Some(status) => Some(TaskStatus::from_input(status)?),
        None => None,
    };
    let patch = TaskPatch {
        subject: input.get("subject").and_then(Value::as_str).map(str::to_string),
        description: input
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        active_form: input
            .get("activeForm")
            .and_then(Value::as_str)
            .map(str::to_string),
        status,
        owner: input.get("owner").and_then(Value::as_str).map(str::to_string),
        metadata_merge: input.get("metadata").and_then(Value::as_object).cloned(),
        add_blocks: input
            .get("addBlocks")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        add_blocked_by: input
            .get("addBlockedBy")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    };

    let outcome = store.update_task(task_id, patch)?;
    if !outcome.success {
        return Ok(ToolResult {
            content: outcome
                .error
                .unwrap_or_else(|| format!("Task #{task_id} not found")),
            is_error: false,
        });
    }
    Ok(ToolResult {
        content: format!(
            "Updated task #{} {}",
            outcome.task_id,
            outcome.updated_fields.join(", ")
        ),
        is_error: false,
    })
}

fn missing_param(name: &str) -> ToolResult {
    ToolResult {
        content: format!("Missing '{name}' parameter"),
        is_error: true,
    }
}

fn numeric_task_key(task_id: &str) -> (u8, u64, String) {
    match task_id.parse::<u64>() {
        Ok(value) => (0, value, task_id.to_string()),
        Err(_) => (1, 0, task_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use uuid::Uuid;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clawed_tasks_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_path_component_matches_ts_shape() {
        assert_eq!(sanitize_path_component("abc-123_DEF"), "abc-123_DEF");
        assert_eq!(sanitize_path_component("../weird path"), "---weird-path");
    }

    #[test]
    fn high_water_mark_prevents_reuse() {
        let root = temp_root();
        let store = TaskStore::new(root.clone(), "session-1");

        let first = store
            .create_task("First".to_string(), "one".to_string(), None, None)
            .unwrap();
        let second = store
            .create_task("Second".to_string(), "two".to_string(), None, None)
            .unwrap();
        assert_eq!(first.id, "1");
        assert_eq!(second.id, "2");
        assert!(store.delete_task("2").unwrap());

        let third = store
            .create_task("Third".to_string(), "three".to_string(), None, None)
            .unwrap();
        assert_eq!(third.id, "3");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn block_task_updates_both_sides() {
        let root = temp_root();
        let store = TaskStore::new(root.clone(), "session-1");

        let one = store
            .create_task("One".to_string(), "one".to_string(), None, None)
            .unwrap();
        let two = store
            .create_task("Two".to_string(), "two".to_string(), None, None)
            .unwrap();
        assert!(store.block_task(&one.id, &two.id).unwrap());

        let one_after = store.get_task(&one.id).unwrap().unwrap();
        let two_after = store.get_task(&two.id).unwrap().unwrap();
        assert_eq!(one_after.blocks, vec![two.id.clone()]);
        assert_eq!(two_after.blocked_by, vec![one.id.clone()]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn execute_task_tool_uses_session_task_list_id() {
        let _guard = crate::test_support::env_lock();
        let root = temp_root();
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &root) };

        let mut session = Session::new(PathBuf::from("/tmp/test"));
        session.task_list_id = "shared-list".to_string();

        let created = execute_task_tool(
            TASK_CREATE_TOOL_NAME,
            serde_json::json!({
                "subject": "Write tests",
                "description": "Port the task store"
            }),
            &session,
        )
        .unwrap()
        .unwrap();
        assert!(!created.is_error);

        let listed = execute_task_tool(TASK_LIST_TOOL_NAME, serde_json::json!({}), &session)
            .unwrap()
            .unwrap();
        assert!(listed.content.contains("#1 [pending] Write tests"));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(root).ok();
    }
}
