use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        thinking: String,
    },
    SubAgentSummary {
        child_session_id: String,
        summary: String,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }

    pub fn thinking(thinking: impl Into<String>) -> Self {
        Self::Thinking {
            thinking: thinking.into(),
        }
    }

    pub fn subagent_summary(
        child_session_id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self::SubAgentSummary {
            child_session_id: child_session_id.into(),
            summary: summary.into(),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_text_block() {
        let block = ContentBlock::text("hello");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"hello"}"#);
    }

    #[test]
    fn serialize_tool_use_block() {
        let block =
            ContentBlock::tool_use("1", "read_file", serde_json::json!({"path": "src/main.rs"}));
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tool_use","id":"1","name":"read_file","input":{"path":"src/main.rs"}}"#
        );
    }

    #[test]
    fn serialize_tool_result_block() {
        let block = ContentBlock::tool_result("1", "file contents");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tool_result","tool_use_id":"1","content":"file contents","is_error":false}"#
        );
    }

    #[test]
    fn serialize_thinking_block() {
        let block = ContentBlock::thinking("let me think");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"type":"thinking","thinking":"let me think"}"#);
    }

    #[test]
    fn serialize_subagent_summary_block() {
        let block = ContentBlock::subagent_summary("abc123", "done");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"sub_agent_summary","child_session_id":"abc123","summary":"done"}"#
        );
    }

    #[test]
    fn deserialize_text_block() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block, ContentBlock::text("hello"));
    }

    #[test]
    fn deserialize_tool_use_block() {
        let json =
            r#"{"type":"tool_use","id":"1","name":"read_file","input":{"path":"src/main.rs"}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            ContentBlock::tool_use("1", "read_file", serde_json::json!({"path": "src/main.rs"}))
        );
    }

    #[test]
    fn deserialize_tool_result_with_default_is_error() {
        let json = r#"{"type":"tool_result","tool_use_id":"1","content":"ok"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block, ContentBlock::tool_result("1", "ok"));
    }

    #[test]
    fn as_text_returns_text_for_text_block() {
        let block = ContentBlock::text("hello");
        assert_eq!(block.as_text(), Some("hello"));
    }

    #[test]
    fn as_text_returns_none_for_non_text() {
        let block = ContentBlock::thinking("hmm");
        assert_eq!(block.as_text(), None);
    }
}
