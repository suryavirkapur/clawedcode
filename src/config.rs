use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub model: String,
    pub provider: ProviderConfig,
    pub ui: UiConfig,
    pub runtime: RuntimeConfig,
    pub prompts: PromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub endpoint: Option<String>,
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub theme: String,
    pub show_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub max_turns: u16,
    pub session_history_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    pub default_system_prompt: String,
    pub default_prompt_pack: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5".to_string(),
            provider: ProviderConfig {
                endpoint: None,
                api_key_env: "OPENAI_API_KEY".to_string(),
            },
            ui: UiConfig {
                theme: "sunrise".to_string(),
                show_thinking: true,
            },
            runtime: RuntimeConfig {
                max_turns: 64,
                session_history_limit: 2_000,
            },
            prompts: PromptConfig {
                default_system_prompt: "core".to_string(),
                default_prompt_pack: "coding".to_string(),
            },
        }
    }
}

impl AppConfig {
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let path = explicit_path
            .map(PathBuf::from)
            .or_else(default_config_path);

        let Some(path) = path else {
            return Ok(Self::default());
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("failed to parse config at {}", path.display()))
    }

    pub fn write_default(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(&Self::default())?;
        fs::write(path, raw)
            .with_context(|| format!("failed to write config to {}", path.display()))
    }
}

pub fn default_data_dir() -> Option<PathBuf> {
    ProjectDirs::from("dev", "clawed", "clawedcode").map(|dirs| dirs.data_dir().to_path_buf())
}

pub fn default_config_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "clawed", "clawedcode")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}
