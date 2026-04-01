use serde_json::{json, Value};

pub fn decode_tool_input(tool_name: &str, raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return json!({});
    }

    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Null) => json!({}),
        Ok(Value::String(value)) => {
            coerce_single_string_builtin(tool_name, &value).unwrap_or_else(|| Value::String(value))
        }
        Ok(value) => value,
        Err(_) => coerce_single_string_builtin(tool_name, raw).unwrap_or(Value::Null),
    }
}

fn coerce_single_string_builtin(tool_name: &str, raw: &str) -> Option<Value> {
    let key = match tool_name {
        "shell" => "command",
        "read_file" => "path",
        "apply_patch" => "patch",
        _ => return None,
    };

    Some(json!({ key: raw }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_json_object_stays_intact() {
        let input = decode_tool_input("shell", r#"{"command":"ls"}"#);
        assert_eq!(input, json!({"command": "ls"}));
    }

    #[test]
    fn raw_json_string_for_shell_becomes_command_object() {
        let input = decode_tool_input("shell", r#""ls -la""#);
        assert_eq!(input, json!({"command": "ls -la"}));
    }

    #[test]
    fn empty_input_becomes_empty_object() {
        let input = decode_tool_input("shell", "   ");
        assert_eq!(input, json!({}));
    }

    #[test]
    fn malformed_shell_input_becomes_command_object() {
        let input = decode_tool_input("shell", "ls -la");
        assert_eq!(input, json!({"command": "ls -la"}));
    }

    #[test]
    fn unknown_tool_with_malformed_input_returns_null() {
        let input = decode_tool_input("unknown_tool", "not-json");
        assert_eq!(input, Value::Null);
    }

    #[test]
    fn null_input_becomes_empty_object() {
        let input = decode_tool_input("shell", "null");
        assert_eq!(input, json!({}));
    }

    #[test]
    fn read_file_raw_string_becomes_path_object() {
        let input = decode_tool_input("read_file", "/tmp/test.txt");
        assert_eq!(input, json!({"path": "/tmp/test.txt"}));
    }

    #[test]
    fn apply_patch_raw_string_becomes_patch_object() {
        let input = decode_tool_input("apply_patch", "diff --git a/foo b/foo...");
        assert_eq!(input, json!({"patch": "diff --git a/foo b/foo..."}));
    }

    #[test]
    fn unknown_tool_does_not_get_shell_coercion() {
        let shell_input = decode_tool_input("unknown_tool", "ls -la");
        assert_eq!(shell_input, Value::Null);
    }
}
