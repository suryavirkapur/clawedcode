use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    prompt::PromptSpec,
    session::{Role, Session},
    tool::{ToolSpec, builtin_tools},
};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Runtime {
    config: AppConfig,
    system_prompt: PromptSpec,
    tools: Vec<ToolSpec>,
    compatibility: CompatibilitySnapshot,
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
        }
    }

    pub fn start_session(&self, cwd: PathBuf) -> Session {
        let mut session = Session::new(cwd);
        session.push(Role::System, self.system_prompt.body);
        session
    }

    pub fn submit(&self, session: &mut Session, prompt: &str) -> RuntimeOutput {
        session.push(Role::User, prompt);

        let response = format!(
            "Model: {}\nPrompt pack: {}\nTools: {}\nSkills discovered: {}\nMCP servers discovered: {}\n\nRequest queued for the execution loop.\n\nNext priorities:\n1. Parse instructions into an explicit task graph.\n2. Resolve tool approvals before execution.\n3. Stream structured updates into the terminal UI.",
            self.config.model,
            self.config.prompts.default_prompt_pack,
            self.tools
                .iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>()
                .join(", "),
            self.compatibility.skills.len(),
            self.compatibility.mcp_servers.len(),
        );

        session.push(Role::Assistant, response.clone());

        RuntimeOutput {
            session_id: session.id.to_string(),
            system_prompt: self.system_prompt.name.to_string(),
            response,
            tool_count: self.tools.len(),
            skill_count: self.compatibility.skills.len(),
            mcp_server_count: self.compatibility.mcp_servers.len(),
        }
    }
}
