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

pub trait ApiClient {
    fn complete(&self, request: &CompletionRequest) -> CompletionResponse;
}

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
}
