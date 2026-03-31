use clawedcode_mcp::McpServerConfig;
use clawedcode_tools::ToolSpec;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt_pack: String,
    pub system_prompt_name: String,
    pub system_prompt_body: String,
    pub prompt: String,
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

// --- API client trait ---

pub trait ApiClient {
    fn complete(&self, request: &CompletionRequest) -> CompletionResponse;
    fn stream(&self, request: &CompletionRequest) -> Vec<ApiEvent>;
}

// --- Mock implementation ---

#[derive(Debug, Default, Clone)]
pub struct MockApiClient;

impl ApiClient for MockApiClient {
    fn complete(&self, request: &CompletionRequest) -> CompletionResponse {
        let response = format!(
            "Model: {}\nPrompt pack: {}\nSystem prompt: {}\nTools: {}\nSkills discovered: {}\nMCP servers discovered: {}\n\nRequest queued for the execution loop.\n\nNext priorities:\n1. Parse instructions into an explicit task graph.\n2. Resolve tool approvals before execution.\n3. Stream structured updates into the terminal UI.",
            request.model,
            request.prompt_pack,
            request.system_prompt_name,
            request
                .tools
                .iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>()
                .join(", "),
            request.skill_count,
            request.mcp_servers.len(),
        );

        CompletionResponse {
            system_prompt: request.system_prompt_name.clone(),
            response,
            tool_count: request.tools.len(),
            skill_count: request.skill_count,
            mcp_server_count: request.mcp_servers.len(),
        }
    }

    fn stream(&self, request: &CompletionRequest) -> Vec<ApiEvent> {
        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_ref()).collect();

        vec![
            ApiEvent::ThinkingDelta {
                text: "Let me start by analyzing the request.".to_string(),
            },
            ApiEvent::MessageDelta {
                text: format!(
                    "Model: {}\nPrompt pack: {}\nSystem prompt: {}\nTools: {}\nSkills discovered: {}\nMCP servers discovered: {}\n",
                    request.model,
                    request.prompt_pack,
                    request.system_prompt_name,
                    tool_names.join(", "),
                    request.skill_count,
                    request.mcp_servers.len(),
                ),
            },
            ApiEvent::MessageDelta {
                text: "\nRequest queued for the execution loop.\n\nNext priorities:\n".to_string(),
            },
            ApiEvent::MessageDelta {
                text: "1. Parse instructions into an explicit task graph.\n".to_string(),
            },
            ApiEvent::MessageDelta {
                text: "2. Resolve tool approvals before execution.\n".to_string(),
            },
            ApiEvent::MessageDelta {
                text: "3. Stream structured updates into the terminal UI.".to_string(),
            },
            ApiEvent::Usage {
                usage: UsageEvent {
                    input_tokens: 120,
                    output_tokens: 85,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            },
            ApiEvent::Completed,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_stream_has_multiple_deltas() {
        let client = MockApiClient;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let events = client.stream(&request);
        assert!(!events.is_empty());

        // Last event must be Completed
        assert!(matches!(events.last(), Some(ApiEvent::Completed)));
    }

    #[test]
    fn mock_stream_event_ordering() {
        let client = MockApiClient;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let events = client.stream(&request);

        // First event should be ThinkingDelta
        assert!(matches!(events[0], ApiEvent::ThinkingDelta { .. }));

        // Then MessageDelta events
        assert!(matches!(events[1], ApiEvent::MessageDelta { .. }));

        // Usage before Completed
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

    #[test]
    fn mock_stream_concatenated_text_matches_complete_response() {
        let client = MockApiClient;
        let request = CompletionRequest {
            model: "test-model".to_string(),
            prompt_pack: "default".to_string(),
            system_prompt_name: "default".to_string(),
            system_prompt_body: "You are helpful.".to_string(),
            prompt: "hello".to_string(),
            tools: vec![],
            skill_count: 0,
            mcp_servers: BTreeMap::new(),
        };

        let direct = client.complete(&request);
        let events = client.stream(&request);

        let mut text = String::new();
        for e in &events {
            if let ApiEvent::MessageDelta { text: t } = e {
                text.push_str(t);
            }
        }

        assert_eq!(text, direct.response);
    }
}
