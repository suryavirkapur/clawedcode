use crate::{
    background_task::{
        get_background_task, poll_background_task, read_background_task_output,
        spawn_background_shell, stop_background_task, TASK_OUTPUT_TOOL_NAME, TASK_STOP_TOOL_NAME,
    },
    compat::CompatibilitySnapshot,
    config::{AppConfig, default_data_dir},
    content::ContentBlock,
    permissions::{PermissionDecision, PermissionEngine, PermissionMode},
    prompt::{PromptRenderContext, PromptSpec, render_system_prompt},
    session::{Message, Role, Session, SessionMode},
    subagent::{SubAgentConfig, SubAgentResult, SubAgentRuntime},
    tasks::execute_task_tool,
    tool_input::decode_tool_input,
};
use clawedcode_api::{
    ApiEvent, BoxedProvider, CompletionRequest, CompletionResponse, create_provider,
};
use clawedcode_mcp::{
    McpServerConfig, discover_mcp_resources_sync, discover_mcp_tools_sync, make_mcp_tool_name,
    read_mcp_resource_sync, run_mcp_tool_sync,
};
use clawedcode_tools::{
    AGENT_TOOL_NAME, LEGACY_AGENT_TOOL_NAME, Tool, ToolResult, ToolSpec, builtin_tool_instances,
    builtin_tools,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::{Duration, Instant},
};

struct McpToolInstance {
    name: String,
    description: String,
    server_name: String,
    remote_tool_name: String,
    config: McpServerConfig,
}

impl Tool for McpToolInstance {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn execute(&self, input: serde_json::Value, _cwd: &std::path::Path) -> ToolResult {
        match run_mcp_tool_sync(
            &self.config,
            &self.server_name,
            &self.remote_tool_name,
            input,
        ) {
            Ok(content) => ToolResult {
                content,
                is_error: false,
            },
            Err(content) => ToolResult {
                content,
                is_error: true,
            },
        }
    }
}

struct ListMcpResourcesToolInstance {
    servers: BTreeMap<String, McpServerConfig>,
}

impl Tool for ListMcpResourcesToolInstance {
    fn name(&self) -> &str {
        "ListMcpResourcesTool"
    }

    fn description(&self) -> &str {
        "List resources exposed by connected MCP servers. Use this before reading a resource when you need to discover valid server names or URIs."
    }

    fn needs_approval(&self) -> bool {
        false
    }

    fn execute(&self, input: serde_json::Value, _cwd: &std::path::Path) -> ToolResult {
        let target_server = input.get("server").and_then(|value| value.as_str());
        let resources = discover_mcp_resources_sync(&self.servers);

        let mut all_resources = Vec::new();
        if let Some(server_name) = target_server {
            match resources.get(server_name) {
                Some(items) => all_resources.extend(items.iter().cloned()),
                None => {
                    return ToolResult {
                        content: format!(
                            "Server \"{server_name}\" not found or does not support resources"
                        ),
                        is_error: true,
                    };
                }
            }
        } else {
            for items in resources.values() {
                all_resources.extend(items.iter().cloned());
            }
        }

        if all_resources.is_empty() {
            ToolResult {
                content: "No resources found. MCP servers may still provide tools even if they have no resources.".to_string(),
                is_error: false,
            }
        } else {
            ToolResult {
                content: serde_json::to_string(&serde_json::json!({
                    "resources": all_resources
                }))
                .unwrap_or_else(|_| "{\"resources\":[]}".to_string()),
                is_error: false,
            }
        }
    }
}

struct ReadMcpResourceToolInstance {
    servers: BTreeMap<String, McpServerConfig>,
}

impl Tool for ReadMcpResourceToolInstance {
    fn name(&self) -> &str {
        "ReadMcpResourceTool"
    }

    fn description(&self) -> &str {
        "Read a specific MCP resource by server name and URI. The result includes a top-level text field when the resource contains plain text."
    }

    fn needs_approval(&self) -> bool {
        false
    }

    fn execute(&self, input: serde_json::Value, _cwd: &std::path::Path) -> ToolResult {
        let Some(server_name) = input.get("server").and_then(|value| value.as_str()) else {
            return ToolResult {
                content: "Missing 'server' parameter".to_string(),
                is_error: true,
            };
        };
        let Some(uri) = input.get("uri").and_then(|value| value.as_str()) else {
            return ToolResult {
                content: "Missing 'uri' parameter".to_string(),
                is_error: true,
            };
        };

        let Some(config) = self.servers.get(server_name) else {
            return ToolResult {
                content: format!("Server \"{server_name}\" not found"),
                is_error: true,
            };
        };

        match read_mcp_resource_sync(config, server_name, uri) {
            Ok(contents) => {
                let text = contents.iter().find_map(|content| content.text.clone());
                ToolResult {
                    content: serde_json::to_string(&serde_json::json!({
                        "server": server_name,
                        "uri": uri,
                        "text": text,
                        "contents": contents,
                    }))
                    .unwrap_or_else(|_| "{\"contents\":[]}".to_string()),
                    is_error: false,
                }
            }
            Err(content) => ToolResult {
                content,
                is_error: true,
            },
        }
    }
}

pub struct Runtime {
    pub(crate) config: AppConfig,
    pub(crate) system_prompt: PromptSpec,
    pub(crate) tools: Vec<ToolSpec>,
    tool_instances: HashMap<String, Box<dyn Tool>>,
    pub(crate) compatibility: CompatibilitySnapshot,
    pub(crate) provider: BoxedProvider,
    permission_engine: PermissionEngine,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

const REPEATED_TOOL_LOOP_LIMIT: usize = 4;

pub(crate) fn casual_reply_for_prompt(prompt: &str) -> Option<&'static str> {
    let normalized = prompt
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_ascii_lowercase();

    match normalized.as_str() {
        "hi" | "hello" | "hey" | "yo" => Some("Hello!"),
        "how are you" | "how are you doing" | "how's it going" | "hows it going" => {
            Some("Doing fine. What do you want to work on?")
        }
        _ => None,
    }
}

fn push_casual_assistant_reply(session: &mut Session, reply: &str) {
    session.push_blocks(Role::Assistant, vec![ContentBlock::text(reply)]);
}

fn runtime_output_from_reply(runtime: &Runtime, session: &Session, reply: &str) -> RuntimeOutput {
    RuntimeOutput {
        session_id: session.id.to_string(),
        system_prompt: runtime.system_prompt.name.to_string(),
        response: reply.to_string(),
        tool_count: runtime.tools.len(),
        skill_count: runtime.compatibility.skills.len(),
        mcp_server_count: runtime.compatibility.mcp_servers.len(),
        tools_executed: 0,
    }
}

fn streaming_output_from_reply(
    runtime: &Runtime,
    session: &Session,
    reply: &str,
) -> StreamingRuntimeOutput {
    StreamingRuntimeOutput {
        session_id: session.id.to_string(),
        system_prompt: runtime.system_prompt.name.to_string(),
        response: reply.to_string(),
        thinking: String::new(),
        tool_count: runtime.tools.len(),
        skill_count: runtime.compatibility.skills.len(),
        mcp_server_count: runtime.compatibility.mcp_servers.len(),
        tools_executed: 0,
        tool_uses: Vec::new(),
    }
}

#[derive(Debug, Clone)]
struct AgentToolInput {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
}

enum PreparedAgentExecution {
    Ready(ContentBlock),
    Pending(std::thread::JoinHandle<Result<(SubAgentResult, AgentToolInput), String>>),
}

fn apply_prompt_override(messages: &mut [clawedcode_api::ProviderMessage], prompt_override: &str) {
    let Some(last_user) = messages.iter_mut().rfind(|message| {
        message.role == clawedcode_api::ProviderRole::User
            && message
                .content
                .iter()
                .any(|block| matches!(block, clawedcode_api::ProviderContentBlock::Text { .. }))
    }) else {
        return;
    };

    last_user.content = vec![clawedcode_api::ProviderContentBlock::Text {
        text: prompt_override.to_string(),
    }];
}

/// Approval callback used in headless mode.
/// Returns `true` if the tool call is approved, `false` to deny.
pub type ApprovalFn = Box<dyn Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync>;

impl Runtime {
    pub fn tool_specs(&self) -> &[ToolSpec] {
        &self.tools
    }

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
        let mut tool_instances: HashMap<String, Box<dyn Tool>> = builtin_tool_instances()
            .into_iter()
            .map(|t| (t.name().to_string(), t))
            .collect();
        let mut tools = builtin_tools();

        for (server_name, discovered_tools) in discover_mcp_tools_sync(&compatibility.mcp_servers) {
            let Some(config) = compatibility.mcp_servers.get(&server_name) else {
                continue;
            };

            let is_supported = matches!(config, McpServerConfig::Stdio { .. } | McpServerConfig::Http { .. });
            if !is_supported {
                continue;
            }

            for discovered in discovered_tools {
                let full_name = make_mcp_tool_name(&server_name, &discovered.name);
                let description = discovered.description.unwrap_or_default();

                tools.push(ToolSpec {
                    name: full_name.clone(),
                    description: description.clone(),
                    needs_approval: true,
                    input_schema: discovered.input_schema.clone(),
                });

                tool_instances.insert(
                    full_name.clone(),
                    Box::new(McpToolInstance {
                        name: full_name,
                        description,
                        server_name: server_name.clone(),
                        remote_tool_name: discovered.name,
                        config: config.clone(),
                    }),
                );
            }
        }

        if !discover_mcp_resources_sync(&compatibility.mcp_servers).is_empty() {
            tools.push(ToolSpec {
                name: "ListMcpResourcesTool".to_string(),
                description: "List resources exposed by connected MCP servers. Use this first when you need to discover valid server names or resource URIs.".to_string(),
                needs_approval: false,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": {
                            "type": "string",
                            "description": "Optional server name to filter resources by"
                        }
                    }
                }),
            });
            tool_instances.insert(
                "ListMcpResourcesTool".to_string(),
                Box::new(ListMcpResourcesToolInstance {
                    servers: compatibility.mcp_servers.clone(),
                }),
            );

            tools.push(ToolSpec {
                name: "ReadMcpResourceTool".to_string(),
                description: "Read a specific MCP resource by server name and URI. Returns JSON with server, uri, text, and contents fields.".to_string(),
                needs_approval: false,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": {
                            "type": "string",
                            "description": "The MCP server name"
                        },
                        "uri": {
                            "type": "string",
                            "description": "The resource URI to read"
                        }
                    },
                    "required": ["server", "uri"]
                }),
            });
            tool_instances.insert(
                "ReadMcpResourceTool".to_string(),
                Box::new(ReadMcpResourceToolInstance {
                    servers: compatibility.mcp_servers.clone(),
                }),
            );
        }

        Self {
            config,
            system_prompt,
            tools,
            tool_instances,
            compatibility,
            provider,
            permission_engine: PermissionEngine::new(mode),
        }
    }

    pub fn start_session(&self, cwd: PathBuf) -> Session {
        self.start_session_with_mode(cwd, SessionMode::Interactive)
    }

    pub fn start_session_with_mode(&self, cwd: PathBuf, mode: SessionMode) -> Session {
        let mut session = Session::with_mode(cwd, mode);
        session.push(Role::System, self.effective_system_prompt_body(&session));
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

    fn effective_system_prompt_body(&self, session: &Session) -> String {
        let tool_names: Vec<String> = self.tools.iter().map(|tool| tool.name.clone()).collect();
        render_system_prompt(
            &self.system_prompt,
            &PromptRenderContext {
                session,
                model: &self.config.model,
                tool_names: &tool_names,
                compatibility: &self.compatibility,
            },
        )
    }

    pub fn build_request(&self, session: &Session) -> CompletionRequest {
        self.build_request_with_prompt_override(session, None)
    }

    pub fn build_request_with_prompt_override(
        &self,
        session: &Session,
        prompt_override: Option<&str>,
    ) -> CompletionRequest {
        let limit = self.config.runtime.session_history_limit;
        let messages = Self::trim_session_messages(&session.messages, limit);
        let mut request = CompletionRequest {
            model: self.config.model.clone(),
            prompt_pack: self.config.prompts.default_prompt_pack.clone(),
            system_prompt_name: self.system_prompt.name.to_string(),
            system_prompt_body: self.effective_system_prompt_body(session),
            prompt: prompt_override
                .unwrap_or_else(|| session.last_user_text().unwrap_or_default())
                .to_string(),
            messages: messages
                .iter()
                .map(|m| clawedcode_api::ProviderMessage {
                    role: match m.role {
                        Role::User => clawedcode_api::ProviderRole::User,
                        Role::Assistant => clawedcode_api::ProviderRole::Assistant,
                        Role::System => clawedcode_api::ProviderRole::User,
                        Role::Tool => clawedcode_api::ProviderRole::User,
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
                            crate::content::ContentBlock::SubAgentSummary { child_session_id, summary } => {
                                clawedcode_api::ProviderContentBlock::Text {
                                    text: format!("[sub-agent: {}] {}", &child_session_id[..8], summary),
                                }
                            }
                        })
                        .collect(),
                })
                .collect(),
            tools: self.tools.clone(),
            skill_count: self.compatibility.skills.len(),
            mcp_servers: self.compatibility.mcp_servers.clone(),
        };
        if let Some(prompt_override) = prompt_override {
            apply_prompt_override(&mut request.messages, prompt_override);
        }
        request
    }

    pub fn max_turns(&self) -> usize {
        self.config.runtime.max_turns as usize
    }

    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_engine.mode()
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
        if let Some(reply) = casual_reply_for_prompt(prompt) {
            push_casual_assistant_reply(session, reply);
            return runtime_output_from_reply(self, session, reply);
        }
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
        if let Some(reply) = casual_reply_for_prompt(prompt) {
            push_casual_assistant_reply(session, reply);
            return runtime_output_from_reply(self, session, reply);
        }
        let rt_output = self.run_in_runtime(async {
            self.submit_loop_with_approval(session, &mut |_| {}, approval_fn, None)
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

    /// Submit where the visible prompt in the session (transcript) differs from the
    /// provider-facing prompt. The override persists for the full tool-call loop.
    pub fn submit_with_visible_prompt(
        &self,
        session: &mut Session,
        visible_prompt: &str,
        provider_prompt: &str,
    ) -> RuntimeOutput {
        self.submit_with_visible_prompt_and_approval(
            session,
            visible_prompt,
            provider_prompt,
            &|_, _, _| true,
        )
    }

    pub fn submit_with_visible_prompt_and_approval<A: ?Sized>(
        &self,
        session: &mut Session,
        visible_prompt: &str,
        provider_prompt: &str,
        approval_fn: &A,
    ) -> RuntimeOutput
    where
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        session.push(Role::User, visible_prompt);
        if let Some(reply) = casual_reply_for_prompt(provider_prompt) {
            push_casual_assistant_reply(session, reply);
            return runtime_output_from_reply(self, session, reply);
        }
        let rt_output = self.run_in_runtime(async {
            self.submit_loop_with_approval(session, &mut |_| {}, approval_fn, Some(provider_prompt))
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
        if let Some(reply) = casual_reply_for_prompt(prompt) {
            on_event(&ApiEvent::MessageDelta {
                text: reply.to_string(),
            });
            on_event(&ApiEvent::Completed);
            push_casual_assistant_reply(session, reply);
            return streaming_output_from_reply(self, session, reply);
        }
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
        if let Some(reply) = casual_reply_for_prompt(prompt) {
            on_event(&ApiEvent::MessageDelta {
                text: reply.to_string(),
            });
            on_event(&ApiEvent::Completed);
            push_casual_assistant_reply(session, reply);
            return streaming_output_from_reply(self, session, reply);
        }
        self.submit_loop_with_approval(session, &mut on_event, approval_fn, None)
            .await
    }

    pub async fn submit_stream_with_prompt_override_and_approval<F, A>(
        &self,
        session: &mut Session,
        visible_prompt: &str,
        execution_prompt: Option<&str>,
        mut on_event: F,
        approval_fn: &A,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        session.push(Role::User, visible_prompt);
        if let Some(reply) = execution_prompt.and_then(casual_reply_for_prompt) {
            on_event(&ApiEvent::MessageDelta {
                text: reply.to_string(),
            });
            on_event(&ApiEvent::Completed);
            push_casual_assistant_reply(session, reply);
            return streaming_output_from_reply(self, session, reply);
        }
        self.submit_loop_with_approval(session, &mut on_event, approval_fn, execution_prompt)
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
        self.submit_loop_with_approval(session, on_event, &|_, _, _| true, None)
            .await
    }

    async fn submit_loop_with_approval<F, A: ?Sized>(
        &self,
        session: &mut Session,
        on_event: &mut F,
        approval_fn: &A,
        prompt_override: Option<&str>,
    ) -> StreamingRuntimeOutput
    where
        F: FnMut(&ApiEvent),
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        let mut total_text = String::new();
        let mut total_thinking = String::new();
        let mut all_tool_use_records: Vec<ToolUseRecord> = Vec::new();
        let mut total_tools_executed = 0usize;
        let mut repeated_tool_signature: Option<String> = None;
        let mut repeated_tool_count = 0usize;

        for _turn in 0..self.max_turns() {
            let request = self.build_request_with_prompt_override(session, prompt_override);
            let stream = self.provider.stream(&request);

            let turn_result = self
                .process_stream_turn(session, stream, on_event, approval_fn)
                .await;

            if let Some(signature) = repeated_tool_call_signature(&turn_result) {
                if repeated_tool_signature.as_deref() == Some(signature.as_str()) {
                    repeated_tool_count += 1;
                } else {
                    repeated_tool_signature = Some(signature);
                    repeated_tool_count = 1;
                }
            } else {
                repeated_tool_signature = None;
                repeated_tool_count = 0;
            }

            total_text.push_str(&turn_result.text);
            total_thinking.push_str(&turn_result.thinking);
            total_tools_executed += turn_result.tools_executed;
            all_tool_use_records.extend(turn_result.tool_use_records);

            if repeated_tool_count >= REPEATED_TOOL_LOOP_LIMIT {
                let warning = format!(
                    "Stopped after {} repeated identical tool calls.",
                    repeated_tool_count
                );
                if !total_text.is_empty() && !total_text.ends_with('\n') {
                    total_text.push('\n');
                }
                total_text.push_str(&warning);
                on_event(&ApiEvent::MessageDelta {
                    text: warning.clone(),
                });
                session.push(Role::Assistant, &warning);
                break;
            }

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
                        decode_tool_input(&tool_use.name, &tool_use.input),
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

        let mut prepared_agent_executions =
            self.prepare_parallel_agent_executions(&tool_uses, session);
        let mut tools_executed = 0usize;
        let mut result_blocks: Vec<ContentBlock> = Vec::new();
        for (tool_use_id, tool_name, input) in &tool_uses {
            let result = if let Some(prepared) = prepared_agent_executions.remove(tool_use_id) {
                self.finish_prepared_agent_execution(tool_use_id, prepared, session)
            } else {
                self.execute_tool_with_approval(
                    tool_use_id,
                    tool_name,
                    input.clone(),
                    session,
                    approval_fn,
                )
            };
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

    fn prepare_parallel_agent_executions(
        &self,
        tool_uses: &[(String, String, serde_json::Value)],
        session: &Session,
    ) -> HashMap<String, PreparedAgentExecution> {
        let mut prepared = HashMap::new();

        for (tool_use_id, tool_name, input) in tool_uses {
            if !is_agent_tool(tool_name) {
                continue;
            }

            let parsed = match parse_agent_tool_input(input) {
                Ok(parsed) => parsed,
                Err(err) => {
                    prepared.insert(
                        tool_use_id.clone(),
                        PreparedAgentExecution::Ready(ContentBlock::tool_error(
                            tool_use_id,
                            err,
                        )),
                    );
                    continue;
                }
            };

            if let Some(error) = self.agent_tool_preflight_error(session) {
                prepared.insert(
                    tool_use_id.clone(),
                    PreparedAgentExecution::Ready(ContentBlock::tool_error(
                        tool_use_id,
                        error,
                    )),
                );
                continue;
            }

            let Some(sessions_dir) = session_store_dir() else {
                prepared.insert(
                    tool_use_id.clone(),
                    PreparedAgentExecution::Ready(ContentBlock::tool_error(
                        tool_use_id,
                        "No sessions directory available for Agent/Task execution",
                    )),
                );
                continue;
            };

            let config = self.config.clone();
            let system_prompt = self.system_prompt.clone();
            let compatibility = self.compatibility.clone();
            let cwd = session.cwd.clone();
            let parent_snapshot = session.clone();
            let permission_mode = self.permission_mode();
            let max_turns = self.max_turns();
            let parsed_for_thread = parsed.clone();

            prepared.insert(
                tool_use_id.clone(),
                PreparedAgentExecution::Pending(std::thread::spawn(move || {
                    let runtime = SubAgentRuntime::new(
                        config,
                        system_prompt,
                        compatibility,
                        sessions_dir,
                        cwd,
                    );
                    let result = runtime
                        .spawn_fork_from_parent(
                            &parent_snapshot,
                            SubAgentConfig {
                                prompt: parsed_for_thread.prompt.clone(),
                                max_turns,
                                permission_mode,
                            },
                        )
                        .map_err(|err| err.to_string())?;
                    Ok((result, parsed_for_thread))
                })),
            );
        }

        prepared
    }

    fn finish_prepared_agent_execution(
        &self,
        tool_use_id: &str,
        prepared: PreparedAgentExecution,
        session: &mut Session,
    ) -> ContentBlock {
        match prepared {
            PreparedAgentExecution::Ready(block) => block,
            PreparedAgentExecution::Pending(handle) => match handle.join() {
                Ok(Ok((result, input))) => {
                    session.add_child(result.child_session_id);
                    ContentBlock::tool_result(
                        tool_use_id,
                        render_agent_tool_result(&input, &result),
                    )
                }
                Ok(Err(err)) => ContentBlock::tool_error(tool_use_id, err),
                Err(_) => ContentBlock::tool_error(tool_use_id, "Agent task panicked"),
            },
        }
    }

    fn agent_tool_preflight_error(&self, session: &Session) -> Option<String> {
        if session.parent_session_id.is_some() {
            return Some(
                "Agent/Task cannot be invoked from within a child sub-agent session".to_string(),
            );
        }
        None
    }

    fn execute_agent_tool(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        session: &mut Session,
    ) -> ContentBlock {
        let parsed = match parse_agent_tool_input(&input) {
            Ok(parsed) => parsed,
            Err(err) => return ContentBlock::tool_error(tool_use_id, err),
        };

        if let Some(error) = self.agent_tool_preflight_error(session) {
            return ContentBlock::tool_error(tool_use_id, error);
        }

        let Some(sessions_dir) = session_store_dir() else {
            return ContentBlock::tool_error(
                tool_use_id,
                "No sessions directory available for Agent/Task execution",
            );
        };

        let runtime = SubAgentRuntime::new(
            self.config.clone(),
            self.system_prompt.clone(),
            self.compatibility.clone(),
            sessions_dir,
            session.cwd.clone(),
        );

        match runtime.spawn_fork_from_parent(
            session,
            SubAgentConfig {
                prompt: parsed.prompt.clone(),
                max_turns: self.max_turns(),
                permission_mode: self.permission_mode(),
            },
        ) {
            Ok(result) => {
                session.add_child(result.child_session_id);
                ContentBlock::tool_result(tool_use_id, render_agent_tool_result(&parsed, &result))
            }
            Err(err) => ContentBlock::tool_error(tool_use_id, err.to_string()),
        }
    }

    pub fn execute_tool(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        session: &mut Session,
    ) -> ContentBlock {
        self.execute_tool_with_approval(tool_use_id, tool_name, input, session, &|_, _, _| true)
    }

    fn execute_tool_with_approval<A: ?Sized>(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        session: &mut Session,
        approval_fn: &A,
    ) -> ContentBlock
    where
        A: Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync,
    {
        let Some((needs_approval, write_like)) = self.tool_policy(tool_name) else {
            return ContentBlock::tool_error(tool_use_id, format!("Unknown tool: {tool_name}"));
        };

        let decision = self
            .permission_engine
            .decide(needs_approval, write_like);

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
                self.execute_tool_inner(tool_use_id, tool_name, input, session)
            }
            PermissionDecision::Allow => self.execute_tool_inner(tool_use_id, tool_name, input, session),
        }
    }

    fn tool_policy(&self, tool_name: &str) -> Option<(bool, bool)> {
        if let Some(tool) = self.tool_instances.get(tool_name) {
            return Some((tool.needs_approval(), is_write_like(tool_name)));
        }

        match tool_name {
            TASK_OUTPUT_TOOL_NAME => Some((false, false)),
            TASK_STOP_TOOL_NAME => Some((true, true)),
            _ if is_agent_tool(tool_name) => Some((false, false)),
            _ if is_task_store_tool(tool_name) => Some((false, false)),
            _ => None,
        }
    }

    fn execute_tool_inner(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        session: &mut Session,
    ) -> ContentBlock {
        if is_agent_tool(tool_name) {
            return self.execute_agent_tool(tool_use_id, input, session);
        }

        if let Some(result) = execute_task_tool(tool_name, input.clone(), session).unwrap_or_else(
            |err| {
                Some(ToolResult {
                    content: err,
                    is_error: true,
                })
            },
        ) {
            return finish_tool_result(tool_use_id, result);
        }

        if tool_name == TASK_OUTPUT_TOOL_NAME {
            return execute_task_output_tool(tool_use_id, input);
        }

        if tool_name == TASK_STOP_TOOL_NAME {
            return execute_task_stop_tool(tool_use_id, input);
        }

        if tool_name == "shell"
            && input_bool_field(&input, &["run_in_background"]).unwrap_or(false)
        {
            return execute_background_shell(tool_use_id, input, session);
        }

        let tool = match self.tool_instances.get(tool_name) {
            Some(t) => t,
            None => {
                return ContentBlock::tool_error(tool_use_id, format!("Unknown tool: {tool_name}"));
            }
        };

        finish_tool_result(tool_use_id, tool.execute(input, &session.cwd))
    }
}

fn repeated_tool_call_signature(turn_result: &TurnResult) -> Option<String> {
    if turn_result.tool_use_records.len() != 1 {
        return None;
    }

    let record = turn_result.tool_use_records.first()?;
    let input = serde_json::to_string(&record.input).ok()?;
    Some(format!("{}:{input}", record.name))
}

fn finish_tool_result(tool_use_id: &str, result: ToolResult) -> ContentBlock {
    if result.is_error {
        ContentBlock::tool_error(tool_use_id, result.content)
    } else {
        ContentBlock::tool_result(tool_use_id, result.content)
    }
}

fn parse_agent_tool_input(input: &serde_json::Value) -> Result<AgentToolInput, String> {
    let Some(description) = input.get("description").and_then(|value| value.as_str()) else {
        return Err("Missing 'description' parameter".to_string());
    };
    let Some(prompt) = input.get("prompt").and_then(|value| value.as_str()) else {
        return Err("Missing 'prompt' parameter".to_string());
    };

    if description.trim().is_empty() {
        return Err("'description' must not be empty".to_string());
    }
    if prompt.trim().is_empty() {
        return Err("'prompt' must not be empty".to_string());
    }

    Ok(AgentToolInput {
        description: description.to_string(),
        prompt: prompt.to_string(),
        subagent_type: input
            .get("subagent_type")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
    })
}

fn render_agent_tool_result(input: &AgentToolInput, result: &SubAgentResult) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "status": "completed",
        "agent_id": result.child_session_id,
        "agentId": result.child_session_id,
        "description": input.description,
        "prompt": input.prompt,
        "subagent_type": input.subagent_type,
        "content": [{
            "type": "text",
            "text": result.summary,
        }],
        "total_tool_use_count": result.tools_executed,
        "totalToolUseCount": result.tools_executed,
    }))
    .unwrap_or_else(|_| "{\"status\":\"completed\"}".to_string())
}

fn session_store_dir() -> Option<PathBuf> {
    std::env::var_os("CLAWEDCODE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(default_data_dir)
        .map(|dir| dir.join("sessions"))
}

fn execute_background_shell(tool_use_id: &str, input: serde_json::Value, session: &Session) -> ContentBlock {
    let command = match input.get("command").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return ContentBlock::tool_error(tool_use_id, "Missing 'command' parameter");
        }
    };
    
    let description = if command.len() > 50 {
        format!("{}...", &command[..50])
    } else {
        command.clone()
    };
    
    match spawn_background_shell(command, description, session.cwd.clone(), &session.id.to_string()) {
        Ok(task) => {
            let result = serde_json::json!({
                "task_id": task.id,
                "output_file": task.output_file_path.display().to_string(),
                "status": "running",
                "message": format!("Background task started: {}", task.id)
            });
            ContentBlock::tool_result(tool_use_id, serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Err(e) => ContentBlock::tool_error(tool_use_id, format!("Failed to start background task: {e}")),
    }
}

fn execute_task_output_tool(tool_use_id: &str, input: serde_json::Value) -> ContentBlock {
    let task_id = match input_string_field(&input, &["task_id", "taskId"]) {
        Some(s) => s.to_string(),
        None => {
            return ContentBlock::tool_error(tool_use_id, "Missing 'task_id' parameter");
        }
    };

    let block = input_bool_field(&input, &["block"]).unwrap_or(true);
    let timeout_ms = input_u64_field(&input, &["timeout"]).unwrap_or(30000);

    let retrieval_status = if block {
        match wait_for_task_completion(&task_id, timeout_ms) {
            Ok(status) => status,
            Err(e) => {
                return ContentBlock::tool_error(tool_use_id, format!("Failed to poll task: {e}"));
            }
        }
    } else {
        if let Err(e) = poll_background_task(&task_id) {
            return ContentBlock::tool_error(tool_use_id, format!("Failed to poll task: {e}"));
        }
        current_retrieval_status(&task_id)
    };

    match get_background_task(&task_id) {
        Some(task) => {
            let output = match read_background_task_output(&task_id) {
                Ok(o) => o,
                Err(e) => return ContentBlock::tool_error(tool_use_id, format!("Failed to read output: {e}")),
            };

            let mut task_json = serde_json::json!({
                "task_id": task.id,
                "task_type": task.task_type,
                "status": background_task_status_name(&task.status),
                "description": task.description,
                "output": output,
            });

            if let Some(ref res) = task.result {
                task_json["exitCode"] = serde_json::json!(res.code);
                task_json["interrupted"] = serde_json::json!(res.interrupted);
            }

            let result = serde_json::json!({
                "retrieval_status": retrieval_status,
                "task": task_json,
            });
            ContentBlock::tool_result(tool_use_id, serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        None => ContentBlock::tool_error(tool_use_id, format!("Task not found: {task_id}")),
    }
}

fn execute_task_stop_tool(tool_use_id: &str, input: serde_json::Value) -> ContentBlock {
    let task_id = match input_string_field(&input, &["task_id", "taskId", "shell_id"]) {
        Some(s) => s.to_string(),
        None => {
            return ContentBlock::tool_error(tool_use_id, "Missing 'task_id' parameter");
        }
    };

    match get_background_task(&task_id) {
        Some(task) => {
            if task.status != crate::background_task::TaskStatus::Running {
                return ContentBlock::tool_error(
                    tool_use_id,
                    format!("Task {} is not running (status: {:?})", task_id, task.status),
                );
            }
            match stop_background_task(&task_id) {
                Some(stopped_task) => {
                    let result = serde_json::json!({
                        "message": format!("Successfully stopped task: {} ({})", task_id, stopped_task.command),
                        "task_id": stopped_task.id,
                        "task_type": stopped_task.task_type,
                        "command": stopped_task.command,
                        "status": "killed",
                    });
                    ContentBlock::tool_result(tool_use_id, serde_json::to_string_pretty(&result).unwrap_or_default())
                }
                None => ContentBlock::tool_error(tool_use_id, format!("Failed to stop task {task_id}")),
            }
        }
        None => ContentBlock::tool_error(tool_use_id, format!("Task not found: {task_id}")),
    }
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch" | TASK_STOP_TOOL_NAME)
}

fn is_task_store_tool(tool_name: &str) -> bool {
    matches!(tool_name, "TaskCreate" | "TaskList" | "TaskGet" | "TaskUpdate")
}

fn is_agent_tool(tool_name: &str) -> bool {
    tool_name == AGENT_TOOL_NAME || tool_name == LEGACY_AGENT_TOOL_NAME
}

fn background_task_status_name(status: &crate::background_task::TaskStatus) -> &'static str {
    match status {
        crate::background_task::TaskStatus::Pending => "pending",
        crate::background_task::TaskStatus::Running => "running",
        crate::background_task::TaskStatus::Completed => "completed",
        crate::background_task::TaskStatus::Failed => "failed",
        crate::background_task::TaskStatus::Killed => "killed",
    }
}

fn current_retrieval_status(task_id: &str) -> &'static str {
    match get_background_task(task_id) {
        Some(task)
            if matches!(
                task.status,
                crate::background_task::TaskStatus::Pending | crate::background_task::TaskStatus::Running
            ) =>
        {
            "not_ready"
        }
        Some(_) => "success",
        None => "not_ready",
    }
}

fn input_value<'a>(input: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|key| input.get(key))
}

fn input_string_field<'a>(input: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    input_value(input, keys).and_then(|value| value.as_str())
}

fn input_bool_field(input: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    let value = input_value(input, keys)?;
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::Number(number) => number.as_u64().map(|value| value != 0),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn input_u64_field(input: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    let value = input_value(input, keys)?;
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn wait_for_task_completion(task_id: &str, timeout_ms: u64) -> std::io::Result<&'static str> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        poll_background_task(task_id)?;
        match get_background_task(task_id) {
            Some(task)
                if matches!(
                    task.status,
                    crate::background_task::TaskStatus::Pending | crate::background_task::TaskStatus::Running
                ) =>
            {
                if Instant::now() >= deadline {
                    return Ok("timeout");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Some(_) => return Ok("success"),
            None => return Ok("not_ready"),
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
            tools_executed: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use clawedcode_api::{MockProvider, MockToolProvider, Provider, ProviderError};
    use futures_util::stream;
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        pin::Pin,
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

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
            memory_files: vec![],
            memory: String::new(),
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
            memory_files: vec![],
            memory: String::new(),
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

    fn set_temp_data_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("clawed_rt_data_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &dir) };
        dir
    }

    fn start_mock_http_mcp_server(responses: Vec<String>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind");
        let addr = listener.local_addr().expect("failed to get addr");

        let handle = thread::spawn(move || {
            for response_body in responses {
                let (mut stream, _) = listener.accept().expect("failed to accept");
                let mut reader = BufReader::new(&stream);

                let mut request_body = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("read line");
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if line.starts_with("Content-Length:") {
                        let len: usize = line.split(':').nth(1).unwrap().trim().parse().unwrap();
                        request_body = vec![0u8; len];
                    }
                }
                if !request_body.is_empty() {
                    reader.read_exact(&mut request_body).ok();
                }

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes()).ok();
                stream.flush().ok();
            }
        });

        (format!("http://127.0.0.1:{}", addr.port()), handle)
    }

    #[derive(Debug, Clone)]
    struct DualTaskToolProvider;

    impl Provider for DualTaskToolProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>,
        > {
            Box::pin(async {
                Ok(CompletionResponse {
                    system_prompt: "test".to_string(),
                    response: "merged".to_string(),
                    tool_count: 2,
                    skill_count: 0,
                    mcp_server_count: 0,
                })
            })
        }

        fn stream(&self, request: &CompletionRequest) -> clawedcode_api::EventStream {
            let events = if request_has_tool_result_message(request) {
                vec![
                    ApiEvent::MessageDelta {
                        text: "Merged child results.".to_string(),
                    },
                    ApiEvent::Completed,
                ]
            } else {
                vec![
                    ApiEvent::ToolUse {
                        tool_use: clawedcode_api::ToolUseEvent {
                            id: "agent-1".to_string(),
                            name: LEGACY_AGENT_TOOL_NAME.to_string(),
                            input: serde_json::json!({
                                "description": "First task",
                                "prompt": "hello"
                            })
                            .to_string(),
                        },
                    },
                    ApiEvent::ToolUse {
                        tool_use: clawedcode_api::ToolUseEvent {
                            id: "agent-2".to_string(),
                            name: AGENT_TOOL_NAME.to_string(),
                            input: serde_json::json!({
                                "description": "Second task",
                                "prompt": "how are you"
                            })
                            .to_string(),
                        },
                    },
                    ApiEvent::Completed,
                ]
            };

            Box::pin(stream::iter(
                events.into_iter().map(Result::<_, ProviderError>::Ok),
            ))
        }
    }

    fn request_has_tool_result_message(request: &CompletionRequest) -> bool {
        request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    clawedcode_api::ProviderContentBlock::ToolResult { .. }
                )
            })
        })
    }

    fn temp_python_mcp_server() -> std::path::PathBuf {
        let script = r#"
import sys
import json

def send(obj):
    content = json.dumps(obj).encode('utf-8')
    header = ('Content-Length: %d\r\n\r\n' % len(content)).encode('ascii')
    sys.stdout.buffer.write(header)
    sys.stdout.buffer.write(content)
    sys.stdout.buffer.flush()

def read_request():
    content_length = None
    while True:
        header = sys.stdin.buffer.readline()
        if not header:
            return None
        if header in (b'\r\n', b'\n'):
            break
        if header.startswith(b'Content-Length:'):
            content_length = int(header.split(b':', 1)[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    if not body:
        return None
    return json.loads(body)

while True:
    msg = read_request()
    if msg is None:
        break
    method = msg.get("method", "")
    id = msg.get("id")

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "runtime-test-server", "version": "1.0.0"}
            }
        })
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "echo",
                    "description": "Echo back the input",
                    "inputSchema": {"type": "object"}
                }]
            }
        })
    elif method == "tools/call":
        params = msg.get("params", {})
        arguments = params.get("arguments", {})
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": json.dumps(arguments)}]
            }
        })
    elif method == "resources/list":
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resources": [{
                    "uri": "resource://runtime/test",
                    "name": "runtime.txt",
                    "mimeType": "text/plain",
                    "description": "Runtime test resource"
                }]
            }
        })
    elif method == "resources/read":
        params = msg.get("params", {})
        uri = params.get("uri", "")
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": "Runtime MCP resource body"
                }]
            }
        })
"#;

        let path = std::env::temp_dir().join(format!(
            "clawed_runtime_mcp_{}.py",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, script).unwrap();
        path
    }

    #[test]
    fn submit_returns_same_response_text_as_direct_complete() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output = runtime.submit(&mut session, "hello");

        assert!(!output.response.is_empty());
        assert!(output.response.contains("Hello"));
    }

    #[test]
    fn submit_with_visible_prompt_uses_provider_override_but_persists_visible_text() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output = runtime.submit_with_visible_prompt(&mut session, "/review", "hello");

        assert_eq!(session.last_user_text(), Some("/review"));
        assert!(
            output.response.contains("Hello"),
            "expected provider-facing prompt override to shape the response"
        );

        let request = runtime.build_request(&session);
        assert_eq!(request.prompt, "/review");

        let overridden_request =
            runtime.build_request_with_prompt_override(&session, Some("hello"));
        assert_eq!(overridden_request.prompt, "hello");
        let last_user_text = overridden_request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == clawedcode_api::ProviderRole::User)
            .and_then(|message| {
                message.content.iter().find_map(|block| match block {
                    clawedcode_api::ProviderContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            });
        assert_eq!(last_user_text, Some("hello"));
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
    fn submit_persists_thinking_block_when_stream_emits_thinking() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        runtime.submit(&mut session, "please help with a coding task");

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
            "Assistant message should contain a thinking block when the provider emits one"
        );
    }

    #[test]
    fn submit_response_text_matches_streamed_deltas() {
        let runtime = make_runtime();
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output = runtime.submit(&mut session, "please help with a coding task");

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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Plan,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let result = runtime.execute_tool(
            "1",
            "shell",
            serde_json::json!({"command": "echo hi"}),
            &mut session,
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
            memory_files: vec![],
            memory: String::new(),
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
        let mut session = runtime.start_session(dir.clone());

        let result = runtime.execute_tool(
            "1",
            "read_file",
            serde_json::json!({"path": "hello.txt"}),
            &mut session,
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
    fn runtime_registers_agent_and_task_tools() {
        let runtime = make_runtime();

        assert!(runtime.tools.iter().any(|tool| tool.name == AGENT_TOOL_NAME));
        assert!(
            runtime
                .tools
                .iter()
                .any(|tool| tool.name == LEGACY_AGENT_TOOL_NAME)
        );
    }

    #[test]
    fn agent_tool_persists_child_session_and_links_parent() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
        let runtime = make_runtime();
        let mut session = runtime.start_session(std::env::temp_dir());

        let result = runtime.execute_tool(
            "agent-tool",
            LEGACY_AGENT_TOOL_NAME,
            serde_json::json!({
                "description": "Greeting task",
                "prompt": "hello"
            }),
            &mut session,
        );

        let child_id = match result {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                let json: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(json["status"], "completed");
                assert_eq!(json["description"], "Greeting task");
                assert_eq!(json["prompt"], "hello");
                json["agent_id"].as_str().unwrap().to_string()
            }
            other => panic!("expected tool result, got {other:?}"),
        };

        assert_eq!(session.child_sessions.len(), 1);
        assert_eq!(session.child_sessions[0].to_string(), child_id);
        assert!(data_dir.join("sessions").join(format!("{child_id}.json")).exists());

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn child_sessions_cannot_spawn_nested_agent_tools() {
        let runtime = make_runtime();
        let mut child_session =
            Session::new_child(PathBuf::from("/tmp"), uuid::Uuid::new_v4());
        child_session.push(Role::System, "You are a child session.");

        let result = runtime.execute_tool(
            "nested-agent",
            AGENT_TOOL_NAME,
            serde_json::json!({
                "description": "Nested task",
                "prompt": "hello"
            }),
            &mut child_session,
        );

        match result {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(is_error);
                assert!(content.contains("child sub-agent session"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[test]
    fn multiple_agent_tool_uses_preserve_original_order() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(DualTaskToolProvider),
        );

        let mut session = runtime.start_session(std::env::temp_dir());
        let output = runtime.submit(&mut session, "investigate two modules in parallel");

        assert_eq!(output.tools_executed, 2);
        assert_eq!(session.child_sessions.len(), 2);

        let tool_message = session
            .messages
            .iter()
            .find(|message| {
                message.role == Role::Tool
                    && message.content_blocks.len() == 2
            })
            .expect("expected tool message with two results");

        let parse_result = |block: &ContentBlock| match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                serde_json::from_str::<serde_json::Value>(content).unwrap()
            }
            other => panic!("expected tool result, got {other:?}"),
        };

        let first = parse_result(&tool_message.content_blocks[0]);
        let second = parse_result(&tool_message.content_blocks[1]);
        assert_eq!(first["description"], "First task");
        assert_eq!(second["description"], "Second task");

        for child_id in &session.child_sessions {
            assert!(data_dir
                .join("sessions")
                .join(format!("{child_id}.json"))
                .exists());
        }

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn runtime_registers_and_executes_mcp_stdio_tools() {
        let script_path = temp_python_mcp_server();

        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let mut mcp_servers = std::collections::BTreeMap::new();
        mcp_servers.insert(
            "test-server".to_string(),
            McpServerConfig::Stdio {
                r#type: Some("stdio".to_string()),
                command: "python3".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: std::collections::BTreeMap::new(),
            },
        );
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: String::new(),
            mcp_servers,
        };

        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );

        assert!(
            runtime
                .tools
                .iter()
                .any(|tool| tool.name == "mcp__test-server__echo"),
            "expected runtime to surface discovered MCP tool"
        );

        let mut session = runtime.start_session(std::env::current_dir().unwrap());
        let result = runtime.execute_tool(
            "tool-1",
            "mcp__test-server__echo",
            serde_json::json!({"hello": "world"}),
            &mut session,
        );

        match result {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(!is_error);
                assert!(content.contains("hello"));
                assert!(content.contains("world"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        fs::remove_file(script_path).ok();
    }

    #[test]
    fn runtime_registers_and_executes_mcp_http_tools() {
        let responses = vec![
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"1.0"}}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"HTTP echo","inputSchema":{"type":"object"}}]}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"1.0"}}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"result":{"resources":[]}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"1.0"}}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"hello\":\"world\"}"}]}}"#.to_string(),
        ];
        let (url, handle) = start_mock_http_mcp_server(responses);

        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let mut mcp_servers = std::collections::BTreeMap::new();
        mcp_servers.insert(
            "http-server".to_string(),
            McpServerConfig::Http {
                r#type: "http".to_string(),
                url,
                headers: std::collections::BTreeMap::new(),
            },
        );
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: String::new(),
            mcp_servers,
        };

        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );

        assert!(
            runtime
                .tools
                .iter()
                .any(|tool| tool.name == "mcp__http-server__echo"),
            "expected runtime to surface discovered HTTP MCP tool"
        );

        let mut session = runtime.start_session(std::env::current_dir().unwrap());
        let result = runtime.execute_tool(
            "tool-1",
            "mcp__http-server__echo",
            serde_json::json!({"hello": "world"}),
            &mut session,
        );

        match result {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(!is_error);
                assert!(content.contains("hello"));
                assert!(content.contains("world"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        handle.join().ok();
    }

    #[test]
    fn runtime_registers_and_executes_mcp_resource_helper_tools() {
        let script_path = temp_python_mcp_server();

        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let mut mcp_servers = std::collections::BTreeMap::new();
        mcp_servers.insert(
            "test-server".to_string(),
            McpServerConfig::Stdio {
                r#type: Some("stdio".to_string()),
                command: "python3".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: std::collections::BTreeMap::new(),
            },
        );
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: String::new(),
            mcp_servers,
        };

        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );

        assert!(
            runtime
                .tools
                .iter()
                .any(|tool| tool.name == "ListMcpResourcesTool")
        );
        assert!(
            runtime
                .tools
                .iter()
                .any(|tool| tool.name == "ReadMcpResourceTool")
        );

        let mut session = runtime.start_session(std::env::current_dir().unwrap());
        let list_result = runtime.execute_tool(
            "tool-list",
            "ListMcpResourcesTool",
            serde_json::json!({}),
            &mut session,
        );
        match list_result {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(!is_error);
                assert!(content.contains("\"resources\""));
                assert!(content.contains("resource://runtime/test"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        let read_result = runtime.execute_tool(
            "tool-read",
            "ReadMcpResourceTool",
            serde_json::json!({
                "server": "test-server",
                "uri": "resource://runtime/test"
            }),
            &mut session,
        );
        match read_result {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(!is_error);
                assert!(content.contains("\"server\":\"test-server\""));
                assert!(content.contains("\"uri\":\"resource://runtime/test\""));
                assert!(content.contains("\"text\":\"Runtime MCP resource body\""));
                assert!(content.contains("Runtime MCP resource body"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        fs::remove_file(script_path).ok();
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
            memory_files: vec![],
            memory: String::new(),
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
            .filter_map(|m| {
                m.content.iter().find_map(|b| match b {
                    clawedcode_api::ProviderContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .collect();

        assert!(
            texts[0].starts_with("## System"),
            "expected rendered system prompt to start with a System section"
        );
        assert!(texts[0].contains("You are a test assistant."));
        assert_eq!(&texts[1..], ["second", "second reply"]);
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
            memory_files: vec![],
            memory: String::new(),
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
        let system_text = request.messages[0].content.iter().find_map(|b| match b {
            clawedcode_api::ProviderContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        });
        let system_text = system_text.expect("expected system prompt text");
        assert!(system_text.starts_with("## System"));
        assert!(system_text.contains("You are a test assistant."));
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
            memory_files: vec![],
            memory: String::new(),
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
                &clawedcode_api::ProviderRole::User,
                &clawedcode_api::ProviderRole::User,
            ]
        );
        assert_eq!(request.messages.len(), 4);
    }

    #[test]
    fn build_request_renders_dynamic_environment_tools_and_mcp_sections() {
        let _guard = crate::test_support::env_lock();
        unsafe { std::env::set_var("SHELL", "/bin/bash") };

        let mut config = AppConfig::default();
        config.model = "test-model".to_string();

        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let script_path = temp_python_mcp_server();
        let mut mcp_servers = std::collections::BTreeMap::new();
        mcp_servers.insert(
            "demo-server".to_string(),
            McpServerConfig::Stdio {
                r#type: Some("stdio".to_string()),
                command: "python3".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: std::collections::BTreeMap::new(),
            },
        );
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: "Follow the project rule.".to_string(),
            mcp_servers,
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let session = runtime.start_session(PathBuf::from("/tmp/dynamic-prompt"));
        let system_text = session
            .messages
            .first()
            .and_then(|message| message.primary_text())
            .expect("expected initial system prompt");

        assert!(system_text.starts_with("## System"));
        assert!(system_text.contains("## Doing Tasks"));
        assert!(system_text.contains("## Actions With Care"));
        assert!(system_text.contains("## Using Your Tools"));
        assert!(system_text.contains("shell"));
        assert!(system_text.contains("## Tone And Style"));
        assert!(system_text.contains("## Environment"));
        assert!(system_text.contains("Primary working directory: /tmp/dynamic-prompt"));
        assert!(system_text.contains("Date: "));
        assert!(system_text.contains("Model: test-model"));
        assert!(system_text.contains("Shell: /bin/bash"));
        assert!(system_text.contains("## Loaded Memory"));
        assert!(system_text.contains("Follow the project rule."));
        assert!(system_text.contains("## MCP Server Instructions"));
        assert!(system_text.contains("demo-server"));
        assert!(system_text.contains("stdio via `python3`"));

        unsafe { std::env::remove_var("SHELL") };
        fs::remove_file(script_path).ok();
    }

    #[test]
    fn build_request_includes_merged_memory_in_system_prompt_body() {
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
            memory_files: vec![],
            memory: "Follow the project rule.".to_string(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let session = runtime.start_session(PathBuf::from("/tmp"));
        let request = runtime.build_request(&session);

        assert!(
            request
                .system_prompt_body
                .contains("You are a test assistant.")
        );
        assert!(request.system_prompt_body.contains("## Loaded Memory"));
        assert!(
            request
                .system_prompt_body
                .contains("Follow the project rule.")
        );
    }

    #[test]
    fn build_request_includes_dynamic_environment_and_tool_sections() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let session = runtime.start_session(PathBuf::from("/tmp"));
        let request = runtime.build_request(&session);

        assert!(request.system_prompt_body.contains("## Doing Tasks"));
        assert!(request.system_prompt_body.contains("## Actions With Care"));
        assert!(request.system_prompt_body.contains("## Using Your Tools"));
        assert!(request.system_prompt_body.contains("## Session Guidance"));
        assert!(request.system_prompt_body.contains("## Language"));
        assert!(request.system_prompt_body.contains("## Environment"));
        assert!(request.system_prompt_body.contains("Primary working directory: /tmp"));
        assert!(request.system_prompt_body.contains("Model: gpt-5"));
        assert!(request.system_prompt_body.contains("Current setting: default"));
        assert!(request.system_prompt_body.contains("shell"));
        assert!(request.system_prompt_body.contains("read_file"));
        assert!(request.system_prompt_body.contains("apply_patch"));
    }

    #[test]
    fn build_request_includes_mcp_server_section_when_present() {
        let config = AppConfig::default();
        let prompt_spec = PromptSpec {
            name: "test",
            summary: "test",
            body: "You are a test assistant.",
        };
        let mut mcp_servers = std::collections::BTreeMap::new();
        mcp_servers.insert(
            "example-http".to_string(),
            McpServerConfig::Http {
                r#type: "http".to_string(),
                url: "https://example.com/mcp".to_string(),
                headers: std::collections::BTreeMap::new(),
            },
        );
        let compat = CompatibilitySnapshot {
            settings_files: vec![],
            settings: serde_json::Value::Null,
            skills: vec![],
            memory_files: vec![],
            memory: String::new(),
            mcp_servers,
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );

        let session = runtime.start_session(PathBuf::from("/tmp"));
        let request = runtime.build_request(&session);

        assert!(request.system_prompt_body.contains("## MCP Server Instructions"));
        assert!(request.system_prompt_body.contains("example-http"));
        assert!(request.system_prompt_body.contains("https://example.com/mcp"));
    }

    #[test]
    fn background_shell_respects_user_denial() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let before = crate::background_task::list_background_tasks().len();

        let result = runtime.execute_tool_with_approval(
            "bg-deny",
            "shell",
            serde_json::json!({
                "command": "sleep 1",
                "run_in_background": true
            }),
            &mut session,
            &|_, _, _| false,
        );

        match result {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(is_error);
                assert!(content.contains("denied by user"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        let after = crate::background_task::list_background_tasks().len();
        assert_eq!(before, after);

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn task_stop_respects_user_denial() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task = crate::background_task::spawn_background_shell(
            "sleep 5".to_string(),
            "sleep".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();

        let result = runtime.execute_tool_with_approval(
            "stop-deny",
            "TaskStop",
            serde_json::json!({ "task_id": task.id }),
            &mut session,
            &|_, _, _| false,
        );

        match result {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(is_error);
                assert!(content.contains("denied by user"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        let state = crate::background_task::get_background_task(&task.id).unwrap();
        assert_eq!(state.status, crate::background_task::TaskStatus::Running);
        let _ = crate::background_task::stop_background_task(&task.id);

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn task_stop_accepts_task_id_aliases() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task_one = crate::background_task::spawn_background_shell(
            "sleep 5".to_string(),
            "sleep".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();
        let task_two = crate::background_task::spawn_background_shell(
            "sleep 5".to_string(),
            "sleep".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();

        let via_task_id = runtime.execute_tool(
            "stop-task-id",
            "TaskStop",
            serde_json::json!({ "taskId": task_one.id }),
            &mut session,
        );
        let via_shell_id = runtime.execute_tool(
            "stop-shell-id",
            "TaskStop",
            serde_json::json!({ "shell_id": task_two.id }),
            &mut session,
        );

        for block in [via_task_id, via_shell_id] {
            match block {
                ContentBlock::ToolResult {
                    is_error, content, ..
                } => {
                    assert!(!is_error, "unexpected tool error: {content}");
                    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
                    assert_eq!(json["status"], "killed");
                }
                other => panic!("expected tool result, got {other:?}"),
            }
        }

        let stopped_one = crate::background_task::get_background_task(&task_one.id).unwrap();
        let stopped_two = crate::background_task::get_background_task(&task_two.id).unwrap();
        assert_eq!(stopped_one.status, crate::background_task::TaskStatus::Killed);
        assert_eq!(stopped_two.status, crate::background_task::TaskStatus::Killed);

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn background_shell_accepts_stringified_run_in_background() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
        let runtime = make_tool_runtime();
        let mut session = runtime.start_session(std::env::temp_dir());

        let result = runtime.execute_tool(
            "bg-string-bool",
            "shell",
            serde_json::json!({
                "command": "sleep 2",
                "run_in_background": "true"
            }),
            &mut session,
        );

        match result {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                let json: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(json["status"], "running");
                let task_id = json["task_id"].as_str().unwrap();
                let _ = crate::background_task::stop_background_task(task_id);
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn task_output_reports_not_ready_timeout_then_success() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task = crate::background_task::spawn_background_shell(
            "sleep 1; echo done".to_string(),
            "delayed echo".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();

        let not_ready = runtime.execute_tool(
            "task-output-ready",
            "TaskOutput",
            serde_json::json!({
                "task_id": task.id,
                "block": false
            }),
            &mut session,
        );
        let timeout = runtime.execute_tool(
            "task-output-timeout",
            "TaskOutput",
            serde_json::json!({
                "task_id": task.id,
                "block": true,
                "timeout": 50
            }),
            &mut session,
        );
        std::thread::sleep(Duration::from_millis(1200));
        let success = runtime.execute_tool(
            "task-output-success",
            "TaskOutput",
            serde_json::json!({
                "task_id": task.id,
                "block": true,
                "timeout": 2000
            }),
            &mut session,
        );

        let parse = |block: ContentBlock| match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                serde_json::from_str::<serde_json::Value>(&content).unwrap()
            }
            other => panic!("expected tool result, got {other:?}"),
        };

        let not_ready_json = parse(not_ready);
        assert_eq!(not_ready_json["retrieval_status"], "not_ready");

        let timeout_json = parse(timeout);
        assert_eq!(timeout_json["retrieval_status"], "timeout");

        let success_json = parse(success);
        assert_eq!(success_json["retrieval_status"], "success");
        assert_eq!(success_json["task"]["status"], "completed");
        assert!(success_json["task"]["output"]
            .as_str()
            .unwrap_or_default()
            .contains("done"));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn task_output_accepts_camel_case_and_string_fields() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task = crate::background_task::spawn_background_shell(
            "sleep 1; echo done".to_string(),
            "delayed echo".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();

        let not_ready = runtime.execute_tool(
            "task-output-camel-not-ready",
            "TaskOutput",
            serde_json::json!({
                "taskId": task.id,
                "block": "false"
            }),
            &mut session,
        );
        let timeout = runtime.execute_tool(
            "task-output-camel-timeout",
            "TaskOutput",
            serde_json::json!({
                "taskId": task.id,
                "block": "true",
                "timeout": "50"
            }),
            &mut session,
        );
        std::thread::sleep(Duration::from_millis(1200));
        let success = runtime.execute_tool(
            "task-output-camel-success",
            "TaskOutput",
            serde_json::json!({
                "taskId": task.id,
                "block": "true",
                "timeout": "2000"
            }),
            &mut session,
        );

        let parse = |block: ContentBlock| match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                serde_json::from_str::<serde_json::Value>(&content).unwrap()
            }
            other => panic!("expected tool result, got {other:?}"),
        };

        let not_ready_json = parse(not_ready);
        assert_eq!(not_ready_json["retrieval_status"], "not_ready");

        let timeout_json = parse(timeout);
        assert_eq!(timeout_json["retrieval_status"], "timeout");

        let success_json = parse(success);
        assert_eq!(success_json["retrieval_status"], "success");
        assert_eq!(success_json["task"]["status"], "completed");
        assert!(success_json["task"]["output"]
            .as_str()
            .unwrap_or_default()
            .contains("done"));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn task_output_accepts_compat_string_fields() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(MockProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task = crate::background_task::spawn_background_shell(
            "sleep 1; echo done".to_string(),
            "delayed echo".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();

        let not_ready = runtime.execute_tool(
            "task-output-compat-not-ready",
            "TaskOutput",
            serde_json::json!({
                "taskId": task.id,
                "block": "false"
            }),
            &mut session,
        );

        match not_ready {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                let json: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(json["retrieval_status"], "not_ready");
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        std::thread::sleep(Duration::from_millis(1200));

        let success = runtime.execute_tool(
            "task-output-compat-success",
            "TaskOutput",
            serde_json::json!({
                "taskId": task.id,
                "block": "true",
                "timeout": "2000"
            }),
            &mut session,
        );

        match success {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!is_error, "unexpected tool error: {content}");
                let json: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(json["retrieval_status"], "success");
                assert!(json["task"]["output"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("done"));
            }
            other => panic!("expected tool result, got {other:?}"),
        }

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[derive(Debug, Clone)]
    struct StringifiedTaskOutputProvider {
        task_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }

    impl Provider for StringifiedTaskOutputProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>,
        > {
            Box::pin(async { unreachable!("use stream instead") })
        }

        fn stream(&self, request: &CompletionRequest) -> clawedcode_api::EventStream {
            if request_has_tool_result_message(request) {
                let events = vec![
                    ApiEvent::MessageDelta {
                        text: "TaskOutput completed.".to_string(),
                    },
                    ApiEvent::Completed,
                ];
                return Box::pin(stream::iter(
                    events.into_iter().map(Result::<_, ProviderError>::Ok),
                ));
            }

            let task_id = self
                .task_id
                .lock()
                .expect("task id lock")
                .clone()
                .expect("task id set");
            let input = serde_json::to_string(
                &serde_json::json!({
                    "taskId": task_id,
                    "block": "true",
                    "timeout": "2000",
                })
                .to_string(),
            )
            .expect("stringify input");

            let events = vec![
                ApiEvent::ToolUse {
                    tool_use: clawedcode_api::ToolUseEvent {
                        id: "task-output-stream-1".to_string(),
                        name: "TaskOutput".to_string(),
                        input,
                    },
                },
                ApiEvent::Completed,
            ];

            Box::pin(stream::iter(
                events.into_iter().map(Result::<_, ProviderError>::Ok),
            ))
        }
    }

    #[test]
    fn task_output_stringified_object_is_decoded_through_stream_loop() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let task_id = std::sync::Arc::new(std::sync::Mutex::new(None));
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(StringifiedTaskOutputProvider {
                task_id: task_id.clone(),
            }),
        );
        let mut session = runtime.start_session(std::env::temp_dir());
        let task = crate::background_task::spawn_background_shell(
            "echo done".to_string(),
            "immediate echo".to_string(),
            std::env::temp_dir(),
            &session.id.to_string(),
        )
        .unwrap();
        *task_id.lock().expect("task id lock") = Some(task.id.clone());

        let output = runtime.run_in_runtime(async {
            runtime
                .submit_stream(&mut session, "fetch the output", |_| {})
                .await
        });

        assert!(output.response.contains("TaskOutput completed."));
        assert!(output.tools_executed >= 1);

        let tool_result = session
            .messages
            .iter()
            .find(|message| message.role == Role::Tool)
            .expect("tool result message");
        let content = tool_result
            .content_blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    content,
                    is_error: false,
                    ..
                } => Some(content.as_str()),
                _ => None,
            })
            .expect("tool result content");
        let json: serde_json::Value = serde_json::from_str(content).expect("task output json");
        assert_eq!(json["retrieval_status"], "success");
        assert!(json["task"]["output"]
            .as_str()
            .unwrap_or_default()
            .contains("done"));

        let _ = crate::background_task::stop_background_task(&task.id);

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[derive(Debug, Clone)]
    struct RepeatingReadFileProvider;

    impl Provider for RepeatingReadFileProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>,
        > {
            Box::pin(async { unreachable!("use stream instead") })
        }

        fn stream(&self, _request: &CompletionRequest) -> clawedcode_api::EventStream {
            let events = vec![
                ApiEvent::ToolUse {
                    tool_use: clawedcode_api::ToolUseEvent {
                        id: "repeat-read-file".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({
                            "path": "loop.txt"
                        })
                        .to_string(),
                    },
                },
                ApiEvent::Completed,
            ];
            Box::pin(stream::iter(events.into_iter().map(Ok)))
        }
    }

    #[test]
    fn submit_loop_breaks_repeated_identical_tool_calls() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(RepeatingReadFileProvider),
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("loop.txt"), "loop guard").unwrap();
        let mut session = runtime.start_session(dir.path().to_path_buf());

        let output = runtime.run_in_runtime(async {
            runtime
                .submit_stream(&mut session, "Inspect the file.", |_| {})
                .await
        });

        assert!(output.response.contains("Stopped after"));
        assert_eq!(output.tools_executed, REPEATED_TOOL_LOOP_LIMIT);

        let warning_present = session.messages.iter().any(|message| {
            message.role == Role::Assistant
                && message
                    .content_blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("Stopped after")))
        });
        assert!(warning_present, "loop guard warning should be persisted");
    }

    #[derive(Debug, Clone)]
    struct RepeatingNarratedReadFileProvider;

    impl Provider for RepeatingNarratedReadFileProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>,
        > {
            Box::pin(async { unreachable!("use stream instead") })
        }

        fn stream(&self, _request: &CompletionRequest) -> clawedcode_api::EventStream {
            let events = vec![
                ApiEvent::MessageDelta {
                    text: "Let me try again with the correct format.".to_string(),
                },
                ApiEvent::ToolUse {
                    tool_use: clawedcode_api::ToolUseEvent {
                        id: "repeat-read-file-with-text".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({
                            "path": "loop.txt"
                        })
                        .to_string(),
                    },
                },
                ApiEvent::Completed,
            ];
            Box::pin(stream::iter(events.into_iter().map(Ok)))
        }
    }

    #[test]
    fn submit_loop_breaks_repeated_identical_tool_calls_with_retry_text() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(RepeatingNarratedReadFileProvider),
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("loop.txt"), "loop guard").unwrap();
        let mut session = runtime.start_session(dir.path().to_path_buf());

        let output = runtime.run_in_runtime(async {
            runtime
                .submit_stream(&mut session, "Inspect the file.", |_| {})
                .await
        });

        assert!(output.response.contains("Stopped after"));
        assert_eq!(output.tools_executed, REPEATED_TOOL_LOOP_LIMIT);
    }

    #[test]
    fn background_shell_output_path_is_session_scoped() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
        let runtime = make_tool_runtime();
        let mut session_a = runtime.start_session(std::env::temp_dir());
        let mut session_b = runtime.start_session(std::env::temp_dir());

        let run_bg = |session: &mut Session| {
            match runtime.execute_tool(
                "bg-path",
                "shell",
                serde_json::json!({
                    "command": "sleep 2",
                    "run_in_background": true
                }),
                session,
            ) {
                ContentBlock::ToolResult {
                    is_error, content, ..
                } => {
                    assert!(!is_error, "unexpected tool error: {content}");
                    serde_json::from_str::<serde_json::Value>(&content).unwrap()
                }
                other => panic!("expected tool result, got {other:?}"),
            }
        };

        let a = run_bg(&mut session_a);
        let b = run_bg(&mut session_b);
        let a_path = a["output_file"].as_str().unwrap();
        let b_path = b["output_file"].as_str().unwrap();

        assert!(a_path.contains(&session_a.id.to_string()));
        assert!(b_path.contains(&session_b.id.to_string()));
        assert_ne!(a_path, b_path);

        let _ = crate::background_task::stop_background_task(a["task_id"].as_str().unwrap());
        let _ = crate::background_task::stop_background_task(b["task_id"].as_str().unwrap());

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn shell_run_in_background_accepts_string_and_numeric_truthy_values() {
        let _guard = crate::test_support::env_lock();
        let data_dir = set_temp_data_dir();
        let runtime = make_tool_runtime();
        let mut session_a = runtime.start_session(std::env::temp_dir());
        let mut session_b = runtime.start_session(std::env::temp_dir());

        let run_bg = |session: &mut Session, value: serde_json::Value| {
            match runtime.execute_tool(
                "bg-compat",
                "shell",
                serde_json::json!({
                    "command": "sleep 5",
                    "run_in_background": value
                }),
                session,
            ) {
                ContentBlock::ToolResult {
                    is_error, content, ..
                } => {
                    assert!(!is_error, "unexpected tool error: {content}");
                    serde_json::from_str::<serde_json::Value>(&content).unwrap()
                }
                other => panic!("expected tool result, got {other:?}"),
            }
        };

        let string_value = run_bg(&mut session_a, serde_json::Value::String("true".to_string()));
        let numeric_value = run_bg(&mut session_b, serde_json::Value::Number(1.into()));

        assert_eq!(string_value["status"], "running");
        assert_eq!(numeric_value["status"], "running");
        assert!(string_value["task_id"].as_str().is_some());
        assert!(numeric_value["task_id"].as_str().is_some());

        let _ = crate::background_task::stop_background_task(
            string_value["task_id"].as_str().unwrap(),
        );
        let _ = crate::background_task::stop_background_task(
            numeric_value["task_id"].as_str().unwrap(),
        );

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(data_dir).ok();
    }

    struct FailAfterTextProvider;

    impl Provider for FailAfterTextProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>>
        {
            Box::pin(async { unreachable!("complete should not be called for streaming test") })
        }

        fn stream(&self, request: &CompletionRequest) -> clawedcode_api::EventStream {
            let has_tool_result = request.messages.iter().any(|m| {
                m.content.iter().any(|b| {
                    matches!(b, clawedcode_api::ProviderContentBlock::ToolResult { .. })
                })
            });

            if has_tool_result {
                let events = vec![
                    ApiEvent::MessageDelta { text: "Done.".to_string() },
                    ApiEvent::Completed,
                ];
                Box::pin(stream::iter(events.into_iter().map(Ok)))
            } else {
                let events: Vec<Result<ApiEvent, ProviderError>> = vec![
                    Ok(ApiEvent::ThinkingDelta { text: "Thinking...".to_string() }),
                    Ok(ApiEvent::MessageDelta { text: "Hello ".to_string() }),
                    Err(ProviderError::Other {
                        message: "simulated stream failure".to_string(),
                    }),
                ];
                Box::pin(stream::iter(events.into_iter()))
            }
        }
    }

    #[test]
    fn provider_stream_failure_preserves_streamed_content() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Bypass,
            Box::new(FailAfterTextProvider),
        );
        let mut session = runtime.start_session(PathBuf::from("/tmp"));

        let output: Option<StreamingRuntimeOutput> = runtime.run_in_runtime(async {
            let mut events: Vec<ApiEvent> = Vec::new();
            let result = runtime
                .submit_stream(&mut session, "please help with a coding task", |e| {
                    events.push(e.clone());
                })
                .await;
            Some(result)
        });

        let output = output.expect("stream should return output despite error");
        assert!(output.thinking.contains("Thinking..."));
        assert!(output.response.contains("Hello "));
        assert_eq!(output.tools_executed, 0);

        let assistant_msg = session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("Assistant message should exist");
        let has_thinking = assistant_msg
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. }));
        let has_text = assistant_msg
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. }));
        assert!(has_thinking);
        assert!(has_text);
    }

    struct ApprovalTrackingProvider;

    impl Provider for ApprovalTrackingProvider {
        fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionResponse, ProviderError>> + Send + '_>>
        {
            Box::pin(async { unreachable!("use stream instead") })
        }

        fn stream(&self, request: &CompletionRequest) -> clawedcode_api::EventStream {
            let has_tool_result = request.messages.iter().any(|m| {
                m.content.iter().any(|b| {
                    matches!(b, clawedcode_api::ProviderContentBlock::ToolResult { .. })
                })
            });

            if has_tool_result {
                let events = vec![
                    ApiEvent::MessageDelta { text: "Task completed.".to_string() },
                    ApiEvent::Completed,
                ];
                Box::pin(stream::iter(events.into_iter().map(Ok)))
            } else {
                let events = vec![
                    ApiEvent::ToolUse {
                        tool_use: clawedcode_api::ToolUseEvent {
                            id: "tool-approval-1".to_string(),
                            name: "shell".to_string(),
                            input: serde_json::json!({
                                "command": "printf approved"
                            })
                            .to_string(),
                        },
                    },
                    ApiEvent::Completed,
                ];
                Box::pin(stream::iter(events.into_iter().map(Ok)))
            }
        }
    }

    #[test]
    fn streaming_tool_use_with_approval_granted_emits_tool_and_response() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(ApprovalTrackingProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());

        let mut tool_use_seen = false;
        let mut response_seen = false;

        runtime.run_in_runtime(async {
            runtime
                .submit_stream_with_approval(
                    &mut session,
                    "read the file",
                    |event| {
                        match event {
                            ApiEvent::ToolUse { .. } => tool_use_seen = true,
                            ApiEvent::MessageDelta { text } if text.contains("completed") => {
                                response_seen = true
                            }
                            _ => {}
                        }
                    },
                    &|_id, name, _input| {
                        assert_eq!(name, "shell");
                        true
                    },
                )
                .await
        });

        assert!(tool_use_seen, "ToolUse event should be emitted");
        assert!(response_seen, "Final response delta should be emitted");
        assert!(session.messages.iter().any(|m| m.role == Role::Tool));

        let has_tool_result = session.messages.iter().any(|m| {
            m.role == Role::Tool
                && m.content_blocks.iter().any(|b| {
                    matches!(b, ContentBlock::ToolResult { is_error: false, content, .. } if content.contains("approved"))
                })
        });
        assert!(has_tool_result, "Tool result should be persisted in session");
    }

    #[test]
    fn streaming_tool_use_with_approval_denied_persists_error() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };

        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(ApprovalTrackingProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());

        let mut response_seen = false;

        runtime.run_in_runtime(async {
            runtime
                .submit_stream_with_approval(
                    &mut session,
                    "read the file",
                    |event| {
                        if let ApiEvent::MessageDelta { text } = event {
                            if text.contains("completed") {
                                response_seen = true;
                            }
                        }
                    },
                    &|_id, name, _input| {
                        assert_eq!(name, "shell");
                        false
                    },
                )
                .await
        });

        assert!(response_seen, "Final response delta should still be emitted");

        let has_denied_error = session.messages.iter().any(|m| {
            m.role == Role::Tool
                && m.content_blocks.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::ToolResult { is_error, content, .. }
                            if *is_error && content.contains("denied")
                    )
                })
        });
        assert!(has_denied_error, "Denied error should be persisted in session");
        assert!(session.messages.iter().any(|m| m.role == Role::User));
    }

    #[test]
    fn approval_denied_session_still_has_consistent_state() {
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
            memory_files: vec![],
            memory: String::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };

        let runtime = Runtime::with_provider(
            config,
            prompt_spec,
            compat,
            PermissionMode::Default,
            Box::new(ApprovalTrackingProvider),
        );
        let mut session = runtime.start_session(std::env::temp_dir());

        let mut events_seen: Vec<ApiEvent> = Vec::new();
        runtime.run_in_runtime(async {
            runtime
                .submit_stream_with_approval(
                    &mut session,
                    "read the file",
                    |event| events_seen.push(event.clone()),
                    &|_id, _name, _input| false,
                )
                .await
        });

        let role_sequence: Vec<_> = session.messages.iter().map(|m| m.role).collect();

        assert!(role_sequence.contains(&Role::User), "Should have user message");
        assert!(role_sequence.contains(&Role::Assistant), "Should have assistant message");
        assert!(role_sequence.contains(&Role::Tool), "Should have tool result message");

        let session_json = serde_json::to_string(&session).unwrap();
        let reparsed: Session = serde_json::from_str(&session_json).unwrap();
        assert_eq!(reparsed.messages.len(), session.messages.len());
    }

    mod performance_tests {
        use super::*;
        use std::time::Instant;

        #[test]
        fn runtime_construction_is_fast() {
            let start = Instant::now();
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
                memory_files: vec![],
                memory: String::new(),
                mcp_servers: std::collections::BTreeMap::new(),
            };

            for _ in 0..10 {
                let _runtime = Runtime::new(config.clone(), prompt_spec.clone(), compat.clone());
            }
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_millis(500),
                "runtime construction should be fast, took {:?}",
                elapsed
            );
        }

        #[test]
        fn session_save_load_is_fast_for_small_sessions() {
            let dir = std::env::temp_dir().join(format!(
                "perf_session_{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();

            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::System, "System prompt here");
            session.push(Role::User, "What is 2+2?");
            session.push(Role::Assistant, "The answer is 4.");
            session.push(Role::User, "And 3+3?");
            session.push(Role::Assistant, "That would be 6.");

            let start = Instant::now();
            for _ in 0..100 {
                session.save(&dir).unwrap();
                let loaded = Session::load(&dir, session.id).unwrap();
                assert_eq!(loaded.messages.len(), 5);
            }
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_secs(2),
                "session save/load should be fast, took {:?}",
                elapsed
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn build_request_is_fast_for_large_history() {
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
                memory_files: vec![],
                memory: String::new(),
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
            for i in 0..500 {
                session.push(Role::User, &format!("Question {}", i));
                session.push(Role::Assistant, &format!("Answer {}", i));
            }

            let start = Instant::now();
            for _ in 0..100 {
                let _request = runtime.build_request(&session);
            }
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_millis(500),
                "build_request for 500-message session should be fast, took {:?}",
                elapsed
            );
        }

        #[test]
        fn session_messages_access_is_fast() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::System, "System prompt here");
            for i in 0..100 {
                session.push(Role::User, &format!("Question {}", i));
                session.push(Role::Assistant, &format!("Answer {}", i));
            }

            let start = Instant::now();
            for _ in 0..1000 {
                let count = session.messages.len();
                let last_user = session.last_user_text();
                let _ = (count, last_user);
            }
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_millis(50),
                "session message access should be fast, took {:?}",
                elapsed
            );
        }

        #[test]
        fn mcp_discovery_is_bounded_for_empty_servers() {
            let servers: BTreeMap<String, McpServerConfig> = BTreeMap::new();

            let start = Instant::now();
            let tools = discover_mcp_tools_sync(&servers);
            let elapsed = start.elapsed();

            assert!(tools.is_empty());
            assert!(
                elapsed < Duration::from_millis(50),
                "empty discovery should be instant, took {:?}",
                elapsed
            );
        }
    }
}
