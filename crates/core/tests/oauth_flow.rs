//! End-to-end: loopback callback server + `run_flow` orchestration. The
//! "browser" here is a fake: given the authorize URL, it extracts
//! `redirect_uri` + `state` and GETs the callback directly, simulating the
//! provider's redirect after the user approves in a real browser.
//!
//! NOTE: this suite intentionally only exercises `anthropic-oauth`
//! (`RedirectMode::LoopbackRandom`) so ports never collide — `openai-oauth`
//! is fixed to `:1455` (Codex's requirement) and binding it here could
//! collide with a real Codex login or a parallel test run.
use ryuzi_core::llm_router::{connections, oauth};
use ryuzi_core::Store;
use std::sync::Arc;
use std::time::Duration;

/// Mock Anthropic token server; returns canned tokens for any code.
async fn mock_token_server() -> u16 {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/v1/oauth/token",
        post(|Json(_b): Json<serde_json::Value>| async move {
            Json(
                serde_json::json!({"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}),
            )
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    port
}

async fn mem_store() -> Arc<Store> {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    Arc::new(Store::open(tmp.path()).await.unwrap())
}

#[tokio::test]
async fn run_flow_persists_oauth_connection_when_browser_hits_callback() {
    let store = mem_store().await;
    let http = reqwest::Client::new();
    let token_port = mock_token_server().await;

    // The "browser": once given the authorize URL, extract redirect_uri+state
    // and GET the callback with a fake code (simulating the provider redirect).
    let hit = std::sync::Arc::new(tokio::sync::Notify::new());
    let store2 = store.clone();
    let conn = oauth::callback::run_flow_with_token_url(
        &store,
        &http,
        "anthropic-oauth",
        "Claude sub",
        &format!("http://127.0.0.1:{token_port}/v1/oauth/token"),
        move |url| {
            // parse redirect_uri + state out of the authorize URL and hit it
            let u = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            let redirect = q["redirect_uri"].clone();
            let state = q["state"].clone();
            tokio::spawn(async move {
                let cb = format!("{redirect}?code=fake-code&state={state}");
                let _ = reqwest::get(&cb).await;
            });
            let _ = &store2;
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();

    assert_eq!(conn.provider, "anthropic-oauth");
    assert_eq!(conn.auth_type, "oauth");
    assert_eq!(conn.data.access_token.as_deref(), Some("at-1"));
    assert_eq!(conn.data.refresh_token.as_deref(), Some("rt-1"));
    assert!(conn.data.expires_at.unwrap() > 0);
    // persisted
    assert_eq!(
        connections::list_connections(&store).await.unwrap().len(),
        1
    );
    let _ = hit;
}

#[tokio::test]
async fn run_flow_errors_and_persists_nothing_on_state_mismatch() {
    let store = mem_store().await;
    let http = reqwest::Client::new();
    let token_port = mock_token_server().await;

    let err = oauth::callback::run_flow_with_token_url(
        &store,
        &http,
        "anthropic-oauth",
        "Claude sub",
        &format!("http://127.0.0.1:{token_port}/v1/oauth/token"),
        move |url| {
            let u = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            let redirect = q["redirect_uri"].clone();
            // Deliberately send the WRONG state — simulates a forged / stale
            // callback that shouldn't be able to complete the flow.
            tokio::spawn(async move {
                let cb = format!("{redirect}?code=fake-code&state=not-the-real-state");
                let _ = reqwest::get(&cb).await;
            });
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().to_lowercase().contains("state"),
        "unexpected error: {err}"
    );
    assert_eq!(
        connections::list_connections(&store).await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn run_flow_errors_without_panicking_when_callback_is_missing_code() {
    let store = mem_store().await;
    let http = reqwest::Client::new();
    let token_port = mock_token_server().await;

    let err = oauth::callback::run_flow_with_token_url(
        &store,
        &http,
        "anthropic-oauth",
        "Claude sub",
        &format!("http://127.0.0.1:{token_port}/v1/oauth/token"),
        move |url| {
            let u = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            let redirect = q["redirect_uri"].clone();
            let state = q["state"].clone();
            // No `code` param at all — degrade to an error, don't hang or panic.
            tokio::spawn(async move {
                let cb = format!("{redirect}?state={state}");
                let _ = reqwest::get(&cb).await;
            });
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().to_lowercase().contains("code"), "{err}");
    assert_eq!(
        connections::list_connections(&store).await.unwrap().len(),
        0
    );
}
