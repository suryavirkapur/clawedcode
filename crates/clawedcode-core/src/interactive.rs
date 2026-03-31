use crate::{
    config::AppConfig,
    content::ContentBlock,
    permissions::PermissionMode,
    prompt::PromptSpec,
    runtime::Runtime,
    session::{Role, Session},
};
use clawedcode_api::ApiEvent;
use futures_util::StreamExt;
use serde::Serialize;
use std::path::PathBuf;

pub struct TuiContext {
    pub runtime: Runtime,
    pub session: Session,
    pub sessions_dir: PathBuf,
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
        let runtime = Runtime::with_mode(
            config,
            system_prompt,
            compatibility,
            PermissionMode::Default,
        );
        let session = runtime.start_session(cwd);
        Self {
            runtime,
            session,
            sessions_dir,
        }
    }

    pub fn submit_interactive(&mut self, prompt: &str, handler: &mut dyn TuiHandler) {
        self.session.push(Role::User, prompt);

        let request = self.runtime.build_request(&self.session);

        let mut text_accum = String::new();
        let mut thinking_accum = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        let mut s = rt.block_on(async { self.runtime.provider.stream(&request) });
        loop {
            let next = rt.block_on(async { s.next().await });
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
                    let input =
                        serde_json::from_str(&tool_use.input).unwrap_or(serde_json::Value::Null);
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
                        &self.session.cwd,
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
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::CompatibilitySnapshot;

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

    #[test]
    fn is_write_like_shell_and_apply_patch() {
        assert!(is_write_like("shell"));
        assert!(is_write_like("apply_patch"));
        assert!(!is_write_like("read_file"));
        assert!(!is_write_like("unknown"));
    }
}
