use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub body: &'static str,
}

pub fn builtin_prompts() -> Vec<PromptSpec> {
    vec![
        PromptSpec {
            name: "core",
            summary: "Default coding assistant behavior",
            body: include_str!("../prompts/core.md"),
        },
        PromptSpec {
            name: "review",
            summary: "Focus on defects, regressions, and missing tests",
            body: include_str!("../prompts/review.md"),
        },
        PromptSpec {
            name: "planning",
            summary: "Bias toward explicit execution plans and checkpoints",
            body: include_str!("../prompts/planning.md"),
        },
    ]
}

pub fn resolve_prompt(name: Option<&str>) -> PromptSpec {
    let wanted = name.unwrap_or("core");
    builtin_prompts()
        .into_iter()
        .find(|prompt| prompt.name == wanted)
        .unwrap_or_else(|| {
            builtin_prompts()
                .into_iter()
                .next()
                .expect("prompt registry is not empty")
        })
}
