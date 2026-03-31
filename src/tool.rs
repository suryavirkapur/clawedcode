use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub needs_approval: bool,
}

pub fn builtin_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "shell",
            description: "Run local commands inside the working directory",
            needs_approval: true,
        },
        ToolSpec {
            name: "apply_patch",
            description: "Apply structured file edits",
            needs_approval: true,
        },
        ToolSpec {
            name: "plan",
            description: "Track execution steps and current status",
            needs_approval: false,
        },
        ToolSpec {
            name: "task",
            description: "Spawn bounded background work items",
            needs_approval: false,
        },
    ]
}
