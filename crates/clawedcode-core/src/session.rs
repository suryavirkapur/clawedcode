use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
        self.messages.push(Message {
            role,
            content: content.into(),
            created_at: self.updated_at,
        });
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
}
