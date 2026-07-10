//! Native MCP client: a persistent stdio JSON-RPC connection that can
//! `initialize`, `tools/list`, and `tools/call`, so the native runtime can
//! execute MCP tools itself (the ACP harness only forwards server specs to the
//! external agent).

use crate::domain::{McpServerSpec, McpTransport};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio::sync::Mutex;

/// Something that can execute an MCP tool call. Implemented by
/// [`McpConnection`]; tests use a fake.
#[async_trait]
pub trait McpCaller: Send + Sync {
    /// Call `tool` with `arguments` and return the MCP `result` value.
    async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value>;
}

/// One discovered MCP tool: its bare name, description, and input schema.
#[derive(Debug, Clone)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Build a `tools/call` JSON-RPC request.
pub fn build_call_request(id: i64, tool: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    })
}

/// Reduce an MCP `tools/call` result's `content` array to plain text.
pub fn render_tool_result(result: &Value) -> (String, bool) {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item.get("type").and_then(|t| t.as_str()) {
                    Some("text") => item
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(str::to_string),
                    Some(other) => Some(format!("[{other} content]")),
                    None => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| result.to_string());
    (text, is_error)
}

/// A live stdio MCP server connection.
pub struct McpConnection {
    stdin: Mutex<tokio::process::ChildStdin>,
    reader: Mutex<Lines<BufReader<ChildStdout>>>,
    next_id: AtomicI64,
    // Kept alive so `kill_on_drop` reaps the server when the session ends.
    _child: Child,
    pub server_name: String,
    pub tools: Vec<McpToolDef>,
}

impl McpConnection {
    /// Spawn a stdio MCP server, handshake, and list its tools.
    pub async fn connect_stdio(spec: &McpServerSpec) -> anyhow::Result<McpConnection> {
        let McpTransport::Stdio { command, args, env } = &spec.transport else {
            anyhow::bail!("mcp: only stdio transport is supported natively");
        };
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        crate::process_util::no_window(&mut cmd);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let reader = BufReader::new(stdout).lines();

        let mut conn = McpConnection {
            stdin: Mutex::new(stdin),
            reader: Mutex::new(reader),
            next_id: AtomicI64::new(1),
            _child: child,
            server_name: spec.name.clone(),
            tools: Vec::new(),
        };
        conn.handshake().await?;
        conn.tools = conn.list_tools().await?;
        Ok(conn)
    }

    async fn handshake(&self) -> anyhow::Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let init = json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "ryuzi-native", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let resp = self.request(id, &init).await?;
        if let Some(err) = resp.get("error") {
            anyhow::bail!("mcp initialize error: {err}");
        }
        let mut stdin = self.stdin.lock().await;
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        stdin
            .write_all(format!("{initialized}\n").as_bytes())
            .await?;
        Ok(())
    }

    async fn list_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" });
        let resp = self.request(id, &req).await?;
        let tools = resp
            .pointer("/result/tools")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        let name = t.get("name").and_then(|v| v.as_str())?.to_string();
                        Some(McpToolDef {
                            name,
                            description: t
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            input_schema: t
                                .get("inputSchema")
                                .cloned()
                                .unwrap_or_else(|| json!({ "type": "object" })),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(tools)
    }

    /// Write a request and read lines until the matching id response arrives.
    async fn request(&self, id: i64, req: &Value) -> anyhow::Result<Value> {
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(format!("{req}\n").as_bytes()).await?;
            stdin.flush().await?;
        }
        let mut reader = self.reader.lock().await;
        let read = async {
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if let Some(v) = crate::mcp::parse_response_line(&line, id) {
                            return Ok(v);
                        }
                    }
                    Ok(None) => anyhow::bail!("mcp: server closed the connection"),
                    Err(e) => anyhow::bail!("mcp: read error: {e}"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(120), read)
            .await
            .map_err(|_| anyhow::anyhow!("mcp: request timed out"))?
    }
}

#[async_trait]
impl McpCaller for McpConnection {
    async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = build_call_request(id, tool, &arguments);
        let resp = self.request(id, &req).await?;
        if let Some(err) = resp.get("error") {
            anyhow::bail!("{}", err);
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_call_request_shape() {
        let req = build_call_request(7, "search", &json!({ "q": "x" }));
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["id"], 7);
        assert_eq!(req["params"]["name"], "search");
        assert_eq!(req["params"]["arguments"]["q"], "x");
    }

    #[test]
    fn render_tool_result_joins_text_and_flags_errors() {
        let (text, err) = render_tool_result(&json!({
            "content": [{"type": "text", "text": "hello"}, {"type": "text", "text": "world"}]
        }));
        assert_eq!(text, "hello\nworld");
        assert!(!err);

        let (etext, eerr) = render_tool_result(&json!({
            "isError": true,
            "content": [{"type": "text", "text": "boom"}]
        }));
        assert_eq!(etext, "boom");
        assert!(eerr);
    }
}
