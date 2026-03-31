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
            let mut config = Self::default();
            apply_env_overrides(&mut config);
            return Ok(config);
        };

        if !path.exists() {
            let mut config = Self::default();
            apply_env_overrides(&mut config);
            return Ok(config);
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        apply_env_overrides(&mut config);
        Ok(config)
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

fn apply_env_overrides(config: &mut AppConfig) {
    if std::env::var("CLAWEDCODE_PROVIDER").unwrap_or_default() == "anthropic" {
        if let Ok(model) = std::env::var("ANTHROPIC_MODEL") {
            config.model = model;
        }
    }
}

pub fn default_data_dir() -> Option<PathBuf> {
    ProjectDirs::from("dev", "clawed", "clawedcode").map(|dirs| dirs.data_dir().to_path_buf())
}

pub fn default_config_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "clawed", "clawedcode")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn anthropic_model_override_applies_without_config_file() {
        // SAFETY: tests here are single-threaded and restore env before exit.
        unsafe { std::env::set_var("CLAWEDCODE_PROVIDER", "anthropic") };
        unsafe { std::env::set_var("ANTHROPIC_MODEL", "qwen3.5:4b") };

        let config = AppConfig::load(None).expect("config loads");
        assert_eq!(config.model, "qwen3.5:4b");

        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };
    }
}
