use crate::content::ContentBlock;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize)]
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
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            cwd,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }

    pub fn push(&mut self, role: Role, content: impl Into<String>) {
        self.updated_at = Utc::now();
        self.messages.push(Message::from_text(role, content));
    }

    pub fn push_blocks(&mut self, role: Role, blocks: Vec<ContentBlock>) {
        self.updated_at = Utc::now();
        self.messages.push(Message::from_blocks(role, blocks));
    }

    pub fn save(&self, base_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(base_dir)
            .with_context(|| format!("failed to create {}", base_dir.display()))?;
        let path = base_dir.join(format!("{}.json", self.id));
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    pub fn load(base_dir: &Path, session_id: Uuid) -> Result<Self> {
        let path = base_dir.join(format!("{}.json", session_id));
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse session from {}", path.display()))
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
}
