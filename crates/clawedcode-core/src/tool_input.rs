use serde_json::{json, Value};

pub fn decode_tool_input(tool_name: &str, raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return json!({});
    }

    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Null) => json!({}),
        Ok(Value::String(value)) => normalize_builtin_value(tool_name, Value::String(value)),
        Ok(value) => normalize_builtin_value(tool_name, value),
        Err(_) => coerce_single_string_builtin(tool_name, raw).unwrap_or(Value::Null),
    }
}

fn builtin_key(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "shell" => Some("command"),
        "read_file" => Some("path"),
        "apply_patch" => Some("patch"),
        _ => None,
    }
}

fn normalize_builtin_value(tool_name: &str, value: Value) -> Value {
    let Some(key) = builtin_key(tool_name) else {
        return value;
    };

    match value {
        Value::String(raw) => {
            unwrap_nested_builtin_payload(tool_name, &raw).unwrap_or_else(|| json!({ key: raw }))
        }
        Value::Object(map) => {
            if let Some(Value::String(raw)) = map.get(key) {
                if let Some(unwrapped) = unwrap_nested_builtin_payload(tool_name, raw) {
                    return unwrapped;
                }
            }
            Value::Object(map)
        }
        other => other,
    }
}

fn unwrap_nested_builtin_payload(tool_name: &str, raw: &str) -> Option<Value> {
    let key = builtin_key(tool_name)?;
    let mut candidate = raw.trim().to_string();

    for _ in 0..2 {
        let parsed = serde_json::from_str::<Value>(&candidate).ok()?;
        match parsed {
            Value::Object(map) if map.get(key).is_some() => return Some(Value::Object(map)),
            Value::String(next) if next.trim() != candidate => {
                candidate = next;
            }
            _ => return None,
        }
    }

    None
}

fn coerce_single_string_builtin(tool_name: &str, raw: &str) -> Option<Value> {
    let key = builtin_key(tool_name)?;

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
    fn shell_command_object_with_nested_json_string_is_unwrapped() {
        let input = decode_tool_input("shell", r#"{"command":"{\"command\":\"ls -la\"}"}"#);
        assert_eq!(input, json!({"command": "ls -la"}));
    }

    #[test]
    fn shell_double_stringified_json_is_unwrapped() {
        let input = decode_tool_input("shell", r#""{\"command\":\"ls -la\"}""#);
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
