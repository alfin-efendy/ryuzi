//! End-to-end: mock upstream (OpenAI format) behind the router; client talks
//! Anthropic (/v1/messages) and OpenAI (/v1/chat/completions).
use ryuzi_core::llm_router::{connections, keys, server::RouterServer};
use ryuzi_core::Store;
use serde_json::json;
use std::sync::Arc;

/// Register `custom-openai`/`custom-anthropic` as user custom providers so the
/// router resolves their wire format/auth exactly as the (now-removed) static
/// catalog entries did: OpenAI/Bearer and Anthropic/x-api-key, base-URL-driven.
/// Idempotent; safe to call from every test setup path.
fn register_custom_test_providers() {
    use ryuzi_core::llm_router::custom::{register, CustomProvider};
    register(&CustomProvider {
        id: "custom-openai".into(),
        name: "Custom (OpenAI-compatible)".into(),
        format: "openai".into(),
        color: "#8B8B8B".into(),
        initial: "C".into(),
        created_at: 0,
    });
    register(&CustomProvider {
        id: "custom-anthropic".into(),
        name: "Custom (Anthropic-compatible)".into(),
        format: "anthropic".into(),
        color: "#8B8B8B".into(),
        initial: "C".into(),
        created_at: 0,
    });
}

async fn mock_openai_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::{IntoResponse, Response};
    use axum::{routing::post, Json, Router};
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|Json(body): Json<serde_json::Value>| async move {
                let content = body["messages"][0]["content"].as_str().unwrap_or_default();
                // Small fixture requests keep the original strict check; the
                // large-body test below sends a multi-MB payload instead.
                if content.len() <= 64 {
                    assert_eq!(content, "hi");
                }
                if body["stream"].as_bool().unwrap_or(false) {
                    let sse = concat!(
                        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"He\"}}]}\n\n",
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"}}]}\n\n",
                        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\n",
                        "data: [DONE]\n\n",
                    );
                    return Response::builder()
                        .status(200)
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from(sse))
                        .unwrap()
                        .into_response();
                }
                Json(json!({
                    "id": "chatcmpl-mock", "object": "chat.completion", "model": body["model"],
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "hello from mock"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 2}
                }))
                .into_response()
            }),
        )
        // Stand-in for a real upstream: real OpenAI-compatible APIs don't run
        // behind axum's 2 MB default, so the mock shouldn't either — this
        // test is about the router's own limit, not the mock's.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

/// Mock ANTHROPIC-format upstream (`POST /v1/messages`) that always replies
/// with a canned SSE stream — used by the OpenAI-client-over-Anthropic-
/// upstream streaming test.
async fn mock_anthropic_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/v1/messages",
        post(|Json(_body): Json<serde_json::Value>| async move {
            let sse = concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"mock-model\"}}\n\n",
                "event: content_block_start\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
                "event: content_block_stop\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            );
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(sse))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

async fn setup() -> (Arc<Store>, String, u16) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (_, path) = tmp.keep().unwrap();
    let store = Arc::new(Store::open(&path).await.unwrap());
    register_custom_test_providers();
    let (up_port, _h) = mock_openai_upstream().await;
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
                api_key: Some("sk-up".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap(); // 0 → ephemeral
                                            // Keep the server alive for the test duration by leaking it.
    std::mem::forget(srv);
    (store, key.key, port)
}

/// Like [`setup`], but wires up BOTH the OpenAI mock upstream (connection
/// `c1`, provider `custom-openai`) and an Anthropic mock upstream
/// (connection `c2`, provider `custom-anthropic`) so cross-format streaming
/// can be exercised in both directions.
async fn setup_with_anthropic() -> (Arc<Store>, String, u16) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (_, path) = tmp.keep().unwrap();
    let store = Arc::new(Store::open(&path).await.unwrap());
    register_custom_test_providers();
    let (up_port, _h1) = mock_openai_upstream().await;
    let (anthropic_port, _h2) = mock_anthropic_upstream().await;
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
                api_key: Some("sk-up".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "c2".into(),
            provider: "custom-anthropic".into(),
            auth_type: "api_key".into(),
            label: "mock-anthropic".into(),
            priority: 1,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-up2".into()),
                base_url_override: Some(format!("http://127.0.0.1:{anthropic_port}/v1")),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap(); // 0 → ephemeral
    std::mem::forget(srv);
    (store, key.key, port)
}

#[tokio::test]
async fn anthropic_client_routes_to_openai_upstream() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 64,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "hello from mock");
    assert_eq!(body["stop_reason"], "end_turn");
}

#[tokio::test]
async fn openai_client_passes_through() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "custom-openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello from mock");
}

#[tokio::test]
async fn missing_or_bad_key_is_rejected_in_client_format() {
    let (_store, _key, port) = setup().await;
    let client = reqwest::Client::new();
    let r1 = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .json(&json!({"model": "x", "max_tokens": 1, "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 401);
    let b1: serde_json::Value = r1.json().await.unwrap();
    assert_eq!(b1["type"], "error");
    let r2 = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer ryz-wrong")
        .json(&json!({"model": "x", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 401);
    let b2: serde_json::Value = r2.json().await.unwrap();
    assert!(b2.get("error").is_some());
}

#[tokio::test]
async fn unknown_model_is_404_and_models_lists_connections() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(&json!({"model": "nope/nope", "max_tokens": 1,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let m: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("x-api-key", &key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m["data"][0]["id"], "custom-openai/mock-model");
}

/// /v1/models lists family-scoped ids: connections whose providers share a
/// family (`anthropic` + `anthropic-oauth`) collapse into ONE
/// `anthropic/<model>` entry owned by the family, and no raw
/// `anthropic-oauth/<model>` id leaks.
#[tokio::test]
async fn models_dedupes_family_members_under_family_id() {
    let (store, key, port) = setup().await;
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "ca".into(),
            provider: "anthropic".into(),
            auth_type: "api_key".into(),
            label: "anthropic api".into(),
            priority: 1,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-ant".into()),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "coauth".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "claude sub".into(),
            priority: 2,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("at-token".into()),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let m: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("x-api-key", &key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = m["data"].as_array().unwrap();
    let family_entries: Vec<_> = data
        .iter()
        .filter(|e| e["id"] == "anthropic/mock-claude")
        .collect();
    assert_eq!(
        family_entries.len(),
        1,
        "family members must dedupe into one anthropic/mock-claude entry: {data:?}"
    );
    assert_eq!(family_entries[0]["owned_by"], "anthropic");
    assert!(
        data.iter().all(|e| !e["id"]
            .as_str()
            .unwrap_or("")
            .starts_with("anthropic-oauth/")),
        "no raw provider-id model ids may leak: {data:?}"
    );
}

#[tokio::test]
async fn count_tokens_estimates_locally() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let r: serde_json::Value = client
        .post(format!("http://127.0.0.1:{port}/v1/messages/count_tokens"))
        .header("x-api-key", &key)
        .json(&json!({"model": "custom-openai/mock-model",
                      "messages": [{"role": "user", "content": "abcdefgh"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r["input_tokens"].as_i64().unwrap() >= 2);
}

#[tokio::test]
async fn anthropic_client_streams_from_openai_upstream() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 64, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "unexpected content-type: {ct}"
    );
    // The mock upstream sends the whole SSE fixture as one chunk, and the
    // pump task finishes writing before the client-side stream ends — so a
    // plain `.text()` read is enough to observe the full translated body.
    let body = resp.text().await.unwrap();

    let idx_start = body
        .find("event: message_start")
        .expect("message_start missing");
    let idx_cb_start = body
        .find("event: content_block_start")
        .expect("content_block_start missing");
    let idx_he = body
        .find("\"text\":\"He\"")
        .expect("\"He\" text_delta missing");
    let idx_llo = body
        .find("\"text\":\"llo\"")
        .expect("\"llo\" text_delta missing");
    let idx_cb_stop = body
        .find("event: content_block_stop")
        .expect("content_block_stop missing");
    let idx_msg_delta = body
        .find("event: message_delta")
        .expect("message_delta missing");
    assert!(
        body.contains("\"stop_reason\":\"end_turn\""),
        "body: {body}"
    );
    assert!(body.contains("\"output_tokens\":3"), "body: {body}");
    let idx_msg_stop = body
        .find("event: message_stop")
        .expect("message_stop missing");

    assert!(
        idx_start < idx_cb_start,
        "message_start must precede content_block_start"
    );
    assert!(
        idx_cb_start < idx_he,
        "content_block_start must precede first text_delta"
    );
    assert!(idx_he < idx_llo, "\"He\" delta must precede \"llo\" delta");
    assert!(
        idx_llo < idx_cb_stop,
        "text deltas must precede content_block_stop"
    );
    assert!(
        idx_cb_stop < idx_msg_delta,
        "content_block_stop must precede message_delta"
    );
    assert!(
        idx_msg_delta < idx_msg_stop,
        "message_delta must precede message_stop"
    );
}

#[tokio::test]
async fn openai_client_streams_from_anthropic_upstream() {
    let (_store, key, port) = setup_with_anthropic().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(
            &json!({"model": "custom-anthropic/mock-claude", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "unexpected content-type: {ct}"
    );
    let body = resp.text().await.unwrap();

    let idx_role = body
        .find("\"role\":\"assistant\"")
        .expect("role chunk missing");
    let idx_content = body
        .find("\"content\":\"Hi\"")
        .expect("content chunk missing");
    let idx_finish = body
        .find("\"finish_reason\":\"stop\"")
        .expect("finish_reason chunk missing");
    let idx_done = body.find("data: [DONE]").expect("[DONE] missing");

    assert!(
        idx_role < idx_content,
        "role chunk must precede content chunk"
    );
    assert!(
        idx_content < idx_finish,
        "content chunk must precede finish_reason chunk"
    );
    assert!(
        idx_finish < idx_done,
        "finish_reason chunk must precede [DONE]"
    );
}

/// Claude Code posts multi-MB conversations (base64 images) to /v1/messages;
/// axum's 2 MB default body limit must not 413 those.
#[tokio::test]
async fn large_bodies_are_accepted() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let big_content = "a".repeat(3_000_000);
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 8,
                      "messages": [{"role": "user", "content": big_content}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// OpenAI-format upstream that emits one delta then abruptly closes the
/// connection mid-stream (no finish chunk, no [DONE]) — simulates a dropped
/// upstream. Uses a raw TcpListener so we can hang up deliberately.
///
/// The response declares `Transfer-Encoding: chunked` and then closes the
/// socket WITHOUT sending the terminating `0\r\n\r\n` chunk. Without a
/// declared framing (no Content-Length / chunked), HTTP/1.1 falls back to
/// close-delimited bodies, where a closed connection is a *valid* end of
/// body — hyper wouldn't surface that as a stream error at all. Chunked
/// framing makes the early close an actual decode error, which is what
/// `resp.bytes_stream()` needs to yield `Some(Err(_))`.
async fn mock_truncating_openai_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            // Read (and ignore) the request headers+body enough to respond.
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = sock.write_all(head.as_bytes()).await;
            // one valid chunk carrying a delta, then hang up WITHOUT the
            // terminating 0-length chunk, a finish chunk, or [DONE].
            let payload = "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"He\"}}]}\n\n";
            let chunk = format!("{:x}\r\n{payload}\r\n", payload.len());
            let _ = sock.write_all(chunk.as_bytes()).await;
            let _ = sock.flush().await;
            // drop `sock` -> connection reset mid-stream (incomplete chunked body)
        }
    });
    (port, h)
}

#[tokio::test]
async fn anthropic_client_gets_error_frame_when_upstream_truncates() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    register_custom_test_providers();
    let (up_port, _h) = mock_truncating_openai_upstream().await;
    // custom-openai connection pointing at the truncating mock
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
                api_key: Some("k".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    // saw the partial content...
    assert!(body.contains("content_block_delta"));
    // ...and a terminal ERROR event, NOT a clean message_stop
    assert!(
        body.contains("event: error"),
        "expected error frame, got: {body}"
    );
    assert!(
        !body.contains("event: message_stop"),
        "must not fake a clean finish: {body}"
    );

    srv.stop().await;
}

/// OpenAI-format upstream that sends one COMPLETE SSE chunk (a content
/// delta) and then closes the connection cleanly — a normal, well-framed
/// HTTP response (unlike `mock_truncating_openai_upstream`, there's no
/// mid-chunk decode error here). This is the "upstream just stopped talking"
/// case: no finish_reason chunk, no `[DONE]`, but a clean EOF.
async fn mock_clean_eof_no_terminal_openai_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;
    use axum::{routing::post, Router};
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: axum::extract::Json<serde_json::Value>| async move {
            let sse =
                "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"He\"}}]}\n\n";
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(sse))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

/// I2b: a clean EOF that never carried a terminal event (no finish_reason
/// chunk, no `[DONE]`) must surface as an error to the Anthropic client, not
/// a fake clean `message_stop`.
#[tokio::test]
async fn anthropic_client_gets_error_frame_on_clean_eof_before_terminal() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    register_custom_test_providers();
    let (up_port, _h) = mock_clean_eof_no_terminal_openai_upstream().await;
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
                api_key: Some("k".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    // saw the partial content...
    assert!(body.contains("content_block_delta"), "body: {body}");
    // ...and a terminal ERROR event, NOT a clean message_stop
    assert!(
        body.contains("event: error"),
        "expected error frame on clean EOF before terminal, got: {body}"
    );
    assert!(
        !body.contains("event: message_stop"),
        "must not fake a clean finish on truncated stream: {body}"
    );

    srv.stop().await;
}

#[tokio::test]
async fn passthrough_streaming_preserves_sse() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "custom-openai/mock-model", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "unexpected content-type: {ct}"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("He"), "body: {body}");
    assert!(body.contains("llo"), "body: {body}");
    assert!(body.contains("data: [DONE]"), "body: {body}");
}

#[tokio::test]
async fn responses_endpoint_non_stream_round_trips() {
    let (_store, key, port) = setup().await; // custom-openai -> openai mock, key
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "custom-openai/mock-model",
                      "input": [{"type": "message", "role": "user",
                                 "content": [{"type": "input_text", "text": "hi"}]}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["content"][0]["text"], "hello from mock");
}

#[tokio::test]
async fn responses_endpoint_streams_sse() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "custom-openai/mock-model", "stream": true,
                      "input": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("response.created"));
    assert!(body.contains("response.output_text.delta"));
    assert!(body.contains("response.completed"));
}

/// A served (passthrough) request writes a usage_daily row: validates the
/// recording seam end to end, even though passthrough itself can't observe
/// upstream token counts (zero tokens + status 200 is the intended shape).
#[tokio::test]
async fn served_request_records_usage() {
    let (store, key, port) = setup().await; // existing helper: custom-openai -> mock, key created
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "custom-openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    // record() spawns a detached task; give it a beat.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let rows = store.usage_daily(None, &day).await.unwrap();
    assert!(
        rows.iter()
            .any(|r| r.connection_id == "c1" && r.requests >= 1),
        "expected a usage_daily row for c1, got {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// HTTP endpoint multi-target failover (Tasks 5-8)
// ---------------------------------------------------------------------------

/// Mock ANTHROPIC-format upstream (`POST /v1/messages`) that always replies
/// with a fixed status + JSON body and counts how many requests it received.
/// Used to prove the endpoint handlers fail over from a failing upstream to
/// the next connection in the family chain.
async fn mock_counting_anthropic_upstream(
    status: u16,
    body: serde_json::Value,
) -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicU32>,
) {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{routing::post, Json, Router};
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_for_handler = hits.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_body): Json<serde_json::Value>| {
            let hits = hits_for_handler.clone();
            let body = body.clone();
            async move {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, hits)
}

/// I(5-8)a: two api-key `anthropic` connections (family `anthropic`), both
/// serving `mock-claude`. The FIRST-tried connection's upstream returns a
/// retryable 429 (`quota exceeded`); the handler must fail over to the
/// SECOND, healthy connection and return its 200 body — trying the first
/// upstream exactly once.
#[tokio::test]
async fn messages_fails_over_to_next_connection_on_retryable_upstream_error() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());

    let (quota_port, _h1, quota_hits) =
        mock_counting_anthropic_upstream(429, json!({"error": {"message": "quota exceeded"}}))
            .await;
    let (ok_port, _h2, ok_hits) = mock_counting_anthropic_upstream(
        200,
        json!({
            "id": "msg_ok", "type": "message", "role": "assistant", "model": "mock-claude",
            "content": [{"type": "text", "text": "second wins"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
    )
    .await;

    // The first-added connection is tried first (list order = priority ASC =
    // insertion order), so the quota-limited mock leads and the router must
    // fail over to the healthy one.
    for (id, up_port) in [("c-quota", quota_port), ("c-ok", ok_port)] {
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: id.into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: id.into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData {
                    api_key: Some("sk-ant".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                    models_override: Some(vec!["mock-claude".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
    }

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude", "max_tokens": 16,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "expected failover to the healthy second connection"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "second wins");

    assert_eq!(
        quota_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the quota-limited upstream must be tried exactly once"
    );
    assert_eq!(
        ok_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the healthy upstream must be tried exactly once"
    );

    // The FAILED (429) attempt is recorded in usage too — per-account error
    // visibility must survive a failover. Before this was wired, only the
    // healthy connection left a usage row. record() spawns a detached task, so
    // give it a beat, then confirm the quota-limited connection has a row.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let rows = store.usage_daily(None, &day).await.unwrap();
    assert!(
        rows.iter()
            .any(|r| r.connection_id == "c-quota" && r.requests >= 1),
        "the failed (429) attempt must record a usage_daily row for c-quota, got {rows:?}"
    );

    srv.stop().await;
}

/// I(5-8)b: a NON-retryable upstream error (400 `invalid request format`)
/// is returned to the client as-is; the handler must NOT fail over to the
/// second connection (pins the `should_try_next_target` wiring).
#[tokio::test]
async fn messages_does_not_fail_over_on_non_retryable_upstream_error() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());

    let (bad_port, _h1, bad_hits) = mock_counting_anthropic_upstream(
        400,
        json!({"error": {"message": "invalid request format"}}),
    )
    .await;
    let (ok_port, _h2, ok_hits) = mock_counting_anthropic_upstream(
        200,
        json!({
            "id": "msg_ok", "type": "message", "role": "assistant", "model": "mock-claude",
            "content": [{"type": "text", "text": "should not reach me"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
    )
    .await;

    for (id, up_port) in [("c-bad", bad_port), ("c-ok", ok_port)] {
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: id.into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: id.into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData {
                    api_key: Some("sk-ant".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                    models_override: Some(vec!["mock-claude".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
    }

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude", "max_tokens": 16,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        400,
        "a non-retryable 400 must be returned, not failed over"
    );
    assert_eq!(
        bad_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the failing upstream must be tried exactly once"
    );
    assert_eq!(
        ok_hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the second connection must not be tried on a non-retryable error"
    );

    srv.stop().await;
}

// ---------------------------------------------------------------------------
// F2b: OAuth/free upstream auth + refresh wiring (Task 6)
// ---------------------------------------------------------------------------

/// Mock Anthropic-format upstream (`POST /v1/messages`) that captures the
/// request headers + body it received, so the test can assert on the OAuth
/// headers and the injected Claude-Code system prompt.
async fn mock_anthropic_oauth_upstream() -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Option<(axum::http::HeaderMap, serde_json::Value)>>>,
) {
    use axum::{routing::post, Json, Router};
    let captured: Arc<std::sync::Mutex<Option<(axum::http::HeaderMap, serde_json::Value)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_for_handler = captured.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| {
                let captured = captured_for_handler.clone();
                async move {
                    *captured.lock().unwrap() = Some((headers, body));
                    Json(json!({
                        "id": "msg_mock", "type": "message", "role": "assistant",
                        "model": "mock-claude",
                        "content": [{"type": "text", "text": "hi from claude"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 3, "output_tokens": 2}
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

/// I6a: an anthropic-oauth connection's upstream request carries the OAuth
/// bearer (from `data.access_token`, never `data.api_key`) + the
/// `anthropic-beta` OAuth/Claude-Code header, and the outgoing body has the
/// Claude-Code system prompt injected ahead of the caller's own system text.
#[tokio::test]
async fn oauth_anthropic_upstream_receives_bearer_beta_header_and_system_prompt() {
    let (up_port, _h, captured) = mock_anthropic_oauth_upstream().await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "coauth".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "claude sub".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("at-secret-token".into()),
                refresh_token: Some("rt-secret".into()),
                // Far enough out that proactive `ensure_fresh` is a no-op —
                // this test is about header/body shape, not refresh timing.
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude", "max_tokens": 32,
                      "system": "be terse",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let response_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(response_body["content"][0]["text"], "hi from claude");

    let (headers, body) = captured.lock().unwrap().clone().expect("upstream not hit");
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer at-secret-token"
    );
    let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
    assert!(beta.contains("claude-code-20250219"));
    assert!(beta.contains("oauth-2025-04-20"));
    assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(body["system"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("x-anthropic-billing-header: cc_version=2.1.92."));
    assert_eq!(
        body["system"][1]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude."
    );
    assert_eq!(body["system"][2]["text"], "be terse");

    srv.stop().await;
}

#[tokio::test]
async fn anthropic_oauth_streaming_and_json_responses_decloak_tools() {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::{IntoResponse, Response};
    use axum::{routing::post, Json, Router};

    let app = Router::new().route(
        "/v1/messages",
        post(|Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["tools"][0]["name"], "lookup_ide");
            if body["stream"].as_bool().unwrap_or(false) {
                let sse = concat!(
                    "event: message_start\n",
                    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tool_stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"mock-claude\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
                    "event: content_block_start\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_stream\",\"name\":\"lookup_ide\",\"input\":{}}}\n\n",
                    "event: content_block_stop\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "event: message_delta\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
                    "event: message_stop\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                );
                return Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(sse))
                    .unwrap()
                    .into_response();
            }
            Json(json!({
                "id": "msg_tool", "type": "message", "role": "assistant",
                "model": "mock-claude",
                "content": [{"type": "tool_use", "id": "tu_1", "name": "lookup_ide", "input": {"q": "x"}}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 3, "output_tokens": 2}
            }))
            .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_port = listener.local_addr().unwrap().port();
    let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "coauth-cloak".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "claude sub".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("sk-ant-oat-test".into()),
                refresh_token: Some("rt-secret".into()),
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "function": {
            "name": "lookup", "description": "Lookup data",
            "parameters": {"type": "object"}
        }}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let response_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        response_body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup"
    );

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude", "stream": true,
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "function": {
            "name": "lookup", "description": "Lookup data",
            "parameters": {"type": "object"}
        }}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let stream_body = resp.text().await.unwrap();
    assert!(stream_body.contains("lookup"), "body: {stream_body}");
    assert!(!stream_body.contains("lookup_ide"), "body: {stream_body}");

    srv.stop().await;
}

/// Mock OAuth token endpoint for the reactive-401 test: records how many
/// times it was hit and always returns a fresh access token. The router's
/// per-`AppState` `oauth_token_url_override` (set via
/// `RouterServer::set_oauth_token_url_override`) points the reactive
/// `force_refresh` call at this instead of the real, static registry token
/// URL — so this test exercises the ACTUAL network refresh, not a stand-in.
async fn mock_oauth_token_server() -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicU32>,
) {
    use axum::{routing::post, Json, Router};
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_for_handler = hits.clone();
    let app = Router::new().route(
        "/token",
        post(move |Json(_b): Json<serde_json::Value>| {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({
                    "access_token": "at-refreshed-real",
                    "refresh_token": "rt-refreshed-real",
                    "expires_in": 3600
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, hits)
}

/// Mock Anthropic-format upstream for the reactive-401 test: the FIRST call
/// returns 401 (simulating the upstream rejecting a token that still looks
/// fresh, e.g. revoked server-side); the SECOND (retried) call returns 200.
/// Captures the `authorization` header of every call so the test can assert
/// the retry actually carried the newly-refreshed token, not the stale one.
async fn mock_anthropic_oauth_upstream_401_then_200_capturing_auth() -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Vec<Option<String>>>>,
) {
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::{routing::post, Json, Router};
    let auths: Arc<std::sync::Mutex<Vec<Option<String>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let auths_for_handler = auths.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(
            move |headers: HeaderMap, Json(_body): Json<serde_json::Value>| {
                let auths = auths_for_handler.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    let call_index = {
                        let mut g = auths.lock().unwrap();
                        g.push(auth);
                        g.len()
                    };
                    if call_index == 1 {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error": {"message": "token expired"}})),
                        )
                            .into_response();
                    }
                    Json(json!({
                        "id": "msg_retry", "type": "message", "role": "assistant",
                        "model": "mock-claude",
                        "content": [{"type": "text", "text": "recovered"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    }))
                    .into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, auths)
}

/// I6b: a 401 from an oauth-target upstream triggers exactly one
/// refresh-and-retry; the refresh genuinely hits the (mock) token endpoint
/// even though the connection's `expires_at` looks fresh (the reactive path
/// must never short-circuit on stale-looking freshness — that's the whole
/// point of a 401-triggered refresh), the retry succeeds, and the RETRIED
/// upstream call carries the NEWLY refreshed access token (not the stale
/// one the first call used).
#[tokio::test]
async fn oauth_401_triggers_refresh_and_retries_once() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "coauth2".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "claude sub".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("at-stale".into()),
                refresh_token: Some("rt-stale".into()),
                // Far enough out that a naive freshness check (and the
                // PROACTIVE ensure_fresh path) would call this fresh — this
                // test is specifically about the REACTIVE 401 path
                // refreshing unconditionally anyway.
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                models_override: Some(vec!["mock-claude".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let (token_port, _h_token, token_hits) = mock_oauth_token_server().await;
    let (up_port, _h_up, captured_auths) =
        mock_anthropic_oauth_upstream_401_then_200_capturing_auth().await;
    {
        let mut conn = connections::get_connection(&store, "coauth2")
            .await
            .unwrap()
            .unwrap();
        conn.data.base_url_override = Some(format!("http://127.0.0.1:{up_port}/v1"));
        connections::update_connection(&store, conn).await.unwrap();
    }

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    srv.set_oauth_token_url_override(Some(format!("http://127.0.0.1:{token_port}/token")));
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "anthropic/mock-claude", "max_tokens": 16,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "expected the retried call to succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "recovered");

    assert!(
        token_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "expected the reactive refresh to have actually hit the (mock) token endpoint"
    );

    let auths = captured_auths.lock().unwrap().clone();
    assert_eq!(
        auths.len(),
        2,
        "expected exactly one retry (two upstream calls total)"
    );
    assert_eq!(auths[0].as_deref(), Some("Bearer at-stale"));
    assert_eq!(
        auths[1].as_deref(),
        Some("Bearer at-refreshed-real"),
        "the retried call must carry the newly refreshed access token"
    );

    let stored = connections::get_connection(&store, "coauth2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.data.access_token.as_deref(),
        Some("at-refreshed-real"),
        "expected the reactive refresh to have updated the stored token"
    );

    srv.stop().await;
}

// ---------------------------------------------------------------------------
// openai-oauth wire-skip on /v1/chat/completions + streaming pre-stream failover
// ---------------------------------------------------------------------------

/// Mock OpenAI-format upstream (`POST /v1/chat/completions`) that replies with
/// a fixed status + JSON body and counts how many requests it received.
async fn mock_counting_openai_upstream(
    status: u16,
    body: serde_json::Value,
) -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicU32>,
) {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{routing::post, Json, Router};
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_for_handler = hits.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(_body): Json<serde_json::Value>| {
            let hits = hits_for_handler.clone();
            let body = body.clone();
            async move {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, hits)
}

/// Mock ANTHROPIC-format upstream (`POST /v1/messages`) that replies 200 with a
/// canned SSE stream carrying `delta_text`, and counts how many requests it
/// received. Used for the streaming pre-stream failover test.
async fn mock_counting_sse_anthropic_upstream(
    delta_text: &'static str,
) -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicU32>,
) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;
    use axum::{routing::post, Json, Router};
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_for_handler = hits.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_body): Json<serde_json::Value>| {
            let hits = hits_for_handler.clone();
            async move {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let sse = format!(
                    concat!(
                        "event: message_start\n",
                        "data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_stream_ok\",\"model\":\"mock-claude\"}}}}\n\n",
                        "event: content_block_start\n",
                        "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
                        "event: content_block_delta\n",
                        "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{delta}\"}}}}\n\n",
                        "event: content_block_stop\n",
                        "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
                        "event: message_delta\n",
                        "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":1}}}}\n\n",
                        "event: message_stop\n",
                        "data: {{\"type\":\"message_stop\"}}\n\n",
                    ),
                    delta = delta_text,
                );
                Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(sse))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, hits)
}

/// I(wire-skip)a: on /v1/chat/completions, an `openai-oauth` (Codex, Responses-
/// only) connection AND an `openai` api-key connection both serve the same
/// `openai/mock-model`. The openai-oauth target must be SKIPPED (it speaks the
/// Responses wire) and the request served by the api-key sibling — NOT a 400.
#[tokio::test]
async fn chat_skips_openai_oauth_when_openai_apikey_sibling_serves_model() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();

    let (up_port, _h, hits) = mock_counting_openai_upstream(
        200,
        json!({
            "id": "chatcmpl-apikey", "object": "chat.completion", "model": "mock-model",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "api-key served"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
    )
    .await;

    // openai-oauth added FIRST (priority 0), so if the wire-skip filter were
    // broken it would be tried ahead of the healthy api-key sibling.
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "c-oauth".into(),
            provider: "openai-oauth".into(),
            auth_type: "oauth".into(),
            label: "chatgpt sub".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("at-codex".into()),
                refresh_token: Some("rt-codex".into()),
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "c-openai".into(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            label: "openai api".into(),
            priority: 1,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-openai".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", key.key))
        .json(&json!({"model": "openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "openai-oauth must be skipped (not 400) when an api-key sibling can serve the model"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "api-key served");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the api-key upstream must be tried exactly once"
    );

    srv.stop().await;
}

/// I(wire-skip)a (only-incompatible half): with ONLY an `openai-oauth`
/// connection serving the model, /v1/chat/completions returns 400 with the
/// wrong-route message (point the tool at /v1/responses) — the skip collapses
/// to an error only when it was the sole candidate.
#[tokio::test]
async fn chat_only_openai_oauth_target_returns_wrong_route_400() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();

    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "c-oauth".into(),
            provider: "openai-oauth".into(),
            auth_type: "oauth".into(),
            label: "chatgpt sub".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("at-codex".into()),
                refresh_token: Some("rt-codex".into()),
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", key.key))
        .json(&json!({"model": "openai/mock-model",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        400,
        "the only candidate being openai-oauth must collapse to a 400 wrong-route error"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("/v1/responses"),
        "expected the wrong-route message pointing at /v1/responses, got: {msg}"
    );

    srv.stop().await;
}

/// I(stream-failover)b: streaming pre-stream failover on /v1/messages. Two
/// api-key `anthropic` connections serve one model; the first upstream returns
/// a retryable 429, the second a valid Anthropic SSE stream. A `stream:true`
/// request must fail over BEFORE any bytes are pumped and return the second
/// mock's stream — trying the first upstream exactly once.
#[tokio::test]
async fn messages_streaming_fails_over_pre_stream_to_next_connection() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());

    let (quota_port, _h1, quota_hits) =
        mock_counting_anthropic_upstream(429, json!({"error": {"message": "quota exceeded"}}))
            .await;
    let (ok_port, _h2, ok_hits) = mock_counting_sse_anthropic_upstream("second-stream-wins").await;

    for (id, up_port) in [("c-quota", quota_port), ("c-ok", ok_port)] {
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: id.into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: id.into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData {
                    api_key: Some("sk-ant".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                    models_override: Some(vec!["mock-claude".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
    }

    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(
            &json!({"model": "anthropic/mock-claude", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "streaming request must fail over to the healthy second connection"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "unexpected content-type: {ct}"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("second-stream-wins"),
        "expected the second mock's stream delta, got: {body}"
    );
    assert_eq!(
        quota_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the quota-limited upstream must be tried exactly once"
    );
    assert_eq!(
        ok_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the healthy streaming upstream must be tried exactly once"
    );

    srv.stop().await;
}

#[tokio::test]
async fn openai_target_gets_max_completion_tokens_while_custom_openai_keeps_max_tokens() {
    use axum::{routing::post, Json, Router};
    use std::sync::Mutex;

    fn ok_completion(model: serde_json::Value) -> serde_json::Value {
        json!({
            "id": "chatcmpl-mock", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "pong"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })
    }

    let captured: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_openai = captured.clone();
    let cap_custom = captured.clone();
    let app = Router::new()
        .route(
            "/openai/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap_openai.clone();
                async move {
                    cap.lock()
                        .unwrap()
                        .push(("openai".to_string(), body.clone()));
                    Json(ok_completion(body["model"].clone()))
                }
            }),
        )
        .route(
            "/custom/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap_custom.clone();
                async move {
                    cap.lock()
                        .unwrap()
                        .push(("custom".to_string(), body.clone()));
                    Json(ok_completion(body["model"].clone()))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (_, path) = tmp.keep().unwrap();
    let store = Arc::new(Store::open(&path).await.unwrap());
    register_custom_test_providers();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "real-openai".into(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            label: "openai".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-oai".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/openai")),
                models_override: Some(vec!["gpt-5.2".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "custom".into(),
            provider: "custom-openai".into(),
            auth_type: "api_key".into(),
            label: "custom".into(),
            priority: 1,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-custom".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/custom")),
                models_override: Some(vec!["mock-model".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();
    std::mem::forget(srv);

    let client = reqwest::Client::new();
    let r1 = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "openai/gpt-5.2", "max_tokens": 64,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    let r2 = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(
            &json!({"model": "custom-openai/mock-model", "max_tokens": 64,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);

    let seen = captured.lock().unwrap().clone();
    let openai_body = &seen.iter().find(|(who, _)| who == "openai").unwrap().1;
    assert_eq!(openai_body["max_completion_tokens"], 64);
    assert!(openai_body.get("max_tokens").is_none());
    let custom_body = &seen.iter().find(|(who, _)| who == "custom").unwrap().1;
    assert_eq!(custom_body["max_tokens"], 64);
    assert!(custom_body.get("max_completion_tokens").is_none());
}

/// `/v1/responses` synthesizes `max_tokens` from the client's
/// `max_output_tokens` (`responses_request_to_chat`), then forwards it
/// verbatim to an OpenAI-format target. An api-key `openai` target still
/// needs the max_tokens -> max_completion_tokens rename applied on that
/// chat-shaped body before it goes upstream, same as the `/v1/messages` path.
#[tokio::test]
async fn responses_endpoint_applies_max_completion_tokens_for_openai_target() {
    use axum::{routing::post, Json, Router};
    use std::sync::Mutex;

    fn ok_completion(model: serde_json::Value) -> serde_json::Value {
        json!({
            "id": "chatcmpl-mock", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "pong"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })
    }

    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();
    let app = Router::new().route(
        "/openai/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let cap = cap.clone();
            async move {
                *cap.lock().unwrap() = Some(body.clone());
                Json(ok_completion(body["model"].clone()))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (_, path) = tmp.keep().unwrap();
    let store = Arc::new(Store::open(&path).await.unwrap());
    register_custom_test_providers();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "real-openai".into(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            label: "openai".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-oai".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/openai")),
                models_override: Some(vec!["gpt-5.2".into()]),
                ..Default::default()
            },
            created_at: 1,
            updated_at: 1,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();
    std::mem::forget(srv);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "openai/gpt-5.2", "max_output_tokens": 64,
                      "input": [{"type": "message", "role": "user",
                                 "content": [{"type": "input_text", "text": "hi"}]}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let seen = captured.lock().unwrap().clone().expect("upstream was hit");
    assert_eq!(seen["max_completion_tokens"], 64);
    assert!(seen.get("max_tokens").is_none());
}
