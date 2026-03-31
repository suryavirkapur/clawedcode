use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug, Clone)]
pub struct McpToolSpec {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

const CLAUDEAI_SERVER_PREFIX: &str = "claude.ai ";

pub fn normalize_name_for_mcp(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            normalized.push(c);
        } else {
            normalized.push('_');
        }
    }
    if name.starts_with(CLAUDEAI_SERVER_PREFIX) {
        let mut collapsed = String::new();
        let mut last_was_underscore = false;
        for c in normalized.chars() {
            if c == '_' {
                if !last_was_underscore {
                    collapsed.push(c);
                    last_was_underscore = true;
                }
            } else {
                collapsed.push(c);
                last_was_underscore = false;
            }
        }
        let trimmed = collapsed.trim_matches('_');
        if trimmed.is_empty() {
            normalized
        } else {
            trimmed.to_string()
        }
    } else {
        normalized
    }
}

pub fn make_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    let prefix = format!("mcp__{}__", normalize_name_for_mcp(server_name));
    format!("{}{}", prefix, normalize_name_for_mcp(tool_name))
}

struct SyncIoBridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl SyncIoBridge {
    fn new(command: &str, args: &[String], env: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn {}: {e}", command))?;
        let stdin = child.stdin.take().ok_or("missing child stdin")?;
        let stdout = child.stdout.take().ok_or("missing child stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn stdin(&mut self) -> &mut ChildStdin {
        &mut self.stdin
    }

    fn stdout(&mut self) -> &mut BufReader<ChildStdout> {
        &mut self.stdout
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

pub struct McpStdioClient {
    server_name: String,
    io: SyncIoBridge,
    initialized: bool,
}

impl McpStdioClient {
    pub fn new(
        server_name: String,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let io = SyncIoBridge::new(command, args, env)?;
        let mut client = Self {
            server_name,
            io,
            initialized: false,
        };
        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<(), String> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "clawedcode",
                    "version": "0.0.3"
                }
            }
        });

        self.send_json(&request)?;
        let _response = self.read_json()?;

        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        self.send_json(&notif)?;

        self.initialized = true;
        Ok(())
    }

    fn send_json(&mut self, value: &serde_json::Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| format!("serialize error: {e}"))?;
        let stdin = self.io.stdin();
        write!(stdin, "Content-Length: {}\r\n\r\n", bytes.len())
            .map_err(|e| format!("write header error: {e}"))?;
        stdin
            .write_all(&bytes)
            .map_err(|e| format!("write error: {e}"))?;
        stdin.flush().map_err(|e| format!("flush error: {e}"))?;
        Ok(())
    }

    fn read_json(&mut self) -> Result<Value, String> {
        let stdout = self.io.stdout();
        let mut content_length = None;
        let mut line = String::new();

        loop {
            line.clear();
            stdout
                .read_line(&mut line)
                .map_err(|e| format!("read header error: {e}"))?;

            if line.is_empty() {
                return Err("unexpected EOF while reading MCP headers".into());
            }

            if line == "\r\n" || line == "\n" {
                break;
            }

            let trimmed = line.trim();
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(
                    value
                        .trim()
                        .parse()
                        .map_err(|e| format!("parse Content-Length error: {e}"))?,
                );
            }
        }

        let content_length = content_length.ok_or("missing Content-Length header")?;

        let mut body = vec![0u8; content_length];
        stdout
            .read_exact(&mut body)
            .map_err(|e| format!("read body error: {e}"))?;

        serde_json::from_slice(&body).map_err(|e| format!("parse error: {e}"))
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpToolSpec>, String> {
        if !self.initialized {
            return Err("not initialized".into());
        }

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });

        self.send_json(&request)?;
        let response = self.read_json()?;

        let tools = response
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        let name = t.get("name")?.as_str()?.to_string();
                        let description = t
                            .get("description")
                            .and_then(|d| d.as_str())
                            .map(String::from);
                        let input_schema = t
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        Some(McpToolSpec {
                            name,
                            description,
                            input_schema,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(tools)
    }

    pub fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<String, String> {
        if !self.initialized {
            return Err("not initialized".into());
        }

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        });

        self.send_json(&request)?;
        let response = self.read_json()?;

        response
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .ok_or_else(|| "invalid response format".into())
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        self.io.kill();
    }
}

pub fn discover_mcp_tools_sync(
    servers: &BTreeMap<String, McpServerConfig>,
) -> BTreeMap<String, Vec<McpToolSpec>> {
    let mut result: BTreeMap<String, Vec<McpToolSpec>> = BTreeMap::new();

    for (name, config) in servers {
        if let McpServerConfig::Stdio {
            command, args, env, ..
        } = config
        {
            match McpStdioClient::new(name.clone(), command, args, env) {
                Ok(ref mut client) => {
                    if let Ok(tools) = client.list_tools() {
                        result.insert(name.clone(), tools);
                    }
                }
                Err(e) => {
                    eprintln!("failed to connect to MCP server {}: {}", name, e);
                }
            }
        }
    }

    result
}

pub fn run_mcp_tool_sync(
    server_name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    tool_name: &str,
    arguments: Value,
) -> Result<String, String> {
    let mut client = McpStdioClient::new(server_name.to_string(), command, args, env)?;
    client.call_tool(tool_name, arguments)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio {
        #[serde(default)]
        r#type: Option<String>,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Sse {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Http {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Ws {
        #[serde(rename = "type")]
        r#type: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Sdk {
        #[serde(rename = "type")]
        r#type: String,
        name: String,
    },
}

impl McpServerConfig {
    pub fn command(&self) -> Option<String> {
        match self {
            McpServerConfig::Stdio { command, .. } => Some(command.clone()),
            _ => None,
        }
    }

    pub fn args(&self) -> &[String] {
        match self {
            McpServerConfig::Stdio { args, .. } => args,
            _ => &[],
        }
    }
}

pub fn discover_mcp_servers(settings: &Value) -> BTreeMap<String, McpServerConfig> {
    settings
        .get("mcpServers")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_python_mcp_server() -> std::path::PathBuf {
        let script = r#"
import sys
import json

def send(obj):
    content = json.dumps(obj).encode('utf-8')
    header = ('Content-Length: %d\r\n\r\n' % len(content)).encode('ascii')
    sys.stdout.buffer.write(header)
    sys.stdout.buffer.write(content)
    sys.stdout.buffer.flush()

def read_request():
    content_length = None
    while True:
        header = sys.stdin.buffer.readline()
        if not header:
            return None
        if header in (b'\r\n', b'\n'):
            break
        if header.startswith(b'Content-Length:'):
            content_length = int(header.split(b':', 1)[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    if not body:
        return None
    return json.loads(body)

while True:
    msg = read_request()
    if msg is None:
        break
    method = msg.get("method", "")
    id = msg.get("id")

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "test-server", "version": "1.0.0"}
            }
        })
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [
                    {
                        "name": "test_tool",
                        "description": "A test MCP tool",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {"type": "string"}
                            },
                            "required": ["message"]
                        }
                    },
                    {
                        "name": "echo",
                        "description": "Echo back the input",
                        "inputSchema": {"type": "object"}
                    }
                ]
            }
        })
    elif method == "tools/call":
        params = msg.get("params", {})
        tool_name = params.get("name", "")
        arguments = params.get("arguments", {})
        if tool_name == "test_tool":
            msg_text = arguments.get("message", "default")
            send({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": f"Received: {msg_text}"}]
                }
            })
        elif tool_name == "echo":
            send({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": json.dumps(arguments)}]
                }
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"}
            })
"#;

        let temp_dir = std::env::temp_dir();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let script_path = temp_dir.join(format!("fake_mcp_server_{}.py", now));
        std::fs::write(&script_path, script).expect("failed to write test script");
        script_path
    }

    #[test]
    fn normalize_name_for_mcp_basic() {
        assert_eq!(normalize_name_for_mcp("hello"), "hello");
        assert_eq!(normalize_name_for_mcp("hello-world"), "hello-world");
        assert_eq!(normalize_name_for_mcp("hello.world"), "hello_world");
        assert_eq!(normalize_name_for_mcp("hello world"), "hello_world");
        assert_eq!(
            normalize_name_for_mcp("hello.world.test"),
            "hello_world_test"
        );
    }

    #[test]
    fn normalize_name_for_mcp_claudeai_prefix() {
        assert_eq!(
            normalize_name_for_mcp("claude.ai server"),
            "claude_ai_server"
        );
        assert_eq!(
            normalize_name_for_mcp("claude.ai  server"),
            "claude_ai_server"
        );
        assert_eq!(
            normalize_name_for_mcp("claude.ai server__tool"),
            "claude_ai_server_tool"
        );
        assert_eq!(
            normalize_name_for_mcp("_claude.ai server_"),
            "_claude_ai_server_"
        );
    }

    #[test]
    fn make_mcp_tool_name_basic() {
        assert_eq!(
            make_mcp_tool_name("my-server", "my_tool"),
            "mcp__my-server__my_tool"
        );
        assert_eq!(
            make_mcp_tool_name("server-with-dashes", "tool-with-dashes"),
            "mcp__server-with-dashes__tool-with-dashes"
        );
    }

    #[test]
    fn make_mcp_tool_name_preserves_valid_names() {
        assert_eq!(
            make_mcp_tool_name("server123", "tool456"),
            "mcp__server123__tool456"
        );
    }

    #[test]
    fn make_mcp_tool_name_claudeai() {
        assert_eq!(
            make_mcp_tool_name("claude.ai github", "create_issue"),
            "mcp__claude_ai_github__create_issue"
        );
    }

    #[test]
    fn mcp_stdio_client_connects_and_lists_tools() {
        let script_path = temp_python_mcp_server();
        let mut client = McpStdioClient::new(
            "test-server".to_string(),
            "python3",
            &[script_path.to_str().unwrap().to_string()],
            &BTreeMap::new(),
        )
        .expect("failed to connect");

        assert!(client.is_initialized());
        let tools = client.list_tools().expect("failed to list tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "test_tool");
        assert_eq!(tools[1].name, "echo");

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    fn mcp_stdio_client_calls_tool() {
        let script_path = temp_python_mcp_server();
        let mut client = McpStdioClient::new(
            "test-server".to_string(),
            "python3",
            &[script_path.to_str().unwrap().to_string()],
            &BTreeMap::new(),
        )
        .expect("failed to connect");

        let result = client
            .call_tool("test_tool", serde_json::json!({"message": "hello"}))
            .expect("failed to call tool");
        assert_eq!(result, "Received: hello");

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    fn run_mcp_tool_sync_integration() {
        let script_path = temp_python_mcp_server();
        let result = run_mcp_tool_sync(
            "test-server",
            "python3",
            &[script_path.to_str().unwrap().to_string()],
            &BTreeMap::new(),
            "echo",
            serde_json::json!({"foo": "bar"}),
        )
        .expect("failed to run tool");
        assert!(result.contains("foo"));

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    fn discover_mcp_tools_sync_with_single_server() {
        let script_path = temp_python_mcp_server();
        let mut servers = BTreeMap::new();
        servers.insert(
            "test".to_string(),
            McpServerConfig::Stdio {
                r#type: Some("stdio".to_string()),
                command: "python3".to_string(),
                args: vec![script_path.to_str().unwrap().to_string()],
                env: BTreeMap::new(),
            },
        );

        let discovered = discover_mcp_tools_sync(&servers);
        assert!(discovered.contains_key("test"));
        let tools = discovered.get("test").unwrap();
        assert_eq!(tools.len(), 2);

        std::fs::remove_file(script_path).ok();
    }
}
