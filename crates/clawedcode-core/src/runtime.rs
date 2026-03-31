use crate::{
    compat::CompatibilitySnapshot,
    config::AppConfig,
    prompt::PromptSpec,
    session::{Role, Session},
};
use clawedcode_api::{ApiClient, CompletionRequest, CompletionResponse, MockApiClient};
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

        let response = self.api_client.complete(&CompletionRequest {
            model: self.config.model.clone(),
            prompt_pack: self.config.prompts.default_prompt_pack.clone(),
            system_prompt_name: self.system_prompt.name.to_string(),
            system_prompt_body: self.system_prompt.body.to_string(),
            prompt: prompt.to_string(),
            tools: self.tools.clone(),
            skill_count: self.compatibility.skills.len(),
            mcp_servers: self.compatibility.mcp_servers.clone(),
        });

        session.push(Role::Assistant, response.response.clone());

        RuntimeOutput::from_api(session.id.to_string(), response)
    }
}

impl RuntimeOutput {
    fn from_api(session_id: String, response: CompletionResponse) -> Self {
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
