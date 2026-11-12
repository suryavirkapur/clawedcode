use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

pub const TASK_OUTPUT_TOOL_NAME: &str = "TaskOutput";
pub const TASK_STOP_TOOL_NAME: &str = "TaskStop";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskResult {
    pub code: i32,
    pub interrupted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskState {
    pub id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub status: TaskStatus,
    pub description: String,
    pub output_file_path: PathBuf,
    pub output_offset: u64,
    pub notified: bool,
    pub command: String,
    pub result: Option<TaskResult>,
    pub is_backgrounded: bool,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

impl BackgroundTaskState {
    pub fn new_local_bash(
        command: String,
        description: String,
        output_dir: &PathBuf,
        session_id: impl Into<String>,
    ) -> Self {
        let id = generate_task_id();
        let output_file_path = output_dir.join(format!("{}.output", id));
        Self {
            id,
            session_id: session_id.into(),
            task_type: "local_bash".to_string(),
            status: TaskStatus::Pending,
            description,
            output_file_path,
            output_offset: 0,
            notified: false,
            command,
            result: None,
            is_backgrounded: false,
            created_at: Utc::now(),
            ended_at: None,
        }
    }
}

fn generate_task_id() -> String {
    let uuid = Uuid::new_v4();
    format!("bash-{}", &uuid.to_string().replace('-', "")[..12])
}

pub struct BackgroundTaskOutput {
    file_path: PathBuf,
    bytes_written: u64,
    capped: bool,
}

const MAX_OUTPUT_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_OUTPUT_BYTES_DISPLAY: &str = "5GB";

impl BackgroundTaskOutput {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            file_path,
            bytes_written: 0,
            capped: false,
        }
    }

    pub fn append(&mut self, content: &str) -> std::io::Result<()> {
        if self.capped {
            return Ok(());
        }
        let bytes = content.as_bytes();
        self.bytes_written += bytes.len() as u64;
        if self.bytes_written > MAX_OUTPUT_BYTES {
            self.capped = true;
            let truncation_msg = format!(
                "\n[output truncated: exceeded {} disk cap]\n",
                MAX_OUTPUT_BYTES_DISPLAY
            );
            self.append_impl(&truncation_msg)?;
        } else {
            self.append_impl(content)?;
        }
        Ok(())
    }

    fn append_impl(&mut self, content: &str) -> std::io::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)?;
        file.write_all(content.as_bytes())
    }

    pub fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }

    pub fn read_from(&self, offset: u64, max_bytes: u64) -> std::io::Result<(String, u64)> {
        let mut file = match File::open(&self.file_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((String::new(), offset));
            }
            Err(e) => return Err(e),
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; max_bytes as usize];
        let bytes_read = file.read(&mut buffer)?;
        buffer.truncate(bytes_read);
        let content = String::from_utf8_lossy(&buffer).into_owned();
        Ok((content, offset + bytes_read as u64))
    }

    pub fn read_tail(&self, max_bytes: u64) -> std::io::Result<String> {
        let metadata = match fs::metadata(&self.file_path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
            Err(e) => return Err(e),
        };
        let file_size = metadata.len();
        if file_size == 0 {
            return Ok(String::new());
        }
        let start = file_size.saturating_sub(max_bytes);
        let mut file = File::open(&self.file_path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut buffer = vec![0u8; (file_size - start) as usize];
        file.read_exact(&mut buffer)?;
        let content = String::from_utf8_lossy(&buffer).into_owned();
        if file_size > max_bytes {
            Ok(format!(
                "[{}KB of earlier output omitted]\n{}",
                (file_size - max_bytes) / 1024,
                content
            ))
        } else {
            Ok(content)
        }
    }

    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }
}

struct RunningTask {
    child: Child,
}

pub struct BackgroundTaskRegistry {
    tasks: HashMap<String, BackgroundTaskState>,
    running: HashMap<String, RunningTask>,
    output_root: PathBuf,
}

impl BackgroundTaskRegistry {
    pub fn new(output_root: PathBuf) -> Self {
        Self {
            tasks: HashMap::new(),
            running: HashMap::new(),
            output_root,
        }
    }

    pub fn spawn_background_task(
        &mut self,
        command: String,
        description: String,
        cwd: &PathBuf,
        session_id: &str,
    ) -> std::io::Result<BackgroundTaskState> {
        let output_dir = task_output_dir_for_session(&self.output_root, session_id);
        let mut task = BackgroundTaskState::new_local_bash(
            command.clone(),
            description,
            &output_dir,
            session_id,
        );

        fs::create_dir_all(&output_dir)?;
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&task.output_file_path)?;
        let stderr = stdout.try_clone()?;

        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&command);
            c
        };

        cmd.current_dir(cwd)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        let child = cmd.spawn()?;
        task.status = TaskStatus::Running;
        task.is_backgrounded = true;

        let task_id = task.id.clone();
        self.tasks.insert(task_id.clone(), task.clone());
        self.running.insert(task_id, RunningTask { child });

        Ok(task)
    }

    pub fn get_task(&self, task_id: &str) -> Option<&BackgroundTaskState> {
        self.tasks.get(task_id)
    }

    pub fn get_task_mut(&mut self, task_id: &str) -> Option<&mut BackgroundTaskState> {
        self.tasks.get_mut(task_id)
    }

    pub fn list_tasks(&self) -> Vec<&BackgroundTaskState> {
        self.tasks.values().collect()
    }

    pub fn poll_task(&mut self, task_id: &str) -> std::io::Result<Option<BackgroundTaskState>> {
        let running = match self.running.get_mut(task_id) {
            Some(r) => r,
            None => return Ok(self.tasks.get(task_id).cloned()),
        };

        match running.child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(-1);
                #[cfg(unix)]
                let interrupted = status.signal().is_some();
                #[cfg(not(unix))]
                let interrupted = false;

                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = if interrupted || code != 0 {
                        TaskStatus::Failed
                    } else {
                        TaskStatus::Completed
                    };
                    task.result = Some(TaskResult { code, interrupted });
                    task.ended_at = Some(Utc::now());
                }

                self.running.remove(task_id);
                Ok(self.tasks.get(task_id).cloned())
            }
            Ok(None) => Ok(self.tasks.get(task_id).cloned()),
            Err(e) => Err(e),
        }
    }

    pub fn stop_task(&mut self, task_id: &str) -> Option<BackgroundTaskState> {
        let mut running = self.running.remove(task_id)?;

        let _ = running.child.kill();
        let _ = running.child.wait();

        if let Some(task) = self.tasks.get_mut(task_id) {
            task.status = TaskStatus::Killed;
            task.result = Some(TaskResult {
                code: -1,
                interrupted: true,
            });
            task.ended_at = Some(Utc::now());
            return Some(task.clone());
        }
        None
    }

    pub fn read_output(&self, task_id: &str) -> std::io::Result<String> {
        let task = self
            .tasks
            .get(task_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Task not found"))?;
        let output = BackgroundTaskOutput::new(task.output_file_path.clone());
        output.read_tail(128 * 1024)
    }

    pub fn read_output_from(&self, task_id: &str, offset: u64) -> std::io::Result<(String, u64)> {
        let task = self
            .tasks
            .get(task_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Task not found"))?;
        let output = BackgroundTaskOutput::new(task.output_file_path.clone());
        output.read_from(offset, 128 * 1024)
    }
}

pub fn get_task_output_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("CLAWEDCODE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(default_data_dir)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "No data directory available")
        })?;
    Ok(base.join("task-outputs"))
}

pub fn task_output_dir_for_session(output_root: &Path, session_id: &str) -> PathBuf {
    output_root.join(session_id).join("tasks")
}

fn default_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "clawedcode").map(|p| p.data_local_dir().to_path_buf())
}

static REGISTRY: Lazy<Arc<Mutex<BackgroundTaskRegistry>>> = Lazy::new(|| {
    let output_dir = get_task_output_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("clawedcode-task-outputs"));
    Arc::new(Mutex::new(BackgroundTaskRegistry::new(output_dir)))
});

pub fn with_registry<F, T>(f: F) -> T
where
    F: FnOnce(&mut BackgroundTaskRegistry) -> T,
{
    let mut registry = REGISTRY.lock().unwrap();
    f(&mut *registry)
}

pub fn get_registry_output_dir() -> PathBuf {
    with_registry(|r| r.output_root.clone())
}

pub fn spawn_background_shell(
    command: String,
    description: String,
    cwd: PathBuf,
    session_id: &str,
) -> std::io::Result<BackgroundTaskState> {
    with_registry(|r| r.spawn_background_task(command, description, &cwd, session_id))
}

pub fn get_background_task(task_id: &str) -> Option<BackgroundTaskState> {
    with_registry(|r| r.get_task(task_id).cloned())
}

pub fn poll_background_task(task_id: &str) -> std::io::Result<Option<BackgroundTaskState>> {
    with_registry(|r| r.poll_task(task_id))
}

pub fn stop_background_task(task_id: &str) -> Option<BackgroundTaskState> {
    with_registry(|r| r.stop_task(task_id))
}

pub fn read_background_task_output(task_id: &str) -> std::io::Result<String> {
    with_registry(|r| r.read_output(task_id))
}

pub fn read_background_task_output_from(
    task_id: &str,
    offset: u64,
) -> std::io::Result<(String, u64)> {
    with_registry(|r| r.read_output_from(task_id, offset))
}

pub fn list_background_tasks() -> Vec<BackgroundTaskState> {
    with_registry(|r| r.list_tasks().into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("clawed_bg_task_{}", Uuid::new_v4()))
    }

    #[test]
    fn task_state_new_local_bash_creates_valid_task() {
        let dir = temp_dir();
        let task = BackgroundTaskState::new_local_bash(
            "echo hello".to_string(),
            "Test command".to_string(),
            &dir,
            "session-a",
        );

        assert!(task.id.starts_with("bash-"));
        assert_eq!(task.session_id, "session-a");
        assert_eq!(task.task_type, "local_bash");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.command, "echo hello");
        assert_eq!(task.description, "Test command");
        assert!(!task.is_backgrounded);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn task_output_append_and_read() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.output");

        let mut output = BackgroundTaskOutput::new(file.clone());
        output.append("line1\n").unwrap();
        output.append("line2\n").unwrap();
        output.flush().unwrap();

        let (content, offset) = output.read_from(0, 1024).unwrap();
        assert_eq!(content, "line1\nline2\n");
        assert_eq!(offset, 12);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn task_status_serialization() {
        let status = TaskStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
        let parsed: TaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TaskStatus::Running);
    }

    #[test]
    fn registry_spawn_and_get_task() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();

        let mut registry = BackgroundTaskRegistry::new(dir.clone());
        let task = registry
            .spawn_background_task(
                "echo hello".to_string(),
                "Test".to_string(),
                &dir,
                "session-a",
            )
            .unwrap();

        assert!(task.id.starts_with("bash-"));
        assert_eq!(task.session_id, "session-a");
        assert_eq!(task.status, TaskStatus::Running);
        assert!(task.is_backgrounded);
        assert!(task
            .output_file_path
            .starts_with(task_output_dir_for_session(&dir, "session-a")));

        let retrieved = registry.get_task(&task.id).unwrap();
        assert_eq!(retrieved.id, task.id);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_stop_task() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();

        let mut registry = BackgroundTaskRegistry::new(dir.clone());
        let task = registry
            .spawn_background_task(
                "sleep 10".to_string(),
                "Long running".to_string(),
                &dir,
                "session-stop",
            )
            .unwrap();

        let stopped = registry.stop_task(&task.id);
        assert!(stopped.is_some());

        let retrieved = registry.get_task(&task.id).unwrap();
        assert_eq!(retrieved.status, TaskStatus::Killed);
        assert_eq!(
            retrieved.result,
            Some(TaskResult {
                code: -1,
                interrupted: true,
            })
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completed_task_output_written() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();

        let mut registry = BackgroundTaskRegistry::new(dir.clone());
        let task = registry
            .spawn_background_task(
                "echo hello".to_string(),
                "Echo test".to_string(),
                &dir,
                "session-output",
            )
            .unwrap();

        thread::sleep(std::time::Duration::from_millis(500));

        let result = registry.poll_task(&task.id).unwrap();
        assert!(result.is_some());
        let completed = result.unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);

        let output = registry.read_output(&task.id).unwrap();
        assert!(output.contains("hello"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_output_dir_includes_session_id() {
        let dir = temp_dir();
        let path = task_output_dir_for_session(&dir, "session-123");
        assert_eq!(path, dir.join("session-123").join("tasks"));
    }

    use std::thread;
}
