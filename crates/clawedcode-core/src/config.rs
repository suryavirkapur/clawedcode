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
                show_thinking: false,
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
    use crate::test_support::env_lock;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("clawed_config_{name}_{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn config_snapshot(config: &AppConfig) -> String {
        format!(
            "model={}\nprovider.endpoint={}\nprovider.api_key_env={}\nui.theme={}\nui.show_thinking={}\nruntime.max_turns={}\nruntime.session_history_limit={}\nprompts.default_system_prompt={}\nprompts.default_prompt_pack={}",
            config.model,
            config.provider.endpoint.as_deref().unwrap_or("<none>"),
            config.provider.api_key_env,
            config.ui.theme,
            config.ui.show_thinking,
            config.runtime.max_turns,
            config.runtime.session_history_limit,
            config.prompts.default_system_prompt,
            config.prompts.default_prompt_pack,
        )
    }

    #[test]
    fn anthropic_model_override_applies_without_config_file() {
        let _guard = env_lock();
        // SAFETY: tests here are single-threaded and restore env before exit.
        unsafe { std::env::set_var("CLAWEDCODE_PROVIDER", "anthropic") };
        unsafe { std::env::set_var("ANTHROPIC_MODEL", "qwen3.5:4b") };

        let config = AppConfig::load(None).expect("config loads");
        assert_eq!(config.model, "qwen3.5:4b");

        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };
    }

    #[test]
    fn config_load_from_file_matches_snapshot() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };

        let dir = temp_dir("snapshot");
        let config_path = dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model = "claude-3-opus"

[provider]
endpoint = "https://api.example.com/v1"
api_key_env = "MY_PROVIDER_KEY"

[ui]
theme = "midnight"
show_thinking = true

[runtime]
max_turns = 128
session_history_limit = 1000

[prompts]
default_system_prompt = "expert-coder"
default_prompt_pack = "code-review"
"#,
        )
        .unwrap();

        let config = AppConfig::load(Some(&config_path)).expect("config loads");
        assert_eq!(
            config_snapshot(&config),
            "model=claude-3-opus\nprovider.endpoint=https://api.example.com/v1\nprovider.api_key_env=MY_PROVIDER_KEY\nui.theme=midnight\nui.show_thinking=true\nruntime.max_turns=128\nruntime.session_history_limit=1000\nprompts.default_system_prompt=expert-coder\nprompts.default_prompt_pack=code-review"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_env_override_preserves_other_file_values() {
        let _guard = env_lock();
        let dir = temp_dir("env_override");
        let config_path = dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model = "file-model"

[provider]
endpoint = "https://api.example.com/v1"
api_key_env = "MY_PROVIDER_KEY"

[ui]
theme = "file-theme"
show_thinking = true

[runtime]
max_turns = 32
session_history_limit = 99

[prompts]
default_system_prompt = "custom"
default_prompt_pack = "review"
"#,
        )
        .unwrap();

        unsafe { std::env::set_var("CLAWEDCODE_PROVIDER", "anthropic") };
        unsafe { std::env::set_var("ANTHROPIC_MODEL", "env-override-model") };

        let config = AppConfig::load(Some(&config_path)).expect("config loads");
        assert_eq!(
            config_snapshot(&config),
            "model=env-override-model\nprovider.endpoint=https://api.example.com/v1\nprovider.api_key_env=MY_PROVIDER_KEY\nui.theme=file-theme\nui.show_thinking=true\nruntime.max_turns=32\nruntime.session_history_limit=99\nprompts.default_system_prompt=custom\nprompts.default_prompt_pack=review"
        );

        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_config_matches_snapshot() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        unsafe { std::env::remove_var("CLAWEDCODE_PROVIDER") };

        let missing_path = temp_dir("default").join("missing-config.toml");
        let config = AppConfig::load(Some(&missing_path)).expect("loads default config");
        assert_eq!(
            config_snapshot(&config),
            "model=gpt-5\nprovider.endpoint=<none>\nprovider.api_key_env=OPENAI_API_KEY\nui.theme=sunrise\nui.show_thinking=false\nruntime.max_turns=64\nruntime.session_history_limit=2000\nprompts.default_system_prompt=core\nprompts.default_prompt_pack=coding"
        );
    }
}
