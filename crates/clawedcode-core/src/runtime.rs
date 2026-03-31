use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    content::ContentBlock,
    permissions::{PermissionDecision, PermissionEngine, PermissionMode},
    prompt::PromptSpec,
    session::{Message, Role, Session},
};
use clawedcode_api::{
    ApiEvent, BoxedProvider, CompletionRequest, CompletionResponse, create_provider,
};
use clawedcode_tools::{Tool, ToolSpec, builtin_tool_instances, builtin_tools};
use futures_util::StreamExt;
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf};

pub struct Runtime {
    pub(crate) config: AppConfig,
    pub(crate) system_prompt: PromptSpec,
    pub(crate) tools: Vec<ToolSpec>,
    tool_instances: HashMap<String, Box<dyn Tool>>,
    pub(crate) compatibility: CompatibilitySnapshot,
    pub(crate) provider: BoxedProvider,
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

#[derive(Debug, Clone, Serialize)]
pub struct StreamingRuntimeOutput {
    pub session_id: String,
    pub system_prompt: String,
    pub response: String,
    pub thinking: String,
    pub tool_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
    pub tools_executed: usize,
    pub tool_uses: Vec<ToolUseRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolUseRecord {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub result: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
struct TurnResult {
    text: String,
    thinking: String,
    has_tool_use: bool,
    tools_executed: usize,
    tool_use_records: Vec<ToolUseRecord>,
}

/// Approval callback used in headless mode.
/// Returns `true` if the tool call is approved, `false` to deny.
pub type ApprovalFn = Box<dyn Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync>;

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
        Self::with_provider(
            config,
            system_prompt,
            compatibility,
            mode,
            create_provider(),
        )
    }

    pub fn with_provider(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: CompatibilitySnapshot,
        mode: PermissionMode,
        provider: BoxedProvider,
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
            provider,
            permission_engine: PermissionEngine::new(mode),
        }
    }

    pub fn start_session(&self, cwd: PathBuf) -> Session {
        let mut session = Session::new(cwd);
        session.push(Role::System, self.system_prompt.body);
        session
    }

    fn trim_session_messages(messages: &[Message], limit: usize) -> Vec<Message> {
        let non_system_total = messages.iter().filter(|m| m.role != Role::System).count();
        let skip_non_system = non_system_total.saturating_sub(limit);
        let mut skipped = 0usize;

        messages
            .iter()
            .filter(|message| {
                if message.role == Role::System {
                    return true;
                }
                if skipped < skip_non_system {
                    skipped += 1;
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    }

    pub fn build_request(&self, session: &Session) -> CompletionRequest {
        let limit = self.config.runtime.session_history_limit;
        let messages = Self::trim_session_messages(&session.messages, limit);
        CompletionRequest {
            model: self.config.model.clone(),
            prompt_pack: self.config.prompts.default_prompt_pack.clone(),
            system_prompt_name: self.system_prompt.name.to_string(),
            system_prompt_body: self.system_prompt.body.to_string(),
            prompt: session.last_user_text().unwrap_or_default().to_string(),
            messages: messages
                .iter()
                .map(|m| clawedcode_api::ProviderMessage {
                    role: match m.role {
                        Role::User => clawedcode_api::ProviderRole::User,
                        Role::Assistant => clawedcode_api::ProviderRole::Assistant,
                        Role::System => clawedcode_api::ProviderRole::User,
                        Role::Tool => clawedcode_api::ProviderRole::Assistant,
                    },
                    content: m
                        .content_blocks
                        .iter()
                        .map(|b| match b {
                            crate::content::ContentBlock::Text { text } => {
                                clawedcode_api::ProviderContentBlock::Text { text: text.clone() }
                            }
                            crate::content::ContentBlock::ToolUse { id, name, input } => {
                                clawedcode_api::ProviderContentBlock::ToolUse {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                }
                            }
                            crate::content::ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => clawedcode_api::ProviderContentBlock::ToolResult {
                                tool_use_id: tool_use_id.clone(),
                                content: content.clone(),
                                is_error: *is_error,
                            },
                            crate::content::ContentBlock::Thinking { thinking } => {
                                clawedcode_api::ProviderContentBlock::Thinking {
                                    thinking: thinking.clone(),
                                }
                            }
                        })
                        .collect(),
                })
                .collect(),
            tools: self.tools.clone(),
            skill_count: self.compatibility.skills.len(),
            mcp_servers: self.compatibility.mcp_servers.clone(),
        }
    }

    fn max_turns(&self) -> usize {
        self.config.runtime.max_turns as usize
    }

    fn run_in_runtime<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(|| handle.block_on(f))
                }
                _ => {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("failed to build tokio runtime");
                    rt.block_on(f)
                }
            },
            Err(_) => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build tokio runtime");
                rt.block_on(f)
            }
        }
    }

    /// Non-streaming submit: wraps the streaming path by collecting the stream.
    /// Loops across provider turns until no ToolUse or max_turns.
    pub fn submit(&self, session: &mut Session, prompt: &str) -> RuntimeOutput {
        session.push(Role::User, prompt);
        let rt_output = self.run_in_runtime(async { self.submit_loop(session, &mut |_| {}).await });

        RuntimeOutput {
            session_id: rt_output.session_id,
            system_prompt: rt_output.system_prompt,
            response: rt_output.response.clone(),
            tool_count: rt_output.tool_count,
            skill_count: rt_output.skill_count,
            mcp_server_count: rt_output.mcp_server_count,
            tools_executed: rt_output.tools_executed,
        }
    }

    /// Submit with an approval callback (for headless mode with --yes / stdin prompt).
    pub fn submit_with_approval<A: ?Sized>(
        &self,
        session: &mut Session,
        prompt: &str,
        approval_fn: &A,
    ) -> RuntimeOutput
    where
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        session.push(Role::User, prompt);
        let rt_output = self.run_in_runtime(async {
            self.submit_loop_with_approval(session, &mut |_| {}, approval_fn)
                .await
        });

        RuntimeOutput {
            session_id: rt_output.session_id,
            system_prompt: rt_output.system_prompt,
            response: rt_output.response.clone(),
            tool_count: rt_output.tool_count,
            skill_count: rt_output.skill_count,
            mcp_server_count: rt_output.mcp_server_count,
            tools_executed: rt_output.tools_executed,
        }
    }

    /// Async streaming submit: consumes the provider stream, yields events to a callback,
    /// while persisting a structured transcript.
    /// Loops across provider turns until no ToolUse or max_turns.
    pub async fn submit_stream<F>(
        &self,
        session: &mut Session,
        prompt: &str,
        mut on_event: F,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
    {
        session.push(Role::User, prompt);
        self.submit_loop(session, &mut on_event).await
    }

    /// Streaming submit with approval callback.
    pub async fn submit_stream_with_approval<F, A>(
        &self,
        session: &mut Session,
        prompt: &str,
        mut on_event: F,
        approval_fn: &A,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        session.push(Role::User, prompt);
        self.submit_loop_with_approval(session, &mut on_event, approval_fn)
            .await
    }

    /// Core tool-call loop: stream provider, execute tools, re-query until done.
    async fn submit_loop<F>(
        &self,
        session: &mut Session,
        on_event: &mut F,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
    {
        self.submit_loop_with_approval(session, on_event, &|_, _, _| true)
            .await
    }

    async fn submit_loop_with_approval<F, A: ?Sized>(
        &self,
        session: &mut Session,
        on_event: &mut F,
        approval_fn: &A,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        let mut total_text = String::new();
        let mut total_thinking = String::new();
        let mut all_tool_use_records: Vec<ToolUseRecord> = Vec::new();
        let mut total_tools_executed = 0usize;

        for _turn in 0..self.max_turns() {
            let request = self.build_request(session);
            let stream = self.provider.stream(&request);

            let turn_result = self
                .process_stream_turn(session, stream, on_event, approval_fn)
                .await;

            total_text.push_str(&turn_result.text);
            total_thinking.push_str(&turn_result.thinking);
            total_tools_executed += turn_result.tools_executed;
            all_tool_use_records.extend(turn_result.tool_use_records);

            if !turn_result.has_tool_use {
                break;
            }
        }

        StreamingRuntimeOutput {
            session_id: session.id.to_string(),
            system_prompt: self.system_prompt.name.to_string(),
            response: total_text,
            thinking: total_thinking,
            tool_count: self.tools.len(),
            skill_count: self.compatibility.skills.len(),
            mcp_server_count: self.compatibility.mcp_servers.len(),
            tools_executed: total_tools_executed,
            tool_uses: all_tool_use_records,
        }
    }

    async fn process_stream_turn<F, A: ?Sized>(
        &self,
        session: &mut Session,
        stream: clawedcode_api::EventStream,
        on_event: &mut F,
        approval_fn: &A,
    ) -> TurnResult
    where
        F: FnMut(&ApiEvent),
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        let mut text_accum = String::new();
        let mut thinking_accum = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
        let mut tool_use_records: Vec<ToolUseRecord> = Vec::new();
        let mut s = stream;

        while let Some(event) = s.next().await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("Provider stream error: {e}");
                    break;
                }
            };

            match &event {
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
                        serde_json::from_str(&tool_use.input).unwrap_or(serde_json::Value::Null),
                    ));
                }
                ApiEvent::ToolResult { tool_result } => {
                    if let Some(record) = tool_use_records
                        .iter_mut()
                        .find(|r| r.id == tool_result.tool_use_id)
                    {
                        record.result = Some(tool_result.content.clone());
                        record.is_error = tool_result.is_error;
                    }
                }
                ApiEvent::Usage { usage: _ } => {}
                ApiEvent::Completed => {}
            }

            on_event(&event);

            if matches!(event, ApiEvent::Completed) {
                break;
            }
        }

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

            return TurnResult {
                text: text_accum,
                thinking: thinking_accum,
                has_tool_use: false,
                tools_executed: 0,
                tool_use_records,
            };
        }

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
            tool_use_records.push(ToolUseRecord {
                id: tool_use_id.clone(),
                name: tool_name.clone(),
                input: input.clone(),
                result: None,
                is_error: false,
            });
        }
        session.push_blocks(Role::Assistant, assistant_blocks);

        let mut tools_executed = 0usize;
        let mut result_blocks: Vec<ContentBlock> = Vec::new();
        for (tool_use_id, tool_name, input) in &tool_uses {
            let result = self.execute_tool_with_approval(
                tool_use_id,
                tool_name,
                input.clone(),
                &session.cwd,
                approval_fn,
            );
            if let ContentBlock::ToolResult {
                is_error, content, ..
            } = &result
            {
                if let Some(record) = tool_use_records.iter_mut().find(|r| r.id == *tool_use_id) {
                    record.result = Some(content.clone());
                    record.is_error = *is_error;
                }
            }
            result_blocks.push(result);
            tools_executed += 1;
        }

        session.push_blocks(Role::Tool, result_blocks);

        TurnResult {
            text: text_accum,
            thinking: thinking_accum,
            has_tool_use: true,
            tools_executed,
            tool_use_records,
        }
    }

    pub fn execute_tool(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        cwd: &PathBuf,
    ) -> ContentBlock {
        self.execute_tool_with_approval(tool_use_id, tool_name, input, cwd, &|_, _, _| true)
    }

    fn execute_tool_with_approval<A: ?Sized>(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        cwd: &PathBuf,
        approval_fn: &A,
    ) -> ContentBlock
    where
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
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
                if !approval_fn(tool_use_id, tool_name, &input) {
                    return ContentBlock::tool_error(
                        tool_use_id,
                        format!("Tool '{tool_name}' denied by user"),
                    );
                }
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
    use clawedcode_api::{MockProvider, MockToolProvider};

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
        Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        )
    }

    fn make_tool_runtime() -> Runtime {
        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant. Use tools when needed.",
        };
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            mcp_servers: std::collections::BTreeMap::new(),
        };
        Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockToolProvider),
        )
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

        let request = runtime.build_request(&session);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        let direct = rt.block_on(runtime.provider.complete(&request)).unwrap();

        assert_eq!(output.response, direct.response);
    }

    #[tokio::test]
    async fn submit_stream_concat_equals_complete() {
        let runtime = make_runtime();
        let session = runtime.start_session(PathBuf::from("/tmp"));
        let request = runtime.build_request(&session);

        let direct_response = runtime.provider.complete(&request).await.unwrap();

        let mut events: Vec<ApiEvent> = Vec::new();
        let stream = runtime.provider.stream(&request);
        let mut s = stream;
        while let Some(event) = s.next().await {
            if let Ok(e) = event {
                let is_completed = matches!(e, ApiEvent::Completed);
                events.push(e);
                if is_completed {
                    break;
                }
            }
        }

        let streamed_text: String = events
            .iter()
            .filter_map(|e| {
                if let ApiEvent::MessageDelta { text } = e {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(streamed_text, direct_response.response);
    }

    #[tokio::test]
    async fn submit_stream_order_thinking_before_text() {
        let runtime = make_runtime();
        let session = runtime.start_session(PathBuf::from("/tmp"));
        let request = runtime.build_request(&session);

        let stream = runtime.provider.stream(&request);
        let mut s = stream;
        let mut events: Vec<ApiEvent> = Vec::new();
        while let Some(event) = s.next().await {
            if let Ok(e) = event {
                events.push(e);
            }
        }

        let thinking_idx = events
            .iter()
            .position(|e| matches!(e, ApiEvent::ThinkingDelta { .. }))
            .expect("Should have ThinkingDelta");
        let text_idx = events
            .iter()
            .position(|e| matches!(e, ApiEvent::MessageDelta { .. }))
            .expect("Should have MessageDelta");

        assert!(
            thinking_idx < text_idx,
            "ThinkingDelta should come before MessageDelta"
        );
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
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Plan,
            Box::new(MockProvider),
        );
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
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );
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

    #[test]
    fn tool_call_loop_executes_tool_then_returns_final_response() {
        let runtime = make_tool_runtime();
        let dir = std::env::temp_dir().join(format!("clawed_tool_loop_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/clawedcode-cli", "crates/clawedcode-core", "crates/clawedcode-api", "crates/clawedcode-tools", "crates/clawedcode-mcp", "crates/clawedcode-tui"]
"#,
        )
        .unwrap();

        let mut session = runtime.start_session(dir.clone());
        let output = runtime.submit(&mut session, "read Cargo.toml and summarize it");

        assert!(
            output.tools_executed > 0,
            "expected at least one tool to be executed, got {}",
            output.tools_executed
        );

        assert!(
            output.response.contains("clawedcode"),
            "expected final response to reference cargo workspace, got: {}",
            output.response
        );

        let tool_msgs: Vec<_> = session
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .collect();
        assert!(
            !tool_msgs.is_empty(),
            "expected tool result messages in session"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tool_result_is_persisted_in_session() {
        let runtime = make_tool_runtime();
        let dir =
            std::env::temp_dir().join(format!("clawed_tool_persist_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

        let mut session = runtime.start_session(dir.clone());
        let _output = runtime.submit(&mut session, "read Cargo.toml");

        let has_tool_result = session.messages.iter().any(|m| {
            m.role == Role::Tool
                && m.content_blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        assert!(
            has_tool_result,
            "expected tool result to be persisted in session"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_request_trims_old_non_system_messages() {
        let mut config = AppConfig::default();
        config.runtime.session_history_limit = 2;

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
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let mut session = runtime.start_session(PathBuf::from("/tmp"));
        session.push(Role::User, "first");
        session.push(Role::Assistant, "first reply");
        session.push(Role::User, "second");
        session.push(Role::Assistant, "second reply");

        let request = runtime.build_request(&session);
        let texts: Vec<_> = request
            .messages
            .iter()
            .filter_map(|m| m.content.iter().find_map(|b| match b {
                clawedcode_api::ProviderContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }))
            .collect();

        assert_eq!(
            texts,
            vec!["You are a test assistant.", "second", "second reply"]
        );
    }

    #[test]
    fn build_request_preserves_system_messages_when_limit_is_zero() {
        let mut config = AppConfig::default();
        config.runtime.session_history_limit = 0;

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
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let mut session = runtime.start_session(PathBuf::from("/tmp"));
        session.push(Role::User, "discard me");
        session.push(Role::Assistant, "discard me too");

        let request = runtime.build_request(&session);
        assert_eq!(request.messages.len(), 1);
        let system_text = request.messages[0]
            .content
            .iter()
            .find_map(|b| match b {
                clawedcode_api::ProviderContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            });
        assert_eq!(system_text, Some("You are a test assistant."));
    }

    #[test]
    fn build_request_keeps_newest_non_system_messages_in_original_order() {
        let mut config = AppConfig::default();
        config.runtime.session_history_limit = 3;

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
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let mut session = runtime.start_session(PathBuf::from("/tmp"));
        session.push(Role::User, "old user");
        session.push(Role::Assistant, "old assistant");
        session.push_blocks(
            Role::Assistant,
            vec![ContentBlock::tool_use(
                "tool-1",
                "read_file",
                serde_json::json!({"path": "Cargo.toml"}),
            )],
        );
        session.push_blocks(
            Role::Tool,
            vec![ContentBlock::tool_result("tool-1", "contents")],
        );
        session.push(Role::User, "latest user");

        let request = runtime.build_request(&session);
        let kept_roles: Vec<_> = request.messages.iter().map(|m| &m.role).collect();

        assert_eq!(
            kept_roles,
            vec![
                &clawedcode_api::ProviderRole::User,
                &clawedcode_api::ProviderRole::Assistant,
                &clawedcode_api::ProviderRole::Assistant,
                &clawedcode_api::ProviderRole::User,
            ]
        );
        assert_eq!(request.messages.len(), 4);
    }
}
