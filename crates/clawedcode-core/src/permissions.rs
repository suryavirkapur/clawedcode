use serde::{Deserialize, Serialize};

/// Permission modes controlling tool execution behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Default: tools that need_approval require user confirmation.
    Default,
    /// Accept all edits without prompting (auto-approve write-like tools).
    AcceptEdits,
    /// Plan-only: no tools are actually executed; dry-run.
    Plan,
    /// Bypass all permission checks; execute everything.
    Bypass,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Default
    }
}

/// Decision returned by the permission engine for a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Tool may execute.
    Allow,
    /// Tool must wait for user approval (not enforced here; caller handles UX).
    Ask,
    /// Tool is denied in this mode.
    Deny,
}

/// Permission engine that decides whether a tool call may proceed.
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    mode: PermissionMode,
}

impl PermissionEngine {
    pub fn new(mode: PermissionMode) -> Self {
        Self { mode }
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// Decide whether a tool call is allowed.
    ///
    /// - `needs_approval`: whether the tool declares it needs user approval.
    /// - `is_write_like`: whether the tool mutates state (edits, shell, etc.).
    pub fn decide(&self, needs_approval: bool, is_write_like: bool) -> PermissionDecision {
        match self.mode {
            PermissionMode::Bypass => PermissionDecision::Allow,
            PermissionMode::Plan => PermissionDecision::Deny,
            PermissionMode::AcceptEdits => {
                if is_write_like {
                    PermissionDecision::Allow
                } else if needs_approval {
                    PermissionDecision::Ask
                } else {
                    PermissionDecision::Allow
                }
            }
            PermissionMode::Default => {
                if needs_approval {
                    PermissionDecision::Ask
                } else {
                    PermissionDecision::Allow
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_allows_read_tools() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
    }

    #[test]
    fn default_mode_asks_for_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        assert_eq!(engine.decide(true, false), PermissionDecision::Ask);
    }

    #[test]
    fn bypass_mode_allows_everything() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);
        assert_eq!(engine.decide(true, true), PermissionDecision::Allow);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
        assert_eq!(engine.decide(true, false), PermissionDecision::Allow);
    }

    #[test]
    fn plan_mode_denies_everything() {
        let engine = PermissionEngine::new(PermissionMode::Plan);
        assert_eq!(engine.decide(true, true), PermissionDecision::Deny);
        assert_eq!(engine.decide(false, false), PermissionDecision::Deny);
    }

    #[test]
    fn accept_edits_allows_write_like_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(true, true), PermissionDecision::Allow);
    }

    #[test]
    fn accept_edits_asks_for_non_write_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(true, false), PermissionDecision::Ask);
    }

    #[test]
    fn accept_edits_allows_non_approval_tools() {
        let engine = PermissionEngine::new(PermissionMode::AcceptEdits);
        assert_eq!(engine.decide(false, false), PermissionDecision::Allow);
    }

    #[test]
    fn default_mode_is_default() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }

    #[test]
    fn engine_mode_accessor() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);
        assert_eq!(engine.mode(), PermissionMode::Bypass);
    }

    #[test]
    fn permission_mode_serializes() {
        let mode = PermissionMode::AcceptEdits;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"accept_edits\"");
    }

    #[test]
    fn permission_mode_deserializes() {
        let mode: PermissionMode = serde_json::from_str("\"bypass\"").unwrap();
        assert_eq!(mode, PermissionMode::Bypass);
    }
}
