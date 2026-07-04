//! End-to-end: mock upstream (OpenAI format) behind the router; client talks
//! Anthropic (/v1/messages) and OpenAI (/v1/chat/completions).
use ryuzi_core::llm_router::{connections, keys, server::RouterServer};
use ryuzi_core::Store;
use serde_json::json;
use std::sync::Arc;

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
    let (up_port, _h) = mock_truncating_openai_upstream().await;
    // custom-openai connection pointing at the truncating mock
    connections::add_connection(&store, connections::ConnectionRow {
        id: "c1".into(), provider: "custom-openai".into(), auth_type: "api_key".into(),
        label: "mock".into(), priority: 0, enabled: true,
        data: connections::ConnectionData {
            api_key: Some("k".into()),
            base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
            models_override: Some(vec!["mock-model".into()]),
        },
        created_at: 0, updated_at: 0,
    }).await.unwrap();
    let key = keys::create_key(&store, "t").await.unwrap();
    let srv = RouterServer::new(store.clone());
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(&json!({"model": "custom-openai/mock-model", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    // saw the partial content...
    assert!(body.contains("content_block_delta"));
    // ...and a terminal ERROR event, NOT a clean message_stop
    assert!(body.contains("event: error"), "expected error frame, got: {body}");
    assert!(!body.contains("event: message_stop"), "must not fake a clean finish: {body}");

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
