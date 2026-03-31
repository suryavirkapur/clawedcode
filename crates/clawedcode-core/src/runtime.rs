use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    content::ContentBlock,
    permissions::{PermissionDecision, PermissionEngine, PermissionMode},
    prompt::PromptSpec,
    session::{Role, Session},
};
use clawedcode_api::{ApiClient, ApiEvent, CompletionRequest, CompletionResponse, MockApiClient};
use clawedcode_tools::{Tool, ToolSpec, builtin_tool_instances, builtin_tools};
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf};

pub struct Runtime {
    pub(crate) config: AppConfig,
    pub(crate) system_prompt: PromptSpec,
    pub(crate) tools: Vec<ToolSpec>,
    tool_instances: HashMap<String, Box<dyn Tool>>,
    pub(crate) compatibility: CompatibilitySnapshot,
    pub(crate) api_client: MockApiClient,
    permission_engine: PermissionEngine,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeOutput {
    pub session_id: String,
    pub system_prompt: String,
    pub response: String,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
    pub tools_executed: usize,
}

impl Runtime {
    pub fn new(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: CompatibilitySnapshot,
    ) -> Self {
        Self::with_mode(
            config,
            system_prompt,
            compatibility,
            PermissionMode::Default,
        )
    }

    pub fn with_mode(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: CompatibilitySnapshot,
        mode: PermissionMode,
    ) -> Self {
        let tool_instances: HashMap<String, Box<dyn Tool>> = builtin_tool_instances()
            .into_iter()
            .map(|t| (t.name().to_string(), t))
            .collect();

        Self {
            config,
            system_prompt,
            tools: builtin_tools(),
            tool_instances,
            compatibility,
            api_client: MockApiClient,
            permission_engine: PermissionEngine::new(mode),
        }
    }

    pub fn start_session(&self, cwd: PathBuf) -> Session {
        let mut session = Session::new(cwd);
        session.push(Role::System, self.system_prompt.body);
        session
    }

    pub fn submit(&self, session: &mut Session, prompt: &str) -> RuntimeOutput {
        session.push(Role::User, prompt);

        let mut tools_executed = 0usize;

        loop {
            let request = CompletionRequest {
                model: self.config.model.clone(),
                prompt_pack: self.config.prompts.default_prompt_pack.clone(),
                system_prompt_name: self.system_prompt.name.to_string(),
                system_prompt_body: self.system_prompt.body.to_string(),
                prompt: session.last_user_text().unwrap_or_default().to_string(),
                tools: self.tools.clone(),
                skill_count: self.compatibility.skills.len(),
                mcp_servers: self.compatibility.mcp_servers.clone(),
            };

            let events = self.api_client.stream(&request);

            let mut text_accum = String::new();
            let mut thinking_accum = String::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            for event in &events {
                match event {
                    ApiEvent::MessageDelta { text } => {
                        text_accum.push_str(text);
                    }
                    ApiEvent::ThinkingDelta { text } => {
                        thinking_accum.push_str(text);
                    }
                    ApiEvent::ToolUse { tool_use } => {
                        tool_uses.push((
                            tool_use.id.clone(),
                            tool_use.name.clone(),
                            serde_json::from_str(&tool_use.input)
                                .unwrap_or(serde_json::Value::Null),
                        ));
                    }
                    ApiEvent::ToolResult { .. } | ApiEvent::Usage { .. } | ApiEvent::Completed => {}
                }
            }

            // If no tool uses, break the loop
            if tool_uses.is_empty() {
                let mut blocks: Vec<ContentBlock> = Vec::new();
                if !thinking_accum.is_empty() {
                    blocks.push(ContentBlock::thinking(&thinking_accum));
                }
                if !text_accum.is_empty() {
                    blocks.push(ContentBlock::text(&text_accum));
                }

                if blocks.is_empty() {
                    session.push(Role::Assistant, "");
                } else {
                    session.push_blocks(Role::Assistant, blocks);
                }
                break;
            }

            // Persist assistant content for this turn, including tool_use blocks.
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !thinking_accum.is_empty() {
                assistant_blocks.push(ContentBlock::thinking(&thinking_accum));
            }
            if !text_accum.is_empty() {
                assistant_blocks.push(ContentBlock::text(&text_accum));
            }
            for (tool_use_id, tool_name, input) in &tool_uses {
                assistant_blocks.push(ContentBlock::tool_use(
                    tool_use_id,
                    tool_name,
                    input.clone(),
                ));
            }
            session.push_blocks(Role::Assistant, assistant_blocks);

            // Execute tool calls (single loop).
            let mut result_blocks: Vec<ContentBlock> = Vec::new();
            for (tool_use_id, tool_name, input) in &tool_uses {
                let result = self.execute_tool(tool_use_id, tool_name, input.clone(), &session.cwd);
                result_blocks.push(result);
                tools_executed += 1;
            }

            // Append tool results as a Tool message
            session.push_blocks(Role::Tool, result_blocks);

            // Single tool-use loop: break after one round
            break;
        }

        RuntimeOutput {
            session_id: session.id.to_string(),
            system_prompt: self.system_prompt.name.to_string(),
            response: text_accum_or_last(session),
            tool_count: self.tools.len(),
            skill_count: self.compatibility.skills.len(),
            mcp_server_count: self.compatibility.mcp_servers.len(),
            tools_executed,
        }
    }

    pub fn execute_tool(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        cwd: &PathBuf,
    ) -> ContentBlock {
        let tool = match self.tool_instances.get(tool_name) {
            Some(t) => t,
            None => {
                return ContentBlock::tool_error(tool_use_id, format!("Unknown tool: {tool_name}"));
            }
        };

        let decision = self
            .permission_engine
            .decide(tool.needs_approval(), is_write_like(tool_name));

        match decision {
            PermissionDecision::Deny => ContentBlock::tool_error(
                tool_use_id,
                format!(
                    "Tool '{tool_name}' denied in {:?} mode",
                    self.permission_engine.mode()
                ),
            ),
            PermissionDecision::Ask => {
                // In non-interactive mode, auto-approve for now (bypass skeleton)
                let result = tool.execute(input, cwd);
                if result.is_error {
                    ContentBlock::tool_error(tool_use_id, result.content)
                } else {
                    ContentBlock::tool_result(tool_use_id, result.content)
                }
            }
            PermissionDecision::Allow => {
                let result = tool.execute(input, cwd);
                if result.is_error {
                    ContentBlock::tool_error(tool_use_id, result.content)
                } else {
                    ContentBlock::tool_result(tool_use_id, result.content)
                }
            }
        }
    }
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
}

fn text_accum_or_last(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .and_then(|m| m.primary_text())
        .unwrap_or("")
        .to_string()
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
            tools_executed: 0,
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

        assert!(!output.response.is_empty());
        assert!(output.response.contains("Model:"));
        assert!(output.response.contains("Prompt pack:"));
    }

    #[test]
    fn submit_persists_assistant_as_content_blocks() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        runtime.submit(&mut session, "hello");

        let assistant_msg = session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("Assistant message should exist");

        assert!(!assistant_msg.content_blocks.is_empty());

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

    #[test]
    fn plan_mode_denies_tool_execution() {
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
        let runtime = Runtime::with_mode(config, prompt_spec, compat, PermissionMode::Plan);
        let session = runtime.start_session(PathBuf::from("/tmp"));

        let result = runtime.execute_tool(
            "1",
            "shell",
            serde_json::json!({"command": "echo hi"}),
            &session.cwd,
        );

        assert!(matches!(
            result,
            ContentBlock::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn bypass_mode_allows_tool_execution() {
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
        let runtime = Runtime::with_mode(config, prompt_spec, compat, PermissionMode::Bypass);
        let dir = std::env::temp_dir().join(format!("clawed_rt_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "hi").unwrap();
        let session = runtime.start_session(dir.clone());

        let result = runtime.execute_tool(
            "1",
            "read_file",
            serde_json::json!({"path": "hello.txt"}),
            &session.cwd,
        );

        if let ContentBlock::ToolResult {
            is_error, content, ..
        } = result
        {
            assert!(!is_error, "expected tool to run, got error: {content}");
        } else {
            panic!("expected ToolResult block");
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
