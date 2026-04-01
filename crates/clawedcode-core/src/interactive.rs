use crate::{
    compat::SkillDescriptor,
    config::AppConfig,
    content::ContentBlock,
    permissions::PermissionMode,
    prompt::PromptSpec,
    runtime::Runtime,
    session::{Role, Session},
    subagent::{
        CompletedSubAgentTask, SubAgentConfig, SubAgentResult, SubAgentRuntime,
        SubAgentTaskState, drain_completed_subagent_tasks_for_parent,
    },
    tool_input::decode_tool_input,
};
use clawedcode_api::ApiEvent;
use futures_util::StreamExt;
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct TuiContext {
    config: AppConfig,
    system_prompt: PromptSpec,
    pub runtime: Runtime,
    pub session: Session,
    cwd: PathBuf,
    pub sessions_dir: PathBuf,
    pub show_thinking: bool,
    last_compatibility_refresh: Instant,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub enum TuiEvent {
    ThinkingDelta {
        text: String,
    },
    MessageDelta {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    AssistantDone,
    TurnComplete,
}

pub trait TuiHandler {
    fn on_event(&mut self, event: &TuiEvent);
    fn request_approval(&mut self, request: &ApprovalRequest) -> bool;
}

impl TuiContext {
    pub fn new(
        config: AppConfig,
        system_prompt: PromptSpec,
        compatibility: crate::compat::CompatibilitySnapshot,
        cwd: PathBuf,
        sessions_dir: PathBuf,
    ) -> Self {
        let show_thinking = config.ui.show_thinking;
        let runtime = Runtime::with_mode(
            config.clone(),
            system_prompt.clone(),
            compatibility,
            PermissionMode::Default,
        );
        let session = runtime.start_session(cwd.clone());
        Self {
            config,
            system_prompt,
            runtime,
            session,
            cwd,
            sessions_dir,
            show_thinking,
            last_compatibility_refresh: Instant::now(),
        }
    }

    pub fn submit_interactive(&mut self, prompt: &str, handler: &mut dyn TuiHandler) {
        self.submit_interactive_with_prompt_override(prompt, None, handler);
    }

    pub fn submit_interactive_with_prompt_override(
        &mut self,
        visible_prompt: &str,
        execution_prompt: Option<&str>,
        handler: &mut dyn TuiHandler,
    ) {
        self.session.push(Role::User, visible_prompt);

        let request = self
            .runtime
            .build_request_with_prompt_override(&self.session, execution_prompt);

        let mut text_accum = String::new();
        let mut thinking_accum = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

        run_in_runtime(async {
            let mut s = self.runtime.provider.stream(&request);
            loop {
                let next = s.next().await;
                let Some(next) = next else { break };
                let event = match next {
                    Ok(e) => e,
                    Err(_) => break,
                };

                match &event {
                    ApiEvent::MessageDelta { text } => {
                        text_accum.push_str(text.as_str());
                        handler.on_event(&TuiEvent::MessageDelta { text: text.clone() });
                    }
                    ApiEvent::ThinkingDelta { text } => {
                        thinking_accum.push_str(text.as_str());
                        handler.on_event(&TuiEvent::ThinkingDelta { text: text.clone() });
                    }
                    ApiEvent::ToolUse { tool_use } => {
                        let input = decode_tool_input(&tool_use.name, &tool_use.input);
                        tool_uses.push((tool_use.id.clone(), tool_use.name.clone(), input.clone()));
                        handler.on_event(&TuiEvent::ToolUse {
                            id: tool_use.id.clone(),
                            name: tool_use.name.clone(),
                            input,
                        });
                    }
                    ApiEvent::ToolResult { .. } | ApiEvent::Usage { .. } | ApiEvent::Completed => {}
                }

                if matches!(event, ApiEvent::Completed) {
                    break;
                }
            }
        });

        handler.on_event(&TuiEvent::AssistantDone);

        if tool_uses.is_empty() {
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !thinking_accum.is_empty() {
                blocks.push(ContentBlock::thinking(&thinking_accum));
            }
            if !text_accum.is_empty() {
                blocks.push(ContentBlock::text(&text_accum));
            }
            if blocks.is_empty() {
                self.session.push(Role::Assistant, "");
            } else {
                self.session.push_blocks(Role::Assistant, blocks);
            }
        } else {
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
            self.session.push_blocks(Role::Assistant, assistant_blocks);

            let mut result_blocks: Vec<ContentBlock> = Vec::new();
            for (tool_use_id, tool_name, input) in &tool_uses {
                let needs_approval = is_write_like(tool_name);
                let approved_now = if needs_approval {
                    let request = ApprovalRequest {
                        tool_use_id: tool_use_id.clone(),
                        tool_name: tool_name.clone(),
                        input: input.clone(),
                    };
                    handler.request_approval(&request)
                } else {
                    true
                };

                if needs_approval && !approved_now {
                    continue;
                }

                let result = if approved_now {
                    self.runtime.execute_tool(
                        tool_use_id,
                        tool_name,
                        input.clone(),
                        &mut self.session,
                    )
                } else {
                    unreachable!("non-approval tools always approved")
                };

                if let ContentBlock::ToolResult {
                    tool_use_id: rid,
                    content,
                    is_error,
                } = result
                {
                    handler.on_event(&TuiEvent::ToolResult {
                        tool_use_id: rid.clone(),
                        content: content.clone(),
                        is_error,
                    });
                    result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: rid,
                        content,
                        is_error,
                    });
                }
            }
            if !result_blocks.is_empty() {
                self.session.push_blocks(Role::Tool, result_blocks);
            }
        }

        handler.on_event(&TuiEvent::TurnComplete);
    }

    pub fn save_session(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.sessions_dir)?;
        self.session
            .save(&self.sessions_dir)
            .map(|_| ())
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("failed to save session: {e}"),
                )
            })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn model_name(&self) -> &str {
        &self.runtime.config.model
    }

    pub fn skills(&self) -> &[SkillDescriptor] {
        &self.runtime.compatibility.skills
    }

    pub fn refresh_compatibility(&mut self) -> anyhow::Result<()> {
        let compatibility = crate::compat::discover(&self.cwd)?;
        self.runtime = Runtime::with_mode(
            self.config.clone(),
            self.system_prompt.clone(),
            compatibility,
            PermissionMode::Default,
        );
        self.last_compatibility_refresh = Instant::now();
        Ok(())
    }

    pub fn refresh_compatibility_if_stale(
        &mut self,
        min_interval: Duration,
    ) -> anyhow::Result<bool> {
        if self.last_compatibility_refresh.elapsed() < min_interval {
            return Ok(false);
        }
        self.refresh_compatibility()?;
        Ok(true)
    }

    pub fn spawn_subagent(&mut self, prompt: &str) -> anyhow::Result<SubAgentResult> {
        let runtime = SubAgentRuntime::new(
            self.config.clone(),
            self.system_prompt.clone(),
            self.runtime.compatibility.clone(),
            self.sessions_dir.clone(),
            self.session.cwd.clone(),
        );

        runtime.spawn_and_link_to_parent(
            &mut self.session,
            SubAgentConfig {
                prompt: prompt.to_string(),
                max_turns: self.runtime.max_turns(),
                permission_mode: self.runtime.permission_mode(),
            },
        )
    }

    pub fn spawn_subagent_background(&mut self, prompt: &str) -> anyhow::Result<SubAgentTaskState> {
        let runtime = SubAgentRuntime::new(
            self.config.clone(),
            self.system_prompt.clone(),
            self.runtime.compatibility.clone(),
            self.sessions_dir.clone(),
            self.session.cwd.clone(),
        );

        runtime.spawn_in_background_and_link_to_parent(
            &mut self.session,
            SubAgentConfig {
                prompt: prompt.to_string(),
                max_turns: self.runtime.max_turns(),
                permission_mode: self.runtime.permission_mode(),
            },
        )
    }

    pub fn drain_completed_subagent_summaries(&mut self) -> Vec<CompletedSubAgentTask> {
        let completed = drain_completed_subagent_tasks_for_parent(self.session.id);
        if completed.is_empty() {
            return completed;
        }

        for task in &completed {
            self.session.push_blocks(
                Role::Assistant,
                vec![ContentBlock::subagent_summary(
                    task.child_session_id.to_string(),
                    task.summary.clone(),
                )],
            );
        }

        completed
    }
}

fn run_in_runtime<F, T>(f: F) -> T
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

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::CompatibilitySnapshot;
    use crate::test_support::env_lock;

    struct TestHandler {
        events: Vec<TuiEvent>,
        approvals: Vec<bool>,
    }

    impl TestHandler {
        fn new() -> Self {
            Self {
                events: Vec::new(),
                approvals: Vec::new(),
            }
        }
    }

    impl TuiHandler for TestHandler {
        fn on_event(&mut self, event: &TuiEvent) {
            self.events.push(event.clone());
        }

        fn request_approval(&mut self, _request: &ApprovalRequest) -> bool {
            self.approvals.push(true);
            true
        }
    }

    fn make_context() -> TuiContext {
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
        TuiContext::new(
            config,
            prompt_spec,
            compat,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/sessions"),
        )
    }

    #[test]
    fn submit_interactive_produces_events() {
        let mut ctx = make_context();
        let mut handler = TestHandler::new();
        ctx.submit_interactive("hello", &mut handler);

        assert!(
            handler
                .events
                .iter()
                .any(|e| matches!(e, TuiEvent::ThinkingDelta { .. }))
        );
        assert!(
            handler
                .events
                .iter()
                .any(|e| matches!(e, TuiEvent::MessageDelta { .. }))
        );
        assert!(
            handler
                .events
                .iter()
                .any(|e| matches!(e, TuiEvent::AssistantDone))
        );
        assert!(
            handler
                .events
                .iter()
                .any(|e| matches!(e, TuiEvent::TurnComplete))
        );
    }

    #[test]
    fn submit_interactive_updates_session() {
        let mut ctx = make_context();
        let mut handler = TestHandler::new();
        let msg_count_before = ctx.session.messages.len();
        ctx.submit_interactive("hello", &mut handler);
        let msg_count_after = ctx.session.messages.len();

        assert!(msg_count_after > msg_count_before);
        assert!(ctx.session.messages.iter().any(|m| m.role == Role::User));
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn submit_interactive_works_inside_existing_runtime() {
        let mut ctx = make_context();
        let mut handler = TestHandler::new();

        ctx.submit_interactive("hello", &mut handler);

        assert!(
            handler
                .events
                .iter()
                .any(|e| matches!(e, TuiEvent::TurnComplete))
        );
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant)
        );
    }

    #[test]
    fn is_write_like_shell_and_apply_patch() {
        assert!(is_write_like("shell"));
        assert!(is_write_like("apply_patch"));
        assert!(!is_write_like("read_file"));
        assert!(!is_write_like("unknown"));
    }

    #[test]
    fn refresh_compatibility_preserves_session() {
        let mut ctx = make_context();
        let session_id_before = ctx.session.id;
        ctx.session.push(Role::User, "test message");

        ctx.refresh_compatibility().unwrap();

        assert_eq!(ctx.session.id, session_id_before);
        assert_eq!(ctx.session.messages.len(), 2);
        assert!(ctx.session.messages.iter().any(|m| m.role == Role::User));
    }

    #[test]
    fn refresh_compatibility_discovers_new_skills() {
        let _guard = env_lock();
        let temp = std::env::temp_dir().join(format!("refresh_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let config_dir = temp.join(".claude");
        std::fs::create_dir_all(&config_dir).unwrap();

        std::fs::write(config_dir.join("settings.json"), r#"{}"#).unwrap();

        std::fs::create_dir_all(config_dir.join("skills")).unwrap();
        std::fs::write(
            config_dir.join("skills").join("NewSkill.md"),
            "---\nname: New Skill\ndescription: A new skill\n---\nSkill body content",
        )
        .unwrap();

        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &config_dir) };

        let mut ctx = make_context();
        let initial_skills = ctx.skills().len();
        assert_eq!(initial_skills, 0);

        ctx.refresh_compatibility().unwrap();

        assert!(
            ctx.skills().iter().any(|s| s.name == "New Skill"),
            "Expected to find New Skill after refresh"
        );

        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
        std::fs::remove_dir_all(&temp).ok();
    }

    mod approval_loop_tests {
        use super::*;

        #[test]
        fn write_like_tools_require_approval_in_interactive_loop() {
            assert!(is_write_like("shell"));
            assert!(is_write_like("apply_patch"));
            assert!(!is_write_like("read_file"));
            assert!(!is_write_like("unknown_tool"));
        }

        #[test]
        fn approval_request_populates_tool_fields() {
            let request = ApprovalRequest {
                tool_use_id: "tool-123".to_string(),
                tool_name: "shell".to_string(),
                input: serde_json::json!({"command": "rm -rf /"}),
            };

            assert_eq!(request.tool_use_id, "tool-123");
            assert_eq!(request.tool_name, "shell");
            assert_eq!(request.input["command"], "rm -rf /");
        }
    }
}
