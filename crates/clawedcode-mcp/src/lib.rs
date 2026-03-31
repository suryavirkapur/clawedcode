use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio {
        #[serde(default)]
        r#type: Option<String>,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Sse {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Http {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Ws {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Sdk {
        #[serde(rename = "type")]
        r#type: String,
        name: String,
    },
}

pub fn discover_mcp_servers(settings: &Value) -> BTreeMap<String, McpServerConfig> {
    settings
        .get("mcpServers")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}
