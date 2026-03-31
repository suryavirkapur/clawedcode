use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    content::ContentBlock,
    prompt::PromptSpec,
    session::{Role, Session},
};
use clawedcode_api::{ApiClient, ApiEvent, CompletionRequest, CompletionResponse, MockApiClient};
use clawedcode_tools::{builtin_tools, ToolSpec};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Runtime {
    config: AppConfig,
    system_prompt: PromptSpec,
    tools: Vec<ToolSpec>,
    compatibility: CompatibilitySnapshot,
    api_client: MockApiClient,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeOutput {
    pub session_id: String,
    pub system_prompt: String,
    pub response: String,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
}

impl Runtime {
    pub fn new(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: CompatibilitySnapshot,
    ) -> Self {
        Self {
            config,
            system_prompt,
            tools: builtin_tools(),
            compatibility,
            api_client: MockApiClient,
        }
    }

    pub fn start_session(&self, cwd: PathBuf) -> Session {
        let mut session = Session::new(cwd);
        session.push(Role::System, self.system_prompt.body);
        session
    }

    pub fn submit(&self, session: &mut Session, prompt: &str) -> RuntimeOutput {
        session.push(Role::User, prompt);

        let request = CompletionRequest {
            model: self.config.model.clone(),
            prompt_pack: self.config.prompts.default_prompt_pack.clone(),
            system_prompt_name: self.system_prompt.name.to_string(),
            system_prompt_body: self.system_prompt.body.to_string(),
            prompt: prompt.to_string(),
            tools: self.tools.clone(),
            skill_count: self.compatibility.skills.len(),
            mcp_servers: self.compatibility.mcp_servers.clone(),
        };

        let events = self.api_client.stream(&request);

        let mut text_accum = String::new();
        let mut thinking_accum = String::new();

        for event in &events {
            match event {
                ApiEvent::MessageDelta { text } => {
                    text_accum.push_str(text);
                }
                ApiEvent::ThinkingDelta { text } => {
                    thinking_accum.push_str(text);
                }
                ApiEvent::ToolUse { .. }
                | ApiEvent::ToolResult { .. }
                | ApiEvent::Usage { .. }
                | ApiEvent::Completed => {}
            }
        }

        // Build content blocks from accumulated text
        let mut blocks: Vec<ContentBlock> = Vec::new();
        if !thinking_accum.is_empty() {
            blocks.push(ContentBlock::thinking(&thinking_accum));
        }
        if !text_accum.is_empty() {
            blocks.push(ContentBlock::text(&text_accum));
        }

        // Persist into session using content blocks
        if blocks.is_empty() {
            session.push(Role::Assistant, "");
        } else {
            session.push_blocks(Role::Assistant, blocks);
        }

        RuntimeOutput {
            session_id: session.id.to_string(),
            system_prompt: self.system_prompt.name.to_string(),
            response: text_accum,
            tool_count: request.tools.len(),
            skill_count: request.skill_count,
            mcp_server_count: request.mcp_servers.len(),
        }
    }
}

impl RuntimeOutput {
    pub fn from_api(session_id: String, response: CompletionResponse) -> Self {
        Self {
            session_id,
            system_prompt: response.system_prompt,
            response: response.response,
            tool_count: response.tool_count,
            skill_count: response.skill_count,
            mcp_server_count: response.mcp_server_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn make_runtime() -> Runtime {
        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            mcp_servers: std::collections::BTreeMap::new(),
        };
        Runtime::new(config, prompt_spec, compat)
    }

    #[test]
    fn submit_returns_same_response_text_as_direct_complete() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output = runtime.submit(&mut session, "hello");

        // Verify response is non-empty and contains expected parts
        assert!(!output.response.is_empty());
        assert!(output.response.contains("Model:"));
        assert!(output.response.contains("Prompt pack:"));
    }

    #[test]
    fn submit_persists_assistant_as_content_blocks() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        runtime.submit(&mut session, "hello");

        // Find the assistant message
        let assistant_msg = session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("Assistant message should exist");

        assert!(!assistant_msg.content_blocks.is_empty());

        // Should have at least a text block
        let has_text = assistant_msg
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. }));
        assert!(has_text, "Assistant message should contain a text block");
    }

    #[test]
    fn submit_produces_thinking_block_when_stream_has_thinking() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        runtime.submit(&mut session, "hello");

        let assistant_msg = session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("Assistant message should exist");

        let has_thinking = assistant_msg
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. }));
        assert!(
            has_thinking,
            "Assistant message should contain a thinking block"
        );
    }

    #[test]
    fn submit_response_text_matches_streamed_deltas() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output = runtime.submit(&mut session, "hello");

        // The response text should match what we'd get from concatenating MessageDelta events
        let direct = runtime.api_client.complete(&CompletionRequest {
            model: runtime.config.model.clone(),
            prompt_pack: runtime.config.prompts.default_prompt_pack.clone(),
            system_prompt_name: runtime.system_prompt.name.to_string(),
            system_prompt_body: runtime.system_prompt.body.to_string(),
            prompt: "hello".to_string(),
            tools: runtime.tools.clone(),
            skill_count: runtime.compatibility.skills.len(),
            mcp_servers: runtime.compatibility.mcp_servers.clone(),
        });

        assert_eq!(output.response, direct.response);
    }
}
