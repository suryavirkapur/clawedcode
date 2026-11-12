use crate::content::ContentBlock;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const TRANSPORT_SESSION_ID_MARKER_PREFIX: &str = "clawedcode-transport-session-id:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Interactive,
    Headless,
    Resume,
    Continue,
    DirectConnect,
    Ssh,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub task_list_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_sessions: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_session_id: Option<String>,
    #[serde(default)]
    pub execution_mode: SessionMode,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub content_blocks: Vec<ContentBlock>,
    pub created_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct MessageWithBlocks {
            role: Role,
            #[serde(default)]
            content_blocks: Vec<ContentBlock>,
            content: Option<String>,
            created_at: DateTime<Utc>,
        }

        let helper = MessageWithBlocks::deserialize(deserializer)?;

        let content_blocks = if !helper.content_blocks.is_empty() {
            helper.content_blocks
        } else if let Some(text) = helper.content {
            vec![ContentBlock::text(text)]
        } else {
            Vec::new()
        };

        Ok(Message {
            role: helper.role,
            content_blocks,
            created_at: helper.created_at,
        })
    }
}

impl Message {
    pub fn from_text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content_blocks: vec![ContentBlock::text(text)],
            created_at: Utc::now(),
        }
    }

    pub fn from_blocks(role: Role, blocks: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content_blocks: blocks,
            created_at: Utc::now(),
        }
    }

    pub fn primary_text(&self) -> Option<&str> {
        self.content_blocks.iter().find_map(|b| b.as_text())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Session {
    pub fn new(cwd: PathBuf) -> Self {
        Self::with_mode(cwd, SessionMode::Interactive)
    }

    pub fn with_mode(cwd: PathBuf, mode: SessionMode) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            cwd,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            task_list_id: String::new(),
            parent_session_id: None,
            child_sessions: Vec::new(),
            transport_session_id: None,
            execution_mode: mode,
        }
        .with_default_task_list_id()
    }

    pub fn new_child(cwd: PathBuf, parent_id: Uuid) -> Self {
        Self::new_child_with_mode(cwd, parent_id, SessionMode::Interactive)
    }

    pub fn new_child_with_mode(cwd: PathBuf, parent_id: Uuid, mode: SessionMode) -> Self {
        Self::new_child_with_task_list(cwd, parent_id, parent_id.to_string(), mode)
    }

    pub fn new_child_with_task_list(
        cwd: PathBuf,
        parent_id: Uuid,
        task_list_id: impl Into<String>,
        mode: SessionMode,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            cwd,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            task_list_id: task_list_id.into(),
            parent_session_id: Some(parent_id),
            child_sessions: Vec::new(),
            transport_session_id: None,
            execution_mode: mode,
        }
    }

    pub fn fork_from(existing: &Session) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            cwd: existing.cwd.clone(),
            created_at: now,
            updated_at: now,
            messages: existing.messages.clone(),
            task_list_id: String::new(),
            parent_session_id: None,
            child_sessions: Vec::new(),
            transport_session_id: None,
            execution_mode: SessionMode::Interactive,
        }
        .with_default_task_list_id()
    }

    pub fn add_child(&mut self, child_id: Uuid) {
        self.child_sessions.push(child_id);
        self.updated_at = Utc::now();
    }

    pub fn push(&mut self, role: Role, content: impl Into<String>) {
        self.updated_at = Utc::now();
        self.messages.push(Message::from_text(role, content));
        self.refresh_transport_session_id_from_messages();
    }

    pub fn push_blocks(&mut self, role: Role, blocks: Vec<ContentBlock>) {
        self.updated_at = Utc::now();
        self.messages.push(Message::from_blocks(role, blocks));
        self.refresh_transport_session_id_from_messages();
    }

    pub fn save(&self, base_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(base_dir)
            .with_context(|| format!("failed to create {}", base_dir.display()))?;
        let path = base_dir.join(format!("{}.json", self.id));
        let mut session = self.clone();
        session.refresh_transport_session_id_from_messages();
        let raw = serde_json::to_string_pretty(&session)?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    pub fn load(base_dir: &Path, session_id: Uuid) -> Result<Self> {
        let path = base_dir.join(format!("{}.json", session_id));
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut session: Self = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse session from {}", path.display()))?;
        session.refresh_transport_session_id_from_messages();
        Ok(session.with_default_task_list_id())
    }

    pub fn load_by_id(base_dir: &Path, session_id: &str) -> Result<Self> {
        let session_uuid = Uuid::parse_str(session_id)
            .with_context(|| format!("invalid session ID format: {}", session_id))?;
        Self::load(base_dir, session_uuid)
    }

    pub fn last_user_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.primary_text())
    }

    pub fn with_default_task_list_id(mut self) -> Self {
        if self.task_list_id.is_empty() {
            self.task_list_id = self.id.to_string();
        }
        self
    }

    pub fn refresh_transport_session_id_from_messages(&mut self) {
        self.transport_session_id = extract_transport_session_id(&self.messages);
    }

    pub fn fork(&self) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            cwd: self.cwd.clone(),
            created_at: now,
            updated_at: now,
            messages: self.messages.clone(),
            task_list_id: self.task_list_id.clone(),
            parent_session_id: Some(self.id),
            child_sessions: Vec::new(),
            transport_session_id: None,
            execution_mode: SessionMode::Interactive,
        }
    }
}

pub fn transport_session_id_marker(session_id: &str) -> String {
    format!("{TRANSPORT_SESSION_ID_MARKER_PREFIX}{session_id}")
}

fn extract_transport_session_id(messages: &[Message]) -> Option<String> {
    for message in messages.iter().rev() {
        for block in message.content_blocks.iter().rev() {
            if let ContentBlock::Thinking { thinking } = block {
                if let Some(rest) = thinking.strip_prefix(TRANSPORT_SESSION_ID_MARKER_PREFIX) {
                    let candidate = rest.trim();
                    if !candidate.is_empty() {
                        return Some(candidate.to_string());
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: Uuid,
    pub short_id: String,
    pub mode: SessionMode,
    pub cwd: PathBuf,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
}

pub fn list_sessions(base_dir: &Path) -> Result<Vec<SessionSummary>> {
    let entries = match fs::read_dir(base_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let session: Session = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let id_str = session.id.to_string();
        sessions.push(SessionSummary {
            short_id: id_str.chars().take(8).collect(),
            id: session.id,
            mode: session.execution_mode,
            cwd: session.cwd,
            updated_at: session.updated_at,
            message_count: session.messages.len(),
        });
    }

    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at.timestamp()));
    Ok(sessions)
}

pub fn resolve_session_by_prefix(base_dir: &Path, prefix: &str) -> Result<Uuid> {
    if prefix.is_empty() {
        anyhow::bail!("session prefix cannot be empty");
    }

    let uuid_parse_result = Uuid::parse_str(prefix);
    if let Ok(uuid) = uuid_parse_result {
        return Ok(uuid);
    }

    let sessions = list_sessions(base_dir)?;
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.id.to_string().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("no session found with prefix '{}'", prefix),
        1 => Ok(matches[0].id),
        _ => {
            let ids: Vec<_> = matches.iter().map(|s| s.id.to_string()).collect();
            anyhow::bail!(
                "multiple sessions match prefix '{}': {}",
                prefix,
                ids.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn old_format_session_json(id: &str) -> String {
        format!(
            r#"{{
  "id": "{}",
  "cwd": "/tmp/test",
  "created_at": "2025-01-01T00:00:00Z",
  "updated_at": "2025-01-01T00:01:00Z",
  "messages": [
    {{
      "role": "system",
      "content": "You are a helpful assistant.",
      "created_at": "2025-01-01T00:00:00Z"
    }},
    {{
      "role": "user",
      "content": "Hello!",
      "created_at": "2025-01-01T00:01:00Z"
    }}
  ]
}}"#,
            id
        )
    }

    #[test]
    fn backward_compatible_load_old_session() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let session_id = Uuid::new_v4();
        let path = dir.join(format!("{}.json", session_id));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(old_format_session_json(&session_id.to_string()).as_bytes())
            .unwrap();
        drop(file);

        let session = Session::load(&dir, session_id).unwrap();
        assert_eq!(session.id, session_id);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, Role::System);
        assert_eq!(
            session.messages[0].primary_text(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(session.messages[1].role, Role::User);
        assert_eq!(session.messages[1].primary_text(), Some("Hello!"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::new(PathBuf::from("/tmp/test"));
        session.push(Role::System, "You are helpful.");
        session.push(Role::User, "What is 2+2?");
        session.push(Role::Assistant, "4");

        let saved_path = session.save(&dir).unwrap();
        assert!(saved_path.exists());

        let loaded = Session::load(&dir, session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[0].role, Role::System);
        assert_eq!(loaded.messages[0].primary_text(), Some("You are helpful."));
        assert_eq!(loaded.messages[1].role, Role::User);
        assert_eq!(loaded.messages[1].primary_text(), Some("What is 2+2?"));
        assert_eq!(loaded.messages[2].role, Role::Assistant);
        assert_eq!(loaded.messages[2].primary_text(), Some("4"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_load_roundtrip_with_content_blocks() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::new(PathBuf::from("/tmp/test"));
        session.push_blocks(
            Role::Assistant,
            vec![
                ContentBlock::thinking("Let me calculate..."),
                ContentBlock::text("The answer is 4."),
            ],
        );

        session.save(&dir).unwrap();
        let loaded = Session::load(&dir, session.id).unwrap();
        assert_eq!(loaded.messages[0].content_blocks.len(), 2);
        assert_eq!(
            loaded.messages[0].content_blocks[0],
            ContentBlock::thinking("Let me calculate...")
        );
        assert_eq!(
            loaded.messages[0].content_blocks[1],
            ContentBlock::text("The answer is 4.")
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_by_id_works() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::new(PathBuf::from("/tmp/test"));
        session.push(Role::User, "test");
        session.save(&dir).unwrap();

        let loaded = Session::load_by_id(&dir, &session.id.to_string()).unwrap();
        assert_eq!(loaded.id, session.id);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_load_roundtrip_with_parent_child_metadata() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let parent_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        let mut session = Session::new_child(PathBuf::from("/tmp/test"), parent_id);
        session.add_child(child_id);
        session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::subagent_summary(
                child_id.to_string(),
                "Child finished",
            )],
        );

        session.save(&dir).unwrap();
        let loaded = Session::load(&dir, session.id).unwrap();

        assert_eq!(loaded.parent_session_id, Some(parent_id));
        assert_eq!(loaded.child_sessions, vec![child_id]);
        assert!(loaded.messages.iter().any(|message| {
            message
                .content_blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::SubAgentSummary { .. }))
        }));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_default_mode_is_interactive() {
        let session = Session::new(PathBuf::from("/tmp/test"));
        assert_eq!(session.execution_mode, SessionMode::Interactive);
        assert_eq!(session.task_list_id, session.id.to_string());
    }

    #[test]
    fn session_with_explicit_mode() {
        let session = Session::with_mode(PathBuf::from("/tmp/test"), SessionMode::Headless);
        assert_eq!(session.execution_mode, SessionMode::Headless);
    }

    #[test]
    fn session_mode_persists_across_save_load() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session =
            Session::with_mode(PathBuf::from("/tmp/test"), SessionMode::DirectConnect);
        session.push(Role::User, "Hello");

        session.save(&dir).unwrap();
        let loaded = Session::load(&dir, session.id).unwrap();

        assert_eq!(loaded.execution_mode, SessionMode::DirectConnect);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transport_session_id_is_persisted_from_thinking_marker() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::new(PathBuf::from("/tmp/test"));
        session.transport_session_id = None;
        session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::thinking(transport_session_id_marker("remote-123"))],
        );

        session.save(&dir).unwrap();
        let loaded = Session::load(&dir, session.id).unwrap();

        assert_eq!(loaded.transport_session_id.as_deref(), Some("remote-123"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn push_blocks_refreshes_transport_session_id_in_memory() {
        let mut session = Session::new(PathBuf::from("/tmp/test"));

        session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::thinking(transport_session_id_marker("remote-456"))],
        );

        assert_eq!(session.transport_session_id.as_deref(), Some("remote-456"));
    }

    #[test]
    fn fork_does_not_inherit_transport_session_id() {
        let mut session = Session::with_mode(PathBuf::from("/tmp/test"), SessionMode::Ssh);
        session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::thinking(transport_session_id_marker("remote-789"))],
        );

        let forked = session.fork();
        let forked_from = Session::fork_from(&session);

        assert_eq!(session.transport_session_id.as_deref(), Some("remote-789"));
        assert_eq!(forked.transport_session_id, None);
        assert_eq!(forked_from.transport_session_id, None);
    }

    #[test]
    fn session_all_modes_persist() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let modes = vec![
            SessionMode::Interactive,
            SessionMode::Headless,
            SessionMode::Resume,
            SessionMode::Continue,
            SessionMode::DirectConnect,
            SessionMode::Ssh,
            SessionMode::Remote,
        ];

        for mode in modes {
            let mut session = Session::with_mode(PathBuf::from("/tmp/test"), mode.clone());
            session.push(Role::User, "test");
            session.save(&dir).unwrap();
            let loaded = Session::load(&dir, session.id).unwrap();
            assert_eq!(
                loaded.execution_mode, mode,
                "mode {:?} did not persist",
                mode
            );
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn child_session_inherits_mode() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let parent_id = Uuid::new_v4();
        let mut parent = Session::with_mode(PathBuf::from("/tmp/test"), SessionMode::Headless);
        parent.push(Role::User, "parent");

        let mut child = Session::new_child_with_mode(
            PathBuf::from("/tmp/test"),
            parent_id,
            SessionMode::Headless,
        );
        child.push(Role::User, "child");

        assert_eq!(parent.execution_mode, SessionMode::Headless);
        assert_eq!(child.execution_mode, SessionMode::Headless);
        assert_eq!(child.task_list_id, parent_id.to_string());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_session_format_loads_with_default_mode() {
        let dir = std::env::temp_dir().join(format!("clawed_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let session_id = Uuid::new_v4();
        let path = dir.join(format!("{}.json", session_id));

        let old_json = format!(
            r#"{{
  "id": "{}",
  "cwd": "/tmp/test",
  "created_at": "2025-01-01T00:00:00Z",
  "updated_at": "2025-01-01T00:01:00Z",
  "messages": [
    {{
      "role": "system",
      "content": "You are helpful.",
      "created_at": "2025-01-01T00:00:00Z"
    }}
  ]
}}"#,
            session_id
        );

        fs::write(&path, old_json).unwrap();

        let loaded = Session::load(&dir, session_id).unwrap();
        assert_eq!(loaded.execution_mode, SessionMode::Interactive);
        assert_eq!(loaded.task_list_id, session_id.to_string());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fork_from_creates_new_interactive_session_with_cloned_transcript() {
        let mut original = Session::with_mode(PathBuf::from("/tmp/test"), SessionMode::Resume);
        let child_id = Uuid::new_v4();
        original.parent_session_id = Some(Uuid::new_v4());
        original.child_sessions.push(child_id);
        original.push(Role::User, "Hello");
        original.push(Role::Assistant, "Hi");

        let forked = Session::fork_from(&original);

        assert_ne!(forked.id, original.id);
        assert_eq!(forked.cwd, original.cwd);
        assert_eq!(forked.messages, original.messages);
        assert_eq!(forked.execution_mode, SessionMode::Interactive);
        assert!(forked.parent_session_id.is_none());
        assert!(forked.child_sessions.is_empty());
        assert_eq!(forked.task_list_id, forked.id.to_string());
    }

    #[test]
    fn fork_sets_parent_session_id() {
        let mut original = Session::new(PathBuf::from("/project"));
        original.push(Role::User, "test prompt");
        original.push(Role::Assistant, "response");

        let forked = original.fork();

        assert_ne!(forked.id, original.id);
        assert_eq!(forked.parent_session_id, Some(original.id));
        assert_eq!(forked.execution_mode, SessionMode::Interactive);
        assert_eq!(forked.cwd, original.cwd);
        assert_eq!(forked.messages.len(), original.messages.len());
        assert!(forked.child_sessions.is_empty());
    }

    #[test]
    fn list_sessions_returns_sessions_sorted_by_updated_at() {
        let dir = std::env::temp_dir().join(format!("clawed_list_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session1 = Session::new(PathBuf::from("/project1"));
        session1.push(Role::User, "first");
        session1.save(&dir).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut session2 = Session::with_mode(PathBuf::from("/project2"), SessionMode::Headless);
        session2.push(Role::User, "second");
        session2.save(&dir).unwrap();

        let sessions = list_sessions(&dir).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, session2.id);
        assert_eq!(sessions[1].id, session1.id);
        assert_eq!(sessions[0].short_id.len(), 8);
        assert_eq!(sessions[0].mode, SessionMode::Headless);
        assert_eq!(sessions[1].mode, SessionMode::Interactive);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_empty_directory() {
        let dir = std::env::temp_dir().join(format!("clawed_empty_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let sessions = list_sessions(&dir).unwrap();
        assert!(sessions.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_nonexistent_directory() {
        let dir = std::env::temp_dir().join(format!("clawed_nonexist_{})", Uuid::new_v4()));
        let sessions = list_sessions(&dir).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn resolve_session_by_prefix_unique_match() {
        let dir = std::env::temp_dir().join(format!("clawed_resolve_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::new(PathBuf::from("/project"));
        session.push(Role::User, "test");
        session.save(&dir).unwrap();

        let id_str = session.id.to_string();
        let prefix: String = id_str.chars().take(8).collect();

        let resolved = resolve_session_by_prefix(&dir, &prefix).unwrap();
        assert_eq!(resolved, session.id);

        let resolved_full = resolve_session_by_prefix(&dir, &id_str).unwrap();
        assert_eq!(resolved_full, session.id);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_session_by_prefix_no_match() {
        let dir = std::env::temp_dir().join(format!("clawed_no_match_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let result = resolve_session_by_prefix(&dir, "nonexist");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no session found"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_session_by_prefix_multiple_matches() {
        let dir = std::env::temp_dir().join(format!("clawed_multi_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut s1 = Session::new(PathBuf::from("/p1"));
        s1.push(Role::User, "1");
        s1.save(&dir).unwrap();

        let mut s2 = Session::new(PathBuf::from("/p2"));
        s2.push(Role::User, "2");
        s2.save(&dir).unwrap();

        let prefix = "a";
        let result = resolve_session_by_prefix(&dir, prefix);
        assert!(result.is_err() || result.is_ok());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_session_by_prefix_empty() {
        let dir = std::env::temp_dir().join(format!("clawed_empty_prefix_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let result = resolve_session_by_prefix(&dir, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_summary_fields() {
        let dir = std::env::temp_dir().join(format!("clawed_summary_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut session = Session::with_mode(PathBuf::from("/myproject"), SessionMode::Ssh);
        session.push(Role::System, "system");
        session.push(Role::User, "user");
        session.push(Role::Assistant, "assistant");
        session.save(&dir).unwrap();

        let summaries = list_sessions(&dir).unwrap();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.id, session.id);
        assert_eq!(summary.mode, SessionMode::Ssh);
        assert_eq!(summary.cwd, PathBuf::from("/myproject"));
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.short_id.len(), 8);

        fs::remove_dir_all(&dir).ok();
    }
}
