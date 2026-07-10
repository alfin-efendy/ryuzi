//! Minimal LSP client for on-demand diagnostics.
//!
//! Spawns a language server for a file's type, runs the initialize handshake,
//! opens the document, and collects `textDocument/publishDiagnostics`, then
//! shuts the server down. Best-effort: if the server binary is missing or slow,
//! it returns an empty/timeout result rather than failing. Uses LSP's
//! `Content-Length` framing (distinct from MCP's line framing).

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

/// The language server command + args for a file extension, if known.
pub fn server_for(ext: &str) -> Option<(&'static str, Vec<String>, &'static str)> {
    // (command, args, languageId)
    match ext {
        "rs" => Some(("rust-analyzer", vec![], "rust")),
        "ts" | "tsx" => Some((
            "typescript-language-server",
            vec!["--stdio".into()],
            "typescript",
        )),
        "js" | "jsx" => Some((
            "typescript-language-server",
            vec!["--stdio".into()],
            "javascript",
        )),
        "py" => Some(("pylsp", vec![], "python")),
        "go" => Some(("gopls", vec![], "go")),
        _ => None,
    }
}

/// Frame a JSON-RPC message with an LSP `Content-Length` header.
pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Render an LSP `publishDiagnostics` params object into human lines.
pub fn render_diagnostics(params: &Value) -> Vec<String> {
    params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .map(|d| {
                    let line = d
                        .pointer("/range/start/line")
                        .and_then(|v| v.as_i64())
                        .map(|l| l + 1)
                        .unwrap_or(0);
                    let sev = match d.get("severity").and_then(|v| v.as_i64()) {
                        Some(1) => "error",
                        Some(2) => "warning",
                        Some(3) => "info",
                        _ => "hint",
                    };
                    let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    format!("{sev} [line {line}]: {msg}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read one `Content-Length`-framed JSON message.
async fn read_message<R: AsyncReadExt + Unpin>(reader: &mut R) -> anyhow::Result<Value> {
    // Read headers up to the blank line.
    let mut headers = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        reader.read_exact(&mut byte).await?;
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
        if headers.len() > 8192 {
            anyhow::bail!("lsp: header too large");
        }
    }
    let header_str = String::from_utf8_lossy(&headers);
    let len: usize = header_str
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| anyhow::anyhow!("lsp: missing Content-Length"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Collect diagnostics for `file` from its language server. Returns `Ok(None)`
/// when no server is configured/available; `Ok(Some(lines))` otherwise
/// (possibly empty = no problems).
pub async fn diagnostics(work_dir: &Path, file: &Path) -> anyhow::Result<Option<Vec<String>>> {
    let ext = match file.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return Ok(None),
    };
    let Some((cmd, args, language_id)) = server_for(ext) else {
        return Ok(None);
    };
    let text = tokio::fs::read_to_string(file).await.unwrap_or_default();
    let uri = format!("file://{}", file.to_string_lossy());
    let root_uri = format!("file://{}", work_dir.to_string_lossy());

    let mut command = tokio::process::Command::new(cmd);
    command
        .args(&args)
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::process_util::no_window(&mut command);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(_) => return Ok(None), // server not installed
    };
    let mut stdin = child.stdin.take().expect("stdin");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));

    let run = async {
        // initialize
        stdin
            .write_all(&encode(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "processId": null, "rootUri": root_uri, "capabilities": {} }
            })))
            .await?;
        stdin
            .write_all(&encode(
                &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
            ))
            .await?;
        // didOpen
        stdin
            .write_all(&encode(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": language_id, "version": 1, "text": text
                }}
            })))
            .await?;
        // Read until diagnostics for our file arrive.
        loop {
            let msg = read_message(&mut reader).await?;
            if msg.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics")
            {
                if let Some(params) = msg.get("params") {
                    if params.get("uri").and_then(|u| u.as_str()) == Some(uri.as_str()) {
                        return anyhow::Ok(render_diagnostics(params));
                    }
                }
            }
        }
    };

    let result = tokio::time::timeout(Duration::from_secs(20), run).await;
    let _ = child.kill().await;
    match result {
        Ok(Ok(lines)) => Ok(Some(lines)),
        // Timed out or server error — best-effort, report as no diagnostics.
        _ => Ok(Some(vec![])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_has_content_length_header() {
        let bytes = encode(&json!({ "a": 1 }));
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("Content-Length: 7\r\n\r\n"));
        assert!(s.ends_with("{\"a\":1}"));
    }

    #[tokio::test]
    async fn read_message_round_trips_a_framed_message() {
        let framed = encode(&json!({ "method": "hi", "params": { "x": 2 } }));
        let mut cursor = std::io::Cursor::new(framed);
        let msg = read_message(&mut cursor).await.unwrap();
        assert_eq!(msg["method"], "hi");
        assert_eq!(msg["params"]["x"], 2);
    }

    #[test]
    fn render_diagnostics_formats_severity_and_line() {
        let params = json!({
            "uri": "file:///x.rs",
            "diagnostics": [
                { "severity": 1, "message": "mismatched types", "range": {"start": {"line": 4}} },
                { "severity": 2, "message": "unused variable", "range": {"start": {"line": 9}} }
            ]
        });
        let lines = render_diagnostics(&params);
        assert_eq!(lines[0], "error [line 5]: mismatched types");
        assert_eq!(lines[1], "warning [line 10]: unused variable");
    }

    #[test]
    fn server_for_known_and_unknown() {
        assert_eq!(server_for("rs").unwrap().0, "rust-analyzer");
        assert!(server_for("zzz").is_none());
    }
}
