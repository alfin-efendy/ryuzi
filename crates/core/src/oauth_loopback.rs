//! Shared loopback OAuth callback primitives, extracted from
//! `llm_router::oauth::callback` so both model-connections OAuth and plugin
//! OAuth (the install wizard) can bind a local listener, capture one
//! redirect, and shut down cleanly. Deliberately neutral: plain ports and
//! paths — no `RedirectMode` (that enum stays in `llm_router::registry`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const CALLBACK_HTML: &str = "<!doctype html><html><body>You can close this tab.</body></html>";

/// Served for any path OTHER than the registered callback path — e.g. a
/// second plugin's vendor redirecting onto the first plugin's live server.
/// Friendlier than axum's bare 404: tells the user how to recover.
const FALLBACK_HTML: &str = "<!doctype html><html><body>This sign-in belongs to a \
different install — copy the code from the address bar and paste it in \
Cockpit.</body></html>";

/// What the loopback callback captured off the query string. Either field
/// can be missing if the provider (or something poking the URL) sends a
/// malformed redirect — callers must degrade to an error, not assume both
/// are present.
#[derive(Debug)]
pub struct CallbackResult {
    pub code: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

type CallbackSlot = Arc<Mutex<Option<oneshot::Sender<CallbackResult>>>>;

async fn handle_callback(
    State(slot): State<CallbackSlot>,
    Query(q): Query<CallbackQuery>,
) -> Html<&'static str> {
    if let Some(tx) = slot.lock().unwrap().take() {
        let _ = tx.send(CallbackResult {
            code: q.code,
            state: q.state,
        });
    }
    Html(CALLBACK_HTML)
}

async fn handle_fallback() -> Html<&'static str> {
    Html(FALLBACK_HTML)
}

/// Bind 127.0.0.1:{port}. Bind failures are mapped to an actionable message
/// naming the port — a fixed loopback port is only ever taken by another
/// sign-in flow already running.
pub async fn bind_fixed(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|_| anyhow!("port {port} already in use — close the other sign-in and retry"))
}

/// Bind an ephemeral loopback port.
pub async fn bind_random() -> Result<TcpListener> {
    TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind the OAuth loopback listener")
}

/// Spawn the loopback callback server on an already-bound `listener`,
/// serving GET `path`; the first matching request wins. This is a plain
/// (non-`async`) fn precisely so `tokio::spawn` runs eagerly — the accept
/// loop is live by the time this returns, before the caller goes on to
/// hand the authorize URL to the browser. Requests to any OTHER path get
/// the friendly [`FALLBACK_HTML`] page instead of a bare 404.
pub fn spawn_callback_server(
    listener: TcpListener,
    path: &str,
) -> (
    tokio::task::JoinHandle<()>,
    oneshot::Receiver<CallbackResult>,
    oneshot::Sender<()>,
) {
    let (result_tx, result_rx) = oneshot::channel::<CallbackResult>();
    let slot: CallbackSlot = Arc::new(Mutex::new(Some(result_tx)));
    let app = Router::new()
        .route(path, get(handle_callback))
        .fallback(handle_fallback)
        .with_state(slot);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    (handle, result_rx, shutdown_tx)
}

/// Wait (up to `timeout`) for the spawned callback server to capture a
/// request, then gracefully shut it down (waits for the in-flight response
/// to finish) regardless of outcome so the task never leaks.
pub async fn await_callback(
    server: tokio::task::JoinHandle<()>,
    result_rx: oneshot::Receiver<CallbackResult>,
    shutdown_tx: oneshot::Sender<()>,
    timeout: Duration,
) -> Result<CallbackResult> {
    let received = tokio::time::timeout(timeout, result_rx).await;
    let _ = shutdown_tx.send(());
    let _ = server.await;

    received
        .context("timed out waiting for the OAuth callback")?
        .context("callback listener closed before receiving a request")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn callback_capture_returns_code_and_state() {
        let listener = bind_random().await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (server, result_rx, shutdown_tx) =
            spawn_callback_server(listener, "/plugin-oauth/acme/callback");
        let url =
            format!("http://127.0.0.1:{port}/plugin-oauth/acme/callback?code=c-1&state=s-1");
        let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
        assert!(body.contains("You can close this tab"), "{body}");
        let result = await_callback(server, result_rx, shutdown_tx, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result.code.as_deref(), Some("c-1"));
        assert_eq!(result.state.as_deref(), Some("s-1"));
    }

    #[tokio::test]
    async fn unmatched_path_gets_the_friendly_fallback_page_without_consuming_the_slot() {
        let listener = bind_random().await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (server, result_rx, shutdown_tx) =
            spawn_callback_server(listener, "/plugin-oauth/acme/callback");
        // Another plugin's vendor redirecting onto this live server.
        let other =
            format!("http://127.0.0.1:{port}/plugin-oauth/other/callback?code=x&state=y");
        let body = reqwest::get(&other).await.unwrap().text().await.unwrap();
        assert!(body.contains("paste it in"), "{body}");
        // The real callback still works afterwards.
        let url =
            format!("http://127.0.0.1:{port}/plugin-oauth/acme/callback?code=c-2&state=s-2");
        let _ = reqwest::get(&url).await.unwrap();
        let result = await_callback(server, result_rx, shutdown_tx, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result.code.as_deref(), Some("c-2"));
        assert_eq!(result.state.as_deref(), Some("s-2"));
    }

    #[tokio::test]
    async fn bind_fixed_conflict_is_actionable() {
        let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();
        let err = bind_fixed(port).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("already in use"), "{msg}");
    }

    #[tokio::test]
    async fn timeout_shuts_the_server_down_and_frees_the_port() {
        let listener = bind_random().await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (server, result_rx, shutdown_tx) = spawn_callback_server(listener, "/callback");
        let err = await_callback(server, result_rx, shutdown_tx, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        // Port must be released once the server shut down.
        bind_fixed(port).await.unwrap();
    }
}
