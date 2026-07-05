//! `webfetch` — fetch a URL and return its text.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_FETCH_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct WebFetch;

/// Very small HTML→text reduction: drop `<script>`/`<style>` blocks and all
/// tags, collapse whitespace. Good enough for Phase 1; a real reader is later.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        // Skip <script>...</script> and <style>...</style> wholesale.
        for (tag, close) in [("<script", "</script>"), ("<style", "</style>")] {
            if lower[i..].starts_with(tag) {
                if let Some(end) = lower[i..].find(close) {
                    i += end + close.len();
                } else {
                    i = html.len();
                }
            }
        }
        if i >= html.len() {
            break;
        }
        let rest = &html[i..];
        if rest.starts_with('<') {
            if let Some(end) = rest.find('>') {
                i += end + 1;
                out.push(' ');
                continue;
            }
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    // Collapse runs of whitespace.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "webfetch"
    }
    fn description(&self) -> &'static str {
        "Fetch a URL over HTTP(S) and return its content. `format` may be \
         `text` (HTML stripped to text, the default), `markdown`, or `html` \
         (raw)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "The URL to fetch."},
                "format": {"type": "string", "enum": ["text", "markdown", "html"], "description": "Output format (default text)."},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default 30)."}
            },
            "required": ["url"]
        })
    }
    fn kind(&self) -> &'static str {
        "fetch"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("webfetch", format!("fetch {url}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let url = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("webfetch: `url` is required"))?;
        let format = input
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("text");
        let secs = input
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(secs))
            .build()
            .unwrap_or_default();
        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(ToolOutput::error("webfetch: interrupted")),
            r = client.get(url).send() => match r {
                Ok(r) => r,
                Err(e) => return Ok(ToolOutput::error(format!("webfetch: {e}"))),
            }
        };
        let status = resp.status();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Ok(ToolOutput::error(format!("webfetch: reading body: {e}"))),
        };
        if body.len() > MAX_FETCH_BYTES {
            return Ok(ToolOutput::error(format!(
                "webfetch: response is {} bytes, over the {MAX_FETCH_BYTES} byte cap",
                body.len()
            )));
        }
        let rendered = match format {
            "html" | "markdown" => body,
            _ => html_to_text(&body),
        };
        let text = format!("[{status}] {url}\n\n{}", truncate(&rendered, &ctx.caps));
        Ok(ToolOutput {
            for_model: text,
            display: None,
            is_error: !status.is_success(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let html = "<html><head><style>x{}</style></head><body>Hello <b>world</b><script>ignore()</script></body></html>";
        assert_eq!(html_to_text(html), "Hello world");
    }

    #[tokio::test]
    async fn fetches_and_strips_html() {
        // Minimal one-shot HTTP server.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let body = "<html><body>Hi <i>there</i></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = WebFetch
            .execute(&ctx, json!({"url": format!("http://{addr}/")}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("Hi there"));
    }
}
