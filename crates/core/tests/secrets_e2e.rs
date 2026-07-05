//! End-to-end proof for F3b (OS-keychain credential encryption): a request
//! routed through the real `RouterServer` reaches its upstream with the
//! correct PLAINTEXT credential even though the credential is stored
//! ENCRYPTED at rest, and a legacy plaintext DB row gets swept to
//! encrypted-at-rest on `init_and_sweep` while still routing correctly.
//!
//! Mirrors the mock-upstream / real-`RouterServer` / real-HTTP-POST pattern
//! from `router_server.rs` and `kiro_upstream.rs`.
use rusqlite::params;
use ryuzi_core::llm_router::{connections, keys, secrets, server::RouterServer};
use ryuzi_core::Store;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Mock OpenAI-compatible upstream (`POST /v1/chat/completions`) that
/// captures the incoming `Authorization` header (into an
/// `Arc<Mutex<Option<String>>>`) and replies with a minimal valid non-stream
/// chat-completion response.
async fn mock_openai_upstream_capturing_auth(
) -> (u16, tokio::task::JoinHandle<()>, Arc<Mutex<Option<String>>>) {
    use axum::http::HeaderMap;
    use axum::{routing::post, Json, Router};
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_for_handler = captured.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(
            move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    *captured.lock().unwrap() = auth;
                    Json(json!({
                        "id": "chatcmpl-mock", "object": "chat.completion", "model": body["model"],
                        "choices": [{"index": 0, "finish_reason": "stop",
                                     "message": {"role": "assistant", "content": "hello from mock"}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 2}
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, captured)
}

/// I7a: a connection's secret is encrypted at rest (`add_connection`
/// encrypts field-by-field), yet the router still decrypts it on read and
/// forwards the ORIGINAL PLAINTEXT credential to the upstream. This is the
/// core end-to-end proof that F3b's encryption is fully transparent to
/// routing.
#[tokio::test]
async fn encrypted_connection_serves_plaintext_credential() {
    let (up_port, _h, captured_auth) = mock_openai_upstream_capturing_auth().await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());

    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "c1".into(),
            provider: "custom-openai".into(),
            auth_type: "api_key".into(),
            label: "mock".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-secret-xyz".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();

    // Prove it's encrypted at rest BEFORE serving anything.
    let raw: String = store
        .with_conn(|c| {
            c.query_row(
                "SELECT data FROM provider_connections WHERE id=?1",
                params!["c1"],
                |r| r.get::<_, String>(0),
            )
        })
        .await
        .unwrap();
    assert!(
        raw.contains("enc:v1:"),
        "raw data must contain the encrypted marker: {raw}"
    );
    assert!(
        !raw.contains("sk-secret-xyz"),
        "raw data must NOT contain the plaintext secret: {raw}"
    );

    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", key.key))
        .json(&json!({"model": "custom-openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello from mock");

    // The core proof: the upstream received the DECRYPTED, PLAINTEXT key —
    // not the ciphertext, and not the endpoint key used to auth the client.
    let auth = captured_auth.lock().unwrap().clone();
    assert_eq!(
        auth.as_deref(),
        Some("Bearer sk-secret-xyz"),
        "upstream must receive the plaintext provider api_key"
    );

    srv.stop().await;
}

/// I7b: a pre-F3b row written with plaintext `data`/`key` columns (bypassing
/// the encrypting write paths entirely) gets upgraded to `enc:v1:` ciphertext
/// by `init_and_sweep`, and — before AND after the sweep — the router still
/// routes a request through it with the correct plaintext upstream
/// credential.
#[tokio::test]
async fn legacy_plaintext_db_swept_then_serves() {
    let (up_port, _h, captured_auth) = mock_openai_upstream_capturing_auth().await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());

    // Insert a `custom-openai` connection row DIRECTLY, simulating a pre-F3b
    // database: raw plaintext `data` JSON, bypassing `add_connection`'s
    // encrypting write path entirely.
    let raw_data =
        format!(r#"{{"apiKey":"sk-legacy","baseUrlOverride":"http://127.0.0.1:{up_port}/v1"}}"#);
    store
        .with_conn({
            let raw_data = raw_data.clone();
            move |c| {
                c.execute(
                    "INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at) \
                     VALUES ('c1','custom-openai','api_key','legacy',0,1,?1,1,1)",
                    params![raw_data],
                )
                .map(|_| ())
            }
        })
        .await
        .unwrap();

    // Also insert a raw PLAINTEXT endpoint key (bypassing `create_key`'s
    // encrypting write path) so the sweep has a legacy key row to upgrade
    // too.
    let plaintext_key = "ryz-legacy-plaintext-key";
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO endpoint_keys(id,name,key,created_at,last_used_at) VALUES ('k1','legacy',?1,1,NULL)",
                params![plaintext_key],
            )
            .map(|_| ())
        })
        .await
        .unwrap();

    // Sanity: before the sweep, both columns are still raw plaintext.
    let raw_before: String = store
        .with_conn(|c| {
            c.query_row(
                "SELECT data FROM provider_connections WHERE id='c1'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .await
        .unwrap();
    assert!(raw_before.contains("sk-legacy"));
    assert!(!raw_before.contains("enc:v1:"));
    let raw_key_before: String = store
        .with_conn(|c| {
            c.query_row("SELECT key FROM endpoint_keys WHERE id='k1'", [], |r| {
                r.get::<_, String>(0)
            })
        })
        .await
        .unwrap();
    assert_eq!(raw_key_before, plaintext_key);

    secrets::init_and_sweep(&store).await;

    // The raw columns are now encrypted at rest.
    let raw_after: String = store
        .with_conn(|c| {
            c.query_row(
                "SELECT data FROM provider_connections WHERE id='c1'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .await
        .unwrap();
    assert!(
        raw_after.contains("enc:v1:"),
        "swept connection data must be ciphertext: {raw_after}"
    );
    assert!(
        !raw_after.contains("sk-legacy"),
        "swept connection data must not leak the plaintext secret: {raw_after}"
    );
    let raw_key_after: String = store
        .with_conn(|c| {
            c.query_row("SELECT key FROM endpoint_keys WHERE id='k1'", [], |r| {
                r.get::<_, String>(0)
            })
        })
        .await
        .unwrap();
    assert!(
        raw_key_after.starts_with("enc:v1:"),
        "swept endpoint key must be ciphertext: {raw_key_after}"
    );

    // The swept row still routes: start the router, present the (still
    // plaintext, from the caller's perspective) endpoint key, and confirm
    // the upstream receives the correct plaintext provider credential.
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {plaintext_key}"))
        .json(&json!({"model": "custom-openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello from mock");

    let auth = captured_auth.lock().unwrap().clone();
    assert_eq!(
        auth.as_deref(),
        Some("Bearer sk-legacy"),
        "the swept row must still forward its original plaintext credential upstream"
    );

    srv.stop().await;
}
