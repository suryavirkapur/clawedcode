use clawedcode_mcp::McpServerConfig;
use clawedcode_tools::ToolSpec;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc;

// --- Request / Response types ---

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt_pack: String,
    pub system_prompt_name: String,
    pub system_prompt_body: String,
    pub prompt: String,
    /// Structured conversation history. Providers may prefer this over `prompt`.
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ToolSpec>,
    pub skill_count: usize,
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletionResponse {
    pub system_prompt: String,
    pub response: String,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
}

// --- Conversation message model (provider-facing) ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderMessage {
    pub role: ProviderRole,
    pub content: Vec<ProviderContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        thinking: String,
    },
}

// --- Streaming event model ---

#[derive(Debug, Clone, Serialize)]
pub struct ToolUseEvent {
    pub id: String,
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResultEvent {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageEvent {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub enum ApiEvent {
    MessageDelta { text: String },
    ThinkingDelta { text: String },
    ToolUse { tool_use: ToolUseEvent },
    ToolResult { tool_result: ToolResultEvent },
    Usage { usage: UsageEvent },
    Completed,
}

// --- Error types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderError {
    Network { message: String },
    Api { status: u16, message: String },
    Parse { message: String },
    Timeout { elapsed_ms: u64 },
    RetryExhausted { attempts: u32, last_error: String },
    Other { message: String },
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Network { message } => write!(f, "Network error: {message}"),
            ProviderError::Api { status, message } => write!(f, "API error ({status}): {message}"),
            ProviderError::Parse { message } => write!(f, "Parse error: {message}"),
            ProviderError::Timeout { elapsed_ms } => write!(f, "Timeout after {elapsed_ms}ms"),
            ProviderError::RetryExhausted {
                attempts,
                last_error,
            } => {
                write!(f, "Retry exhausted after {attempts} attempts: {last_error}")
            }
            ProviderError::Other { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

// --- Usage accounting ---

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageAccount {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub request_count: u64,
}

impl UsageAccount {
    pub fn record(&mut self, usage: &UsageEvent) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.total_cache_read_tokens += usage.cache_read_tokens;
        self.total_cache_write_tokens += usage.cache_write_tokens;
        self.request_count += 1;
    }
}

// --- Retry / Timeout envelope ---

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    pub per_request: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            per_request: Duration::from_secs(60),
        }
    }
}

// --- Provider trait (async, streaming) ---

pub type EventStream = Pin<Box<dyn Stream<Item = Result<ApiEvent, ProviderError>> + Send>>;

pub trait Provider: Send + Sync {
    fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>>;

    fn stream(&self, request: &CompletionRequest) -> EventStream;
}

// --- Boxed provider handle ---

pub type BoxedProvider = Box<dyn Provider>;

// --- Mock provider (streams with delays) ---

#[derive(Debug, Default, Clone)]
pub struct MockProvider;

impl Provider for MockProvider {
    fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>> {
        let response = mock_complete_response(request);
        Box::pin(async move { Ok(response) })
    }

    fn stream(&self, request: &CompletionRequest) -> EventStream {
        let req = request.clone();
        let (tx, rx) = mpsc::channel::<Result<ApiEvent, ProviderError>>(32);

        tokio::spawn(async move {
            let events = mock_stream_events(&req);
            for (idx, event) in events.into_iter().enumerate() {
                if idx > 0 {
                    tokio::time::sleep(Duration::from_millis(18)).await;
                }
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });

        Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }
}

fn mock_complete_response(request: &CompletionRequest) -> CompletionResponse {
    if wants_read_cargo_toml(&request.prompt) {
        if let Some(tool_content) = first_tool_result_content(&request.messages) {
            let response = mock_summarize_cargo_toml(&tool_content);
            return CompletionResponse {
                system_prompt: request.system_prompt_name.clone(),
                response,
                tool_count: request.tools.len(),
                skill_count: request.skill_count,
                mcp_server_count: request.mcp_servers.len(),
            };
        }

        return CompletionResponse {
            system_prompt: request.system_prompt_name.clone(),
            response: "I'll read Cargo.toml first.".to_string(),
            tool_count: request.tools.len(),
            skill_count: request.skill_count,
            mcp_server_count: request.mcp_servers.len(),
        };
    }

    let response = mock_plain_reply(request);

    CompletionResponse {
        system_prompt: request.system_prompt_name.clone(),
        response,
        tool_count: request.tools.len(),
        skill_count: request.skill_count,
        mcp_server_count: request.mcp_servers.len(),
    }
}

fn mock_stream_events(request: &CompletionRequest) -> Vec<ApiEvent> {
    if wants_read_cargo_toml(&request.prompt) {
        if let Some(tool_content) = first_tool_result_content(&request.messages) {
            return vec![
                ApiEvent::ThinkingDelta {
                    text: "I have the Cargo.toml contents; summarizing.".to_string(),
                },
                ApiEvent::MessageDelta {
                    text: mock_summarize_cargo_toml(&tool_content),
                },
                ApiEvent::Usage {
                    usage: UsageEvent {
                        input_tokens: 220,
                        output_tokens: 90,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                    },
                },
                ApiEvent::Completed,
            ];
        }

        return vec![
            ApiEvent::ThinkingDelta {
                text: "I should read Cargo.toml to answer this.".to_string(),
            },
            ApiEvent::ToolUse {
                tool_use: ToolUseEvent {
                    id: "tool_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "Cargo.toml"}).to_string(),
                },
            },
            ApiEvent::Completed,
        ];
    }

    vec![
        ApiEvent::ThinkingDelta {
            text: "Thinking about how to help...".to_string(),
        },
        ApiEvent::MessageDelta {
            text: mock_plain_reply(request),
        },
        ApiEvent::Usage {
            usage: UsageEvent {
                input_tokens: 80,
                output_tokens: 40,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        },
        ApiEvent::Completed,
    ]
}

fn mock_plain_reply(request: &CompletionRequest) -> String {
    let prompt = request.prompt.trim().to_ascii_lowercase();

    if prompt.is_empty() {
        return "Hello! How can I help you today?".to_string();
    }

    if ["hello", "hi", "hey"]
        .iter()
        .any(|greeting| prompt == *greeting)
    {
        return "Hello! How can I assist you today?".to_string();
    }

    if prompt.contains("how are you") {
        return "I'm doing well, thank you! What can I help you with?".to_string();
    }

    "I received your message. To get started with real AI-powered assistance, configure a provider like Claude or Ollama.".to_string()
}

fn wants_read_cargo_toml(prompt: &str) -> bool {
    let p = prompt.to_ascii_lowercase();
    p.contains("cargo.toml") && (p.contains("read") || p.contains("summarize"))
}

fn first_tool_result_content(messages: &[ProviderMessage]) -> Option<String> {
    for m in messages {
        for b in &m.content {
            if let ProviderContentBlock::ToolResult { content, .. } = b {
                return Some(content.clone());
            }
        }
    }
    None
}

fn mock_summarize_cargo_toml(contents: &str) -> String {
    if contents.contains("[workspace]") {
        let mut out = String::from("Cargo.toml defines a Rust workspace.\n");
        if contents.contains("members") {
            out.push_str("It declares workspace members; this repo is a multi-crate workspace.\n");
        }
        out.push_str("Key crates include: clawedcode (cli), clawedcode-core, clawedcode-api, clawedcode-tools, clawedcode-mcp, clawedcode-tui.");
        out
    } else {
        "Cargo.toml does not look like a workspace manifest (no [workspace] section).".to_string()
    }
}

// --- MockToolProvider: deterministic provider for tests that triggers a tool call ---

/// A mock provider that, on the first call, emits a `read_file` ToolUse for
/// `Cargo.toml`. On subsequent calls (i.e. after the runtime has appended a
/// tool_result message), it returns a final text response that references the
/// tool result.
///
/// Detection of "subsequent call" is done by checking whether the session
/// already contains a `tool` role message (injected by the runtime after tool
/// execution).
#[derive(Debug, Default, Clone)]
pub struct MockToolProvider;

impl Provider for MockToolProvider {
    fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>> {
        let response = mock_tool_complete_response(request);
        Box::pin(async move { Ok(response) })
    }

    fn stream(&self, request: &CompletionRequest) -> EventStream {
        let req = request.clone();
        let (tx, rx) = mpsc::channel::<Result<ApiEvent, ProviderError>>(32);

        tokio::spawn(async move {
            let events = mock_tool_stream_events(&req);
            for (idx, event) in events.into_iter().enumerate() {
                if idx > 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });

        Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }
}

fn mock_tool_complete_response(request: &CompletionRequest) -> CompletionResponse {
    if has_tool_result_message(request) {
        let response = "Based on the Cargo.toml file, this is a Rust workspace named 'clawedcode' with multiple crates including clawedcode-cli, clawedcode-core, clawedcode-api, clawedcode-tools, clawedcode-mcp, and clawedcode-tui.".to_string();
        return CompletionResponse {
            system_prompt: request.system_prompt_name.clone(),
            response,
            tool_count: request.tools.len(),
            skill_count: request.skill_count,
            mcp_server_count: request.mcp_servers.len(),
        };
    }

    CompletionResponse {
        system_prompt: request.system_prompt_name.clone(),
        response: "I'll read the Cargo.toml file.".to_string(),
        tool_count: request.tools.len(),
        skill_count: request.skill_count,
        mcp_server_count: request.mcp_servers.len(),
    }
}

fn mock_tool_stream_events(request: &CompletionRequest) -> Vec<ApiEvent> {
    if has_tool_result_message(request) {
        return vec![
            ApiEvent::ThinkingDelta {
                text: "I have the file contents now.".to_string(),
            },
            ApiEvent::MessageDelta {
                text: "Based on the Cargo.toml file, this is a Rust workspace named 'clawedcode' with multiple crates including clawedcode-cli, clawedcode-core, clawedcode-api, clawedcode-tools, clawedcode-mcp, and clawedcode-tui.".to_string(),
            },
            ApiEvent::Usage {
                usage: UsageEvent {
                    input_tokens: 200,
                    output_tokens: 60,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            },
            ApiEvent::Completed,
        ];
    }

    vec![
        ApiEvent::ThinkingDelta {
            text: "I should read the Cargo.toml file.".to_string(),
        },
        ApiEvent::ToolUse {
            tool_use: ToolUseEvent {
                id: "tool_1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "Cargo.toml"}).to_string(),
            },
        },
        ApiEvent::Usage {
            usage: UsageEvent {
                input_tokens: 100,
                output_tokens: 30,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        },
        ApiEvent::Completed,
    ]
}

/// Returns true if the conversation already contains tool result blocks,
/// indicating this is a re-query after tool execution.
fn has_tool_result_message(request: &CompletionRequest) -> bool {
    request.messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ProviderContentBlock::ToolResult { .. }))
    })
}

// --- Optional Anthropic provider (behind feature flag + env var) ---

#[cfg(feature = "anthropic")]
pub mod anthropic_provider {
    use super::*;
    use reqwest::Client;

    #[derive(Debug, Clone)]
    pub struct AnthropicProvider {
        client: Client,
        api_key: String,
        endpoint: String,
        anthropic_version: String,
        retry: RetryConfig,
        timeout: TimeoutConfig,
    }

    pub(crate) fn normalize_anthropic_endpoint(endpoint: &str) -> String {
        if endpoint.contains("/v1/messages") {
            endpoint.to_string()
        } else {
            let endpoint = endpoint.trim_end_matches('/');
            format!("{}/v1/messages", endpoint)
        }
    }

    impl AnthropicProvider {
        pub fn from_env() -> Option<Self> {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
                .ok()?;
            let endpoint = std::env::var("CLAWEDCODE_ANTHROPIC_ENDPOINT")
                .or_else(|_| std::env::var("ANTHROPIC_BASE_URL"))
                .unwrap_or_else(|_| "https://api.anthropic.com/v1/messages".to_string());
            let endpoint = normalize_anthropic_endpoint(&endpoint);
            let anthropic_version = std::env::var("CLAWEDCODE_ANTHROPIC_VERSION")
                .unwrap_or_else(|_| "2023-06-01".to_string());
            Some(Self {
                client: Client::new(),
                api_key,
                endpoint,
                anthropic_version,
                retry: RetryConfig::default(),
                timeout: TimeoutConfig::default(),
            })
        }

        pub fn new(api_key: String, endpoint: String) -> Self {
            Self {
                client: Client::new(),
                api_key,
                endpoint,
                anthropic_version: "2023-06-01".to_string(),
                retry: RetryConfig::default(),
                timeout: TimeoutConfig::default(),
            }
        }
    }

    fn build_anthropic_tools(tools: &[ToolSpec]) -> serde_json::Value {
        if tools.is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::Value::Array(
            tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect(),
        )
    }

    fn sanitize_messages_for_anthropic(messages: &[ProviderMessage]) -> Vec<ProviderMessage> {
        messages
            .iter()
            .map(|m| ProviderMessage {
                role: m.role.clone(),
                content: m
                    .content
                    .iter()
                    .filter(|b| !matches!(b, ProviderContentBlock::Thinking { .. }))
                    .cloned()
                    .collect(),
            })
            .filter(|m| !m.content.is_empty())
            .collect()
    }

    pub(crate) fn initial_tool_input_buffer(input: &serde_json::Value) -> String {
        match input {
            serde_json::Value::Null => String::new(),
            serde_json::Value::Object(map) if map.is_empty() => String::new(),
            serde_json::Value::String(text) if text.trim().is_empty() => String::new(),
            other => serde_json::to_string(other).unwrap_or_default(),
        }
    }

    pub(crate) fn finalize_tool_input_buffer(input: String) -> String {
        if input.trim().is_empty() {
            "{}".to_string()
        } else {
            input
        }
    }

    impl Provider for AnthropicProvider {
        fn complete(
            &self,
            request: &CompletionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>>
        {
            let req = request.clone();
            let api_key = self.api_key.clone();
            let client = self.client.clone();
            let endpoint = self.endpoint.clone();
            let anthropic_version = self.anthropic_version.clone();
            let retry = self.retry.clone();
            let timeout = self.timeout.clone();
            Box::pin(async move {
                let tools = build_anthropic_tools(&req.tools);
                let messages = sanitize_messages_for_anthropic(&req.messages);
                let messages = if messages.is_empty() {
                    serde_json::json!([{"role": "user", "content": req.prompt}])
                } else {
                    serde_json::to_value(messages).unwrap_or_else(
                        |_| serde_json::json!([{"role": "user", "content": req.prompt}]),
                    )
                };
                let mut body = serde_json::json!({
                    "model": req.model,
                    "max_tokens": 4096,
                    "system": req.system_prompt_body,
                    "messages": messages,
                });
                if !tools.is_null() {
                    body["tools"] = tools;
                }

                let mut last_err: Option<ProviderError> = None;

                for attempt in 1..=retry.max_attempts {
                    let send_fut = client
                        .post(&endpoint)
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", &anthropic_version)
                        .header("content-type", "application/json")
                        .json(&body)
                        .send();

                    let resp = match tokio::time::timeout(timeout.per_request, send_fut).await {
                        Ok(Ok(r)) => r,
                        Ok(Err(e)) => {
                            last_err = Some(ProviderError::Network {
                                message: e.to_string(),
                            });
                            if attempt < retry.max_attempts {
                                let backoff_ms = (retry.base_delay.as_millis() as u64)
                                    .saturating_mul(1u64 << (attempt - 1));
                                tokio::time::sleep(Duration::from_millis(
                                    backoff_ms.min(retry.max_delay.as_millis() as u64),
                                ))
                                .await;
                                continue;
                            }
                            break;
                        }
                        Err(_) => {
                            last_err = Some(ProviderError::Timeout {
                                elapsed_ms: timeout.per_request.as_millis() as u64,
                            });
                            if attempt < retry.max_attempts {
                                tokio::time::sleep(retry.base_delay).await;
                                continue;
                            }
                            break;
                        }
                    };

                    let status = resp.status().as_u16();
                    if !resp.status().is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        let err = ProviderError::Api {
                            status,
                            message: text,
                        };
                        last_err = Some(err);

                        // Retry 5xx; fail fast otherwise.
                        let retryable = (500..=599).contains(&status);
                        if retryable && attempt < retry.max_attempts {
                            tokio::time::sleep(retry.base_delay).await;
                            continue;
                        }
                        break;
                    }

                    let json: serde_json::Value =
                        resp.json().await.map_err(|e| ProviderError::Parse {
                            message: e.to_string(),
                        })?;

                    let response = json["content"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|block| block["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();

                    return Ok(CompletionResponse {
                        system_prompt: req.system_prompt_name.clone(),
                        response,
                        tool_count: req.tools.len(),
                        skill_count: req.skill_count,
                        mcp_server_count: req.mcp_servers.len(),
                    });
                }

                Err(match last_err {
                    Some(e) => ProviderError::RetryExhausted {
                        attempts: retry.max_attempts,
                        last_error: e.to_string(),
                    },
                    None => ProviderError::Other {
                        message: "request failed".to_string(),
                    },
                })
            })
        }

        fn stream(&self, request: &CompletionRequest) -> EventStream {
            let req = request.clone();
            let api_key = self.api_key.clone();
            let client = self.client.clone();
            let endpoint = self.endpoint.clone();
            let anthropic_version = self.anthropic_version.clone();
            let timeout = self.timeout.clone();

            let (tx, rx) = mpsc::channel::<Result<ApiEvent, ProviderError>>(64);

            tokio::spawn(async move {
                let tools = build_anthropic_tools(&req.tools);
                let messages = sanitize_messages_for_anthropic(&req.messages);
                let messages = if messages.is_empty() {
                    serde_json::json!([{"role": "user", "content": req.prompt}])
                } else {
                    serde_json::to_value(messages).unwrap_or_else(
                        |_| serde_json::json!([{"role": "user", "content": req.prompt}]),
                    )
                };
                let mut body = serde_json::json!({
                    "model": req.model,
                    "max_tokens": 4096,
                    "system": req.system_prompt_body,
                    "messages": messages,
                    "stream": true,
                });
                if !tools.is_null() {
                    body["tools"] = tools;
                }

                let send_fut = client
                    .post(&endpoint)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", &anthropic_version)
                    .header("content-type", "application/json")
                    .json(&body)
                    .send();

                let resp = match tokio::time::timeout(timeout.per_request, send_fut).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        let _ = tx
                            .send(Err(ProviderError::Network {
                                message: e.to_string(),
                            }))
                            .await;
                        return;
                    }
                    Err(_) => {
                        let _ = tx
                            .send(Err(ProviderError::Timeout {
                                elapsed_ms: timeout.per_request.as_millis() as u64,
                            }))
                            .await;
                        return;
                    }
                };

                if !resp.status().is_success() {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    let _ = tx
                        .send(Err(ProviderError::Api {
                            status,
                            message: text,
                        }))
                        .await;
                    return;
                }

                let mut buf = String::new();
                let mut bytes = resp.bytes_stream();
                use futures_util::StreamExt;

                let mut current_tool_use: Option<(String, String, String)> = None;

                while let Some(chunk) = bytes.next().await {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx
                                .send(Err(ProviderError::Network {
                                    message: e.to_string(),
                                }))
                                .await;
                            return;
                        }
                    };

                    buf.push_str(&String::from_utf8_lossy(&chunk));

                    // SSE frames are separated by a blank line.
                    while let Some(idx) = buf.find("\n\n") {
                        let frame: String = buf.drain(..(idx + 2)).collect();
                        let mut data_lines = Vec::new();
                        for line in frame.lines() {
                            let line = line.trim();
                            if let Some(rest) = line.strip_prefix("data:") {
                                let payload = rest.trim();
                                if !payload.is_empty() {
                                    data_lines.push(payload.to_string());
                                }
                            }
                        }

                        if data_lines.is_empty() {
                            continue;
                        }

                        let data = data_lines.join("\n");
                        if data == "[DONE]" {
                            continue;
                        }

                        let Ok(event) = serde_json::from_str::<serde_json::Value>(&data) else {
                            continue;
                        };
                        let typ = event["type"].as_str().unwrap_or("");
                        match typ {
                            "content_block_start" => {
                                let cb_type = event["content_block"]["type"].as_str().unwrap_or("");
                                if cb_type == "text" {
                                    if let Some(t) = event["content_block"]["text"].as_str() {
                                        if !t.is_empty() {
                                            let _ = tx
                                                .send(Ok(ApiEvent::MessageDelta {
                                                    text: t.to_string(),
                                                }))
                                                .await;
                                        }
                                    }
                                } else if cb_type == "tool_use" {
                                    let id = event["content_block"]["id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let name = event["content_block"]["name"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let input =
                                        initial_tool_input_buffer(&event["content_block"]["input"]);
                                    let input_str = input;
                                    current_tool_use = Some((id, name, input_str));
                                } else if cb_type == "thinking" {
                                    if let Some(t) = event["content_block"]["thinking"].as_str() {
                                        if !t.is_empty() {
                                            let _ = tx
                                                .send(Ok(ApiEvent::ThinkingDelta {
                                                    text: t.to_string(),
                                                }))
                                                .await;
                                        }
                                    }
                                }
                            }
                            "content_block_delta" => {
                                let delta_type = event["delta"]["type"].as_str().unwrap_or("");
                                match delta_type {
                                    "text_delta" => {
                                        if let Some(t) = event["delta"]["text"].as_str() {
                                            let _ = tx
                                                .send(Ok(ApiEvent::MessageDelta {
                                                    text: t.to_string(),
                                                }))
                                                .await;
                                        }
                                    }
                                    "thinking_delta" => {
                                        if let Some(t) = event["delta"]["thinking"].as_str() {
                                            let _ = tx
                                                .send(Ok(ApiEvent::ThinkingDelta {
                                                    text: t.to_string(),
                                                }))
                                                .await;
                                        }
                                    }
                                    "input_json_delta" => {
                                        if let Some(partial) =
                                            event["delta"]["partial_json"].as_str()
                                        {
                                            if let Some((_id, _name, input_buf)) =
                                                current_tool_use.as_mut()
                                            {
                                                input_buf.push_str(partial);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            "content_block_stop" => {
                                if let Some((id, name, input)) = current_tool_use.take() {
                                    let input = finalize_tool_input_buffer(input);
                                    let _ = tx
                                        .send(Ok(ApiEvent::ToolUse {
                                            tool_use: ToolUseEvent { id, name, input },
                                        }))
                                        .await;
                                }
                            }
                            "message_delta" => {
                                if let Some(usage) = event.get("usage") {
                                    let _ = tx
                                        .send(Ok(ApiEvent::Usage {
                                            usage: UsageEvent {
                                                input_tokens: usage["input_tokens"]
                                                    .as_u64()
                                                    .unwrap_or(0),
                                                output_tokens: usage["output_tokens"]
                                                    .as_u64()
                                                    .unwrap_or(0),
                                                cache_read_tokens: usage["cache_read_input_tokens"]
                                                    .as_u64()
                                                    .unwrap_or(0),
                                                cache_write_tokens:
                                                    usage["cache_creation_input_tokens"]
                                                        .as_u64()
                                                        .unwrap_or(0),
                                            },
                                        }))
                                        .await;
                                }
                            }
                            "message_stop" => {
                                let _ = tx.send(Ok(ApiEvent::Completed)).await;
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                let _ = tx.send(Ok(ApiEvent::Completed)).await;
            });

            Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            }))
        }
    }
}

// --- Provider factory (picks provider from env) ---

pub fn create_provider() -> BoxedProvider {
    let provider_name = std::env::var("CLAWEDCODE_PROVIDER").unwrap_or_default();

    match provider_name.as_str() {
        #[cfg(feature = "anthropic")]
        "anthropic" => {
            if let Some(p) = anthropic_provider::AnthropicProvider::from_env() {
                tracing::info!("Using Anthropic provider");
                return Box::new(p);
            }
            tracing::warn!("ANTHROPIC_API_KEY not set, falling back to mock provider");
        }
        #[cfg(not(feature = "anthropic"))]
        "anthropic" => {
            tracing::warn!(
                "Anthropic provider requested but 'anthropic' feature not enabled, falling back to mock"
            );
        }
        _ => {}
    }

    tracing::info!("Using mock provider");
    Box::new(MockProvider)
}

// --- Helper: collect stream into response ---

pub async fn collect_stream_to_response(
    stream: EventStream,
    request: &CompletionRequest,
) -> Result<CompletionResponse, ProviderError> {
    use futures_util::StreamExt;
    let mut text = String::new();
    let mut thinking = String::new();

    let mut s = stream;
    while let Some(event) = s.next().await {
        match event? {
            ApiEvent::MessageDelta { text: t } => text.push_str(&t),
            ApiEvent::ThinkingDelta { text: t } => thinking.push_str(&t),
            ApiEvent::Usage { usage: _ } => {}
            ApiEvent::Completed => break,
            ApiEvent::ToolUse { .. } | ApiEvent::ToolResult { .. } => {}
        }
    }

    Ok(CompletionResponse {
        system_prompt: request.system_prompt_name.clone(),
        response: text,
        tool_count: request.tools.len(),
        skill_count: request.skill_count,
        mcp_server_count: request.mcp_servers.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn mock_stream_has_multiple_deltas() {
        let provider = MockProvider;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            messages: vec![],
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let events: Vec<_> = provider
            .stream(&request)
            .filter_map(|e| async move { e.ok() })
            .collect()
            .await;
        assert!(!events.is_empty());

        assert!(matches!(events.last(), Some(ApiEvent::Completed)));
    }

    #[tokio::test]
    async fn mock_stream_event_ordering() {
        let provider = MockProvider;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            messages: vec![],
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let events: Vec<_> = provider
            .stream(&request)
            .filter_map(|e| async move { e.ok() })
            .collect()
            .await;

        assert!(matches!(events[0], ApiEvent::ThinkingDelta { .. }));
        assert!(matches!(events[1], ApiEvent::MessageDelta { .. }));

        let usage_idx = events
            .iter()
            .position(|e| matches!(e, ApiEvent::Usage { .. }))
            .expect("Usage event should exist");
        let completed_idx = events
            .iter()
            .position(|e| matches!(e, ApiEvent::Completed))
            .expect("Completed event should exist");
        assert!(usage_idx < completed_idx);
    }

    #[tokio::test]
    async fn mock_stream_concatenated_text_matches_complete_response() {
        let provider = MockProvider;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            messages: vec![],
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let direct = provider.complete(&request).await.unwrap();
        let events: Vec<_> = provider
            .stream(&request)
            .filter_map(|e| async move { e.ok() })
            .collect()
            .await;

        let mut text = String::new();
        for e in &events {
            if let ApiEvent::MessageDelta { text: t } = e {
                text.push_str(t);
            }
        }

        assert_eq!(text, direct.response);
    }

    #[test]
    fn usage_account_accumulates() {
        let mut account = UsageAccount::default();
        account.record(&UsageEvent {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 20,
        });
        account.record(&UsageEvent {
            input_tokens: 200,
            output_tokens: 75,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });

        assert_eq!(account.total_input_tokens, 300);
        assert_eq!(account.total_output_tokens, 125);
        assert_eq!(account.total_cache_read_tokens, 10);
        assert_eq!(account.total_cache_write_tokens, 20);
        assert_eq!(account.request_count, 2);
    }

    #[test]
    fn provider_error_display() {
        let err = ProviderError::Timeout { elapsed_ms: 5000 };
        assert!(err.to_string().contains("5000"));

        let err = ProviderError::RetryExhausted {
            attempts: 3,
            last_error: "timeout".to_string(),
        };
        assert!(err.to_string().contains("3"));
    }

    #[cfg(feature = "anthropic")]
    mod anthropic_tests {
        use crate::anthropic_provider::{
            finalize_tool_input_buffer, initial_tool_input_buffer, normalize_anthropic_endpoint,
        };

        #[test]
        fn test_normalize_anthropic_endpoint_base_url() {
            assert_eq!(
                normalize_anthropic_endpoint("http://localhost:11434"),
                "http://localhost:11434/v1/messages"
            );
        }

        #[test]
        fn test_normalize_anthropic_endpoint_trailing_slash() {
            assert_eq!(
                normalize_anthropic_endpoint("http://localhost:11434/"),
                "http://localhost:11434/v1/messages"
            );
        }

        #[test]
        fn test_normalize_anthropic_endpoint_already_has_v1() {
            assert_eq!(
                normalize_anthropic_endpoint("http://localhost:11434/v1/messages"),
                "http://localhost:11434/v1/messages"
            );
        }

        #[test]
        fn test_normalize_anthropic_endpoint_custom_path() {
            assert_eq!(
                normalize_anthropic_endpoint("https://api.anthropic.com"),
                "https://api.anthropic.com/v1/messages"
            );
        }

        #[test]
        fn initial_tool_input_buffer_drops_empty_object() {
            assert_eq!(
                initial_tool_input_buffer(&serde_json::json!({})),
                String::new()
            );
        }

        #[test]
        fn initial_tool_input_buffer_keeps_non_empty_object() {
            assert_eq!(
                initial_tool_input_buffer(&serde_json::json!({"command": "ls -la"})),
                r#"{"command":"ls -la"}"#
            );
        }

        #[test]
        fn finalize_tool_input_buffer_defaults_empty_to_object() {
            assert_eq!(finalize_tool_input_buffer(String::new()), "{}");
        }
    }
}
