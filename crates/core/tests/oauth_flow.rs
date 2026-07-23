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

/// Point the secret cipher at a process-unique temp file via
/// `RYUZI_SECRET_KEY_FILE` instead of the real OS keychain, so these tests stay
/// hermetic. Mirrors `ryuzi_core`'s own `#[cfg(test)]` `use_test_key_file`
/// helper and the copy in `tests/secrets_e2e.rs`. Load-bearing on macOS:
/// persisting an oauth connection encrypts secrets, forcing the process-global
/// cipher; absent this seam that calls the Security-framework keychain, which
/// BLOCKS on a headless CI runner with a locked login keychain and hangs the
/// suite until the job cap. The `Once` makes repeated calls race-free.
fn use_test_key_file() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let path =
            std::env::temp_dir().join(format!("ryuzi-test-secret-{}.key", std::process::id()));
        std::env::set_var("RYUZI_SECRET_KEY_FILE", path);
    });
}

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
    use_test_key_file();
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
        None,
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

/// Reconnecting a `needs_relogin` connection must update that SAME row in
/// place (new tokens, `needs_relogin` cleared) rather than inserting a
/// second, shadowing row — otherwise the stale row (lower priority / earlier
/// `created_at`) keeps winning `route_model`'s `ORDER BY priority ASC` and
/// the "fresh" connection is never actually used.
#[tokio::test]
async fn run_flow_with_existing_id_updates_the_same_connection_instead_of_inserting() {
    let store = mem_store().await;
    let http = reqwest::Client::new();
    let token_port = mock_token_server().await;

    // Seed a stale, needs-relogin oauth connection directly (as if it had
    // been added earlier and its refresh later failed terminally).
    let stale = connections::ConnectionRow {
        id: "stale-conn-1".into(),
        provider: "anthropic-oauth".into(),
        auth_type: "oauth".into(),
        label: "Claude sub".into(),
        priority: 0,
        enabled: true,
        data: connections::ConnectionData {
            access_token: Some("stale-at".into()),
            refresh_token: Some("stale-rt".into()),
            expires_at: Some(1),
            needs_relogin: Some(true),
            ..Default::default()
        },
        created_at: 1,
        updated_at: 1,
    };
    connections::add_connection(&store, stale).await.unwrap();
    // add_connection ignores row.priority and always assigns the row its own
    // id, so re-fetch to get the real persisted id.
    let seeded = connections::list_connections(&store).await.unwrap();
    assert_eq!(seeded.len(), 1);
    let stale_id = seeded[0].id.clone();

    let conn = oauth::callback::run_flow_with_token_url(
        &store,
        &http,
        "anthropic-oauth",
        "Claude sub",
        &format!("http://127.0.0.1:{token_port}/v1/oauth/token"),
        Some(&stale_id),
        move |url| {
            let u = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            let redirect = q["redirect_uri"].clone();
            let state = q["state"].clone();
            tokio::spawn(async move {
                let cb = format!("{redirect}?code=fake-code&state={state}");
                let _ = reqwest::get(&cb).await;
            });
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();

    // (b) same id — no new row minted.
    assert_eq!(conn.id, stale_id);
    // (c) tokens updated to the new values from the mock token server.
    assert_eq!(conn.data.access_token.as_deref(), Some("at-1"));
    assert_eq!(conn.data.refresh_token.as_deref(), Some("rt-1"));
    // (d) needs_relogin cleared.
    assert_eq!(conn.data.needs_relogin, Some(false));

    // (a) still exactly ONE connection — the reconnect did not insert a
    // second, shadowing row.
    let after = connections::list_connections(&store).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, stale_id);
    assert_eq!(after[0].data.access_token.as_deref(), Some("at-1"));
    assert_eq!(after[0].data.needs_relogin, Some(false));
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
        None,
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
        None,
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
