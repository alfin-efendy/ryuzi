//! Newline-delimited JSON-RPC 2.0 framing over a stdio pipe.
//!
//! This module is deliberately protocol-agnostic: it only knows how to build
//! JSON-RPC envelopes, write them as newline-terminated lines, and read
//! lines back until one matches a requested id. It carries no MCP or
//! extension-specific semantics (method names, timeouts, tool schemas,
//! result flattening) — those stay with each transport's own caller
//! (`harness::native::mcp_client`, `mcp`, and later the plugin extension
//! host), which reuse this codec so the framing is implemented exactly once.

use serde_json::{json, Value};
use std::fmt;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, Lines};

/// Build a JSON-RPC 2.0 request envelope:
/// `{ "jsonrpc": "2.0", "id": <id>, "method": <method>[, "params": <params>] }`.
///
/// `params` is omitted entirely when `None` (some servers reject a literal
/// `"params": null`), matching every existing hand-rolled request builder.
pub fn build_request(id: i64, method: &str, params: Option<Value>) -> Value {
    match params {
        Some(p) => json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": p }),
        None => json!({ "jsonrpc": "2.0", "id": id, "method": method }),
    }
}

/// Build a JSON-RPC 2.0 notification envelope (no `id`; no response expected).
pub fn build_notification(method: &str, params: Option<Value>) -> Value {
    match params {
        Some(p) => json!({ "jsonrpc": "2.0", "method": method, "params": p }),
        None => json!({ "jsonrpc": "2.0", "method": method }),
    }
}

/// Parse `line` as JSON and return it only if it carries the matching `id`.
/// Malformed lines or a mismatched `id` yield `None` so callers can keep
/// reading subsequent lines (e.g. interleaved logs, or another call's
/// response arriving out of order).
pub fn parse_response_line(line: &str, id: i64) -> Option<Value> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    (v.get("id").and_then(|i| i.as_i64()) == Some(id)).then_some(v)
}

/// Serialize `value` as a single JSON-RPC line, write it, and flush.
pub async fn write_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> std::io::Result<()> {
    writer.write_all(format!("{value}\n").as_bytes()).await?;
    writer.flush().await
}

/// Why [`read_response`] gave up before finding a matching-id line.
#[derive(Debug)]
pub enum ReadError {
    /// The peer closed its stdout (EOF) before a matching response arrived.
    Closed,
    /// The underlying line read failed.
    Io(std::io::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Closed => {
                write!(f, "connection closed before a matching response arrived")
            }
            ReadError::Io(e) => write!(f, "read error: {e}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Closed => None,
            ReadError::Io(e) => Some(e),
        }
    }
}

/// Read lines from `lines` until one parses as a JSON-RPC response with the
/// matching `id` (non-matching or unparsable lines are skipped and reading
/// continues). Callers apply their own overall timeout, e.g. via
/// `tokio::time::timeout`, since the appropriate budget is transport/call
/// specific (MCP's 120s tool timeout vs. the 25s probe, and later per-event
/// extension timeouts).
pub async fn read_response<R: AsyncBufRead + Unpin>(
    lines: &mut Lines<R>,
    id: i64,
) -> Result<Value, ReadError> {
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(v) = parse_response_line(&line, id) {
                    return Ok(v);
                }
            }
            Ok(None) => return Err(ReadError::Closed),
            Err(e) => return Err(ReadError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn build_request_includes_params_when_present() {
        let req = build_request(7, "tools/call", Some(json!({ "name": "search" })));
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 7);
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "search");
    }

    #[test]
    fn build_request_omits_params_key_when_absent() {
        let req = build_request(2, "tools/list", None);
        assert_eq!(req["method"], "tools/list");
        assert!(
            req.get("params").is_none(),
            "params key must be omitted (not null) when absent"
        );
        assert_eq!(req["id"], 2);
    }

    #[test]
    fn build_notification_has_no_id() {
        let note = build_notification("notifications/initialized", None);
        assert_eq!(note["method"], "notifications/initialized");
        assert!(note.get("id").is_none());
        assert!(note.get("params").is_none());
    }

    #[test]
    fn build_notification_includes_params_when_present() {
        let note = build_notification("event/tool.before", Some(json!({ "tool": "bash" })));
        assert!(note.get("id").is_none());
        assert_eq!(note["params"]["tool"], "bash");
    }

    #[test]
    fn parse_response_line_matches_id_only() {
        assert!(parse_response_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, 1).is_some());
        assert!(parse_response_line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 1).is_none());
        assert!(parse_response_line("not json", 1).is_none());
    }

    #[test]
    fn parse_response_line_surfaces_error_responses() {
        let v = parse_response_line(
            r#"{"jsonrpc":"2.0","id":5,"error":{"code":-1,"message":"boom"}}"#,
            5,
        )
        .expect("matching id parses");
        assert_eq!(v["error"]["message"], "boom");
    }

    #[tokio::test]
    async fn read_response_skips_non_matching_ids_then_returns_match() {
        let data =
            b"{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":\"ignored\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"ok\"}\n";
        let mut lines = BufReader::new(&data[..]).lines();
        let v = read_response(&mut lines, 1)
            .await
            .expect("finds matching id");
        assert_eq!(v["result"], "ok");
    }

    #[tokio::test]
    async fn read_response_reports_closed_on_eof() {
        let data: &[u8] = b"";
        let mut lines = BufReader::new(data).lines();
        let err = read_response(&mut lines, 1).await.unwrap_err();
        assert!(matches!(err, ReadError::Closed));
    }
}
