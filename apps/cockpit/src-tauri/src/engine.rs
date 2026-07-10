//! Thin client to the engine daemon's control API. Cockpit's Tauri commands
//! proxy through this; the CoreEvent bridge consumes `events()`.

use crate::error::CmdError;
use futures::Stream;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct EngineClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl EngineClient {
    pub fn new(base_url: String, token: String) -> Self {
        EngineClient {
            base_url,
            token,
            // No global timeout: OAuth flows legitimately block for minutes.
            http: reqwest::Client::new(),
        }
    }

    pub async fn rpc<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, CmdError> {
        let resp = self
            .http
            .post(format!("{}/rpc/{}", self.base_url, method))
            .bearer_auth(&self.token)
            .json(&params)
            .send()
            .await
            .map_err(|e| CmdError {
                message: format!("engine unreachable: {e}"),
            })?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| CmdError {
            message: format!("engine returned invalid JSON: {e}"),
        })?;
        if status.is_success() {
            serde_json::from_value(body).map_err(|e| CmdError {
                message: format!("engine result decode failed for {method}: {e}"),
            })
        } else {
            Err(CmdError {
                message: body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("engine error")
                    .to_string(),
            })
        }
    }

    pub async fn resolve_approval(&self, request_id: &str, allow: bool) -> bool {
        let r = self
            .http
            .post(format!("{}/approvals/{}", self.base_url, request_id))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "allow": allow }))
            .send()
            .await;
        match r {
            Ok(resp) => resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("resolved").and_then(|b| b.as_bool()))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Line-parse the SSE stream: accumulate raw bytes, split on '\n', strip
    /// "data: " prefixes, parse JSON. Ignores comments/keep-alives.
    ///
    /// Bytes are buffered as `Vec<u8>` (not `String`) because reqwest chunk
    /// boundaries do not respect UTF-8 character boundaries: a multi-byte
    /// sequence (emoji, CJK, accented text in a Notice/Message payload) can
    /// legitimately split across two chunks. Decoding each chunk
    /// independently would replace the truncated tail with U+FFFD before the
    /// completing bytes ever arrive. Decoding only complete, newline-terminated
    /// lines guarantees every decode sees a full UTF-8 boundary.
    pub async fn events(&self) -> Result<impl Stream<Item = Value>, CmdError> {
        use futures::StreamExt;
        let resp = self
            .http
            .get(format!("{}/events", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| CmdError {
                message: format!("engine events unreachable: {e}"),
            })?;
        let mut buf: Vec<u8> = Vec::new();
        let stream = resp
            .bytes_stream()
            .filter_map(move |chunk| {
                let mut out: Vec<Value> = Vec::new();
                if let Ok(bytes) = chunk {
                    buf.extend_from_slice(&bytes);
                    out = drain_lines(&mut buf);
                }
                async move { Some(futures::stream::iter(out)) }
            })
            .flatten();
        Ok(stream)
    }
}

/// Drain complete (`\n`-terminated) lines from `buf`, decode each as UTF-8,
/// strip the SSE `"data: "` prefix, and parse the payload as JSON. Bytes
/// after the last newline are left in `buf` for the next call so a
/// multi-byte UTF-8 sequence split across chunk boundaries is only decoded
/// once all of its bytes have arrived. Non-data and unparseable lines are
/// skipped.
fn drain_lines(buf: &mut Vec<u8>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = buf.drain(..=nl).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        let line = line.trim_end();
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                out.push(v);
            }
        }
    }
    out
}

const SPAWN_TIMEOUT_MS: u64 = 30_000;

/// Attach to a live daemon, or spawn one (`current_exe --engine-daemon`) and
/// poll daemon.json until Running(port). A live pid whose control API is
/// unreachable/401 is treated as outdated: SIGTERM, wait dead (5s), spawn fresh.
pub async fn connect_or_spawn() -> anyhow::Result<EngineClient> {
    let dir = ryuzi_core::paths::db_path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    if let Some(client) = try_attach(&dir).await {
        return Ok(client);
    }

    // A live-but-unreachable daemon (pre-API version, stale token) blocks
    // attach forever — take over: SIGTERM, wait dead, respawn. Reconcile
    // resumes any interrupted turns after the new daemon boots.
    if let Some(status) = ryuzi_core::daemon_status::read_status(&dir) {
        if status.pid > 0 && ryuzi_core::daemon_status::is_alive(status.pid) {
            eprintln!(
                "[ryuzi] engine daemon (pid {}) unreachable — restarting it",
                status.pid
            );
            ryuzi_core::daemon_status::send_sigterm(status.pid);
            for _ in 0..50 {
                if !ryuzi_core::daemon_status::is_alive(status.pid) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }

    spawn_engine_daemon(&dir)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(SPAWN_TIMEOUT_MS);
    while std::time::Instant::now() < deadline {
        if let Some(client) = try_attach(&dir).await {
            return Ok(client);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    anyhow::bail!("engine daemon did not become reachable within 30s (see daemon.log)")
}

async fn try_attach(dir: &std::path::Path) -> Option<EngineClient> {
    let status = ryuzi_core::daemon_status::read_status(dir)?;
    let state = ryuzi_core::daemon_status::derive_state(
        Some(&status),
        &ryuzi_core::daemon_status::is_alive,
    );
    if !state.running {
        return None;
    }
    let port = status.port?;
    let token = ryuzi_core::control_token::read_token(dir)?;
    let client = EngineClient::new(format!("http://127.0.0.1:{port}"), token);
    // Auth-verified liveness probe: /health is open, so probe an authed route.
    match client
        .rpc::<Option<String>>("get_setting", serde_json::json!({"key": "control_port"}))
        .await
    {
        Ok(_) => Some(client),
        Err(_) => None,
    }
}

fn spawn_engine_daemon(dir: &std::path::Path) -> anyhow::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let log = std::fs::File::options()
        .append(true)
        .create(true)
        .open(dir.join("daemon.log"))?;
    let log2 = log.try_clone()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--engine-daemon")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log2);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    async fn test_server(token: &str) -> (String, Arc<ryuzi_core::ControlPlane>) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        std::mem::forget(tmp);
        let cp = ryuzi_core::ControlPlane::new(store, ryuzi_core::Registries::new()).await;
        let state = ryuzi_core::serve::ApiState {
            router_server: Arc::new(ryuzi_core::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp: cp.clone(),
            token: Some(token.to_string()),
        };
        let port = ryuzi_core::serve::serve(state, 0).await.unwrap();
        (format!("http://127.0.0.1:{port}"), cp)
    }

    #[tokio::test]
    async fn rpc_ok_and_error_envelope() {
        let (base, _cp) = test_server("tok").await;
        let client = EngineClient::new(base, "tok".into());

        let projects: Vec<serde_json::Value> =
            client.rpc("list_projects", json!({})).await.unwrap();
        assert!(projects.is_empty());

        let err = client
            .rpc::<serde_json::Value>("nope", json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.message, "unknown method: nope");
    }

    #[tokio::test]
    async fn events_stream_yields_core_events() {
        use futures::StreamExt;
        let (base, cp) = test_server("tok").await;
        let client = EngineClient::new(base, "tok".into());
        let mut stream = Box::pin(client.events().await.unwrap());
        cp.emit(ryuzi_core::CoreEvent::Notice {
            session_pk: "s".into(),
            text: "ping".into(),
        });
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ev["kind"], "notice");
    }

    /// A multi-byte UTF-8 char (emoji, 4 bytes) split across two reqwest
    /// chunks must survive intact — not get replaced with U+FFFD by
    /// per-chunk lossy decoding.
    #[test]
    fn drain_lines_reassembles_utf8_split_across_chunks() {
        let payload = json!({"kind": "notice", "text": "hello 🎉 world"});
        let line = format!("data: {}\n", payload);
        let bytes = line.into_bytes();
        // Split the buffer mid-sequence, inside the 4-byte emoji encoding,
        // so neither half is valid UTF-8 on its own.
        let emoji_pos = bytes
            .windows(4)
            .position(|w| w == "🎉".as_bytes())
            .expect("emoji bytes present in payload");
        let split_at = emoji_pos + 2; // land inside the 4-byte sequence

        let mut buf: Vec<u8> = Vec::new();
        // First chunk: ends mid-character. No complete line yet, so no
        // events should be produced (and no premature lossy decode).
        buf.extend_from_slice(&bytes[..split_at]);
        let first = drain_lines(&mut buf);
        assert!(first.is_empty(), "no newline yet, nothing should drain");

        // Second chunk: completes the character and the line.
        buf.extend_from_slice(&bytes[split_at..]);
        let events = drain_lines(&mut buf);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["text"], "hello 🎉 world");
        assert!(
            !events[0]["text"].as_str().unwrap().contains('\u{FFFD}'),
            "multi-byte char must not be corrupted into U+FFFD"
        );
    }
}
