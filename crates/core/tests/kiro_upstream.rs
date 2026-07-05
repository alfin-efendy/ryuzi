//! End-to-end: a mock CodeWhisperer (Kiro) upstream serving real AWS
//! event-stream bytes, routed through the actual `RouterServer` and served
//! back out in all three client wire formats (Anthropic `/v1/messages`,
//! OpenAI `/v1/chat/completions`, Responses `/v1/responses`) — the first
//! real exercise of the full request-translate -> CodeWhisperer upstream ->
//! AWS event-stream pump -> client-format output path.
//!
//! Mirrors the pattern in `router_server.rs`: an axum mock bound to
//! `127.0.0.1:0`, a `RouterServer` pointed at it via the test-only
//! `set_kiro_base_override`/`set_oauth_token_url_override` seams, a
//! connection seeded directly into the store, real HTTP calls against the
//! router, and assertions on the served body.
use ryuzi_core::llm_router::{connections, keys, server::RouterServer};
use ryuzi_core::Store;
use serde_json::{json, Value};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// AWS event-stream frame builder (copied from `aws_stream.rs`'s own
// `#[cfg(test)]` module — integration tests can't reach a lib-private test
// helper, so this ~15-line builder is duplicated here verbatim rather than
// exposed as a new public API just for tests).
// ---------------------------------------------------------------------------

/// Build one AWS event-stream frame: single `:event-type` string header +
/// JSON payload. Prelude/message CRC bytes are zeroed (the parser doesn't
/// validate them).
fn frame(event_type: &str, payload: &str) -> Vec<u8> {
    let name = ":event-type";
    let mut headers = Vec::new();
    headers.push(name.len() as u8);
    headers.extend_from_slice(name.as_bytes());
    headers.push(7u8); // string type
    headers.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
    headers.extend_from_slice(event_type.as_bytes());
    let payload_b = payload.as_bytes();
    let total = 4 + 4 + 4 + headers.len() + payload_b.len() + 4;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // prelude CRC (ignored)
    out.extend_from_slice(&headers);
    out.extend_from_slice(payload_b);
    out.extend_from_slice(&[0, 0, 0, 0]); // message CRC (ignored)
    out
}

/// The shared 3-frame success fixture: `assistantResponseEvent` carrying
/// "Hello from Kiro", a `metricsEvent` with token counts, then
/// `messageStopEvent`.
fn three_frame_body() -> Vec<u8> {
    let mut b = frame("assistantResponseEvent", r#"{"content":"Hello from Kiro"}"#);
    b.extend(frame(
        "metricsEvent",
        r#"{"inputTokens":5,"outputTokens":3}"#,
    ));
    b.extend(frame("messageStopEvent", "{}"));
    b
}

const KIRO_PATH: &str = "/generateAssistantResponse";

// ---------------------------------------------------------------------------
// Mock kiro upstreams
// ---------------------------------------------------------------------------

/// Always-succeeds mock: replies with `three_frame_body()` regardless of the
/// request.
async fn mock_kiro_upstream_ok() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;
    use axum::{routing::post, Router};
    let app = Router::new().route(
        KIRO_PATH,
        post(|_body: axum::body::Bytes| async move {
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
                .body(Body::from(three_frame_body()))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

/// Mock: one `toolUseEvent` (tool-call-only, no text) then
/// `messageStopEvent` — used by the non-stream tool-only-content test.
async fn mock_kiro_upstream_tool_only() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;
    use axum::{routing::post, Router};
    let app = Router::new().route(
        KIRO_PATH,
        post(|_body: axum::body::Bytes| async move {
            let mut b = frame(
                "toolUseEvent",
                r#"{"toolUseId":"t1","name":"get_weather","input":{"city":"Paris"}}"#,
            );
            b.extend(frame("messageStopEvent", "{}"));
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
                .body(Body::from(b))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

/// Mock: writes ONE valid AWS event-stream frame (via chunked framing) then
/// hangs up WITHOUT the terminating `messageStopEvent` frame or the
/// terminating `0\r\n\r\n` chunk — a truncated stream, not a completed one.
/// Uses a raw `TcpListener` (like `router_server.rs`'s
/// `mock_truncating_openai_upstream`) so the early close surfaces as a
/// genuine decode error to `resp.bytes_stream()`.
async fn mock_kiro_truncating_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = sock.write_all(head.as_bytes()).await;
            let payload = frame("assistantResponseEvent", r#"{"content":"Hello"}"#);
            let mut chunk = format!("{:x}\r\n", payload.len()).into_bytes();
            chunk.extend_from_slice(&payload);
            chunk.extend_from_slice(b"\r\n");
            let _ = sock.write_all(&chunk).await;
            let _ = sock.flush().await;
            // drop `sock` here -> connection reset mid-stream, no terminal
            // messageStopEvent frame and no terminating chunk.
        }
    });
    (port, h)
}

/// Mock: 403s on the first call, then replies with `three_frame_body()` on
/// the second — used by the 403 -> refresh -> retry test. Captures the
/// `authorization` header seen on each call.
async fn mock_kiro_upstream_403_then_ok() -> (
    u16,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Vec<Option<String>>>>,
) {
    use axum::body::Body;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::Router;
    let auths: Arc<std::sync::Mutex<Vec<Option<String>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let auths_for_handler = auths.clone();
    let app = Router::new().route(
        KIRO_PATH,
        post(move |headers: HeaderMap, _body: axum::body::Bytes| {
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
                    return StatusCode::FORBIDDEN.into_response();
                }
                Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
                    .body(Body::from(three_frame_body()))
                    .unwrap()
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h, auths)
}

/// Mock kiro refresh (social) token endpoint returning a fresh access token.
async fn mock_kiro_refresh_server() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/refreshToken",
        post(|Json(_b): Json<Value>| async move {
            Json(json!({"accessToken": "tok2", "expiresIn": 3600}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, h)
}

// ---------------------------------------------------------------------------
// Setup helper
// ---------------------------------------------------------------------------

/// Seed a `kiro` (builder-id, no client creds) oauth connection with an
/// access token far from expiry (so the PROACTIVE `ensure_fresh` path is a
/// no-op — tests that want the reactive 403 path exercise it explicitly),
/// point `set_kiro_base_override` at `http://127.0.0.1:{kiro_port}{KIRO_PATH}`,
/// create an endpoint key, and start the router. Returns the owned
/// `RouterServer` so the caller can `.stop()` it explicitly.
async fn setup_kiro(kiro_port: u16) -> (Arc<Store>, String, u16, RouterServer) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "k1".into(),
            provider: "kiro".into(),
            auth_type: "oauth".into(),
            label: "kiro free".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("kiro-at".into()),
                provider_specific: Some(json!({"authMethod": "builder-id"})),
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let key = keys::create_key(&store, "test").await.unwrap();
    let srv = RouterServer::new(store.clone());
    srv.set_kiro_base_override(Some(format!("http://127.0.0.1:{kiro_port}{KIRO_PATH}")));
    let port = srv.start(0).await.unwrap();
    (store, key.key, port, srv)
}

// ---------------------------------------------------------------------------
// Step 1: three-frame success, all three client formats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_client_streams_kiro_three_frames() {
    let (up_port, _h) = mock_kiro_upstream_ok().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(
            &json!({"model": "kiro/claude-sonnet-4.5", "max_tokens": 64, "stream": true,
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
    assert!(ct.starts_with("text/event-stream"), "content-type: {ct}");
    let body = resp.text().await.unwrap();

    let idx_start = body
        .find("event: message_start")
        .expect("message_start missing");
    let idx_delta = body
        .find("event: content_block_delta")
        .expect("content_block_delta missing");
    let idx_text = body
        .find("\"text\":\"Hello from Kiro\"")
        .expect("text delta missing");
    let idx_msg_delta = body
        .find("event: message_delta")
        .expect("message_delta missing");
    assert!(body.contains("\"output_tokens\":3"), "body: {body}");
    let idx_stop = body
        .find("event: message_stop")
        .expect("message_stop missing");

    assert!(
        idx_start < idx_delta,
        "message_start must precede content_block_delta"
    );
    assert!(
        idx_delta < idx_text,
        "content_block_delta event must precede its text"
    );
    assert!(
        idx_text < idx_msg_delta,
        "text delta must precede message_delta"
    );
    assert!(
        idx_msg_delta < idx_stop,
        "message_delta must precede message_stop"
    );

    srv.stop().await;
}

#[tokio::test]
async fn openai_client_streams_kiro_three_frames() {
    let (up_port, _h) = mock_kiro_upstream_ok().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "kiro/claude-sonnet-4.5", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"object\":\"chat.completion.chunk\""),
        "body: {body}"
    );
    let idx_content = body.find("Hello from Kiro").expect("content missing");
    let idx_finish = body
        .find("\"finish_reason\":\"stop\"")
        .expect("finish_reason chunk missing");
    let idx_done = body.find("data: [DONE]").expect("[DONE] missing");
    assert!(
        idx_content < idx_finish,
        "content must precede finish_reason"
    );
    assert!(idx_finish < idx_done, "finish_reason must precede [DONE]");

    srv.stop().await;
}

#[tokio::test]
async fn responses_client_streams_kiro_three_frames() {
    let (up_port, _h) = mock_kiro_upstream_ok().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "kiro/claude-sonnet-4.5", "stream": true, "input": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    let idx_delta = body
        .find("response.output_text.delta")
        .expect("response.output_text.delta missing");
    let idx_text = body.find("Hello from Kiro").expect("text missing");
    let idx_completed = body
        .find("response.completed")
        .expect("response.completed missing");
    assert!(idx_delta < idx_text, "delta event must precede its text");
    assert!(
        idx_text < idx_completed,
        "text must precede response.completed"
    );

    srv.stop().await;
}

// ---------------------------------------------------------------------------
// Step 2a: truncation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_client_gets_error_frame_when_kiro_upstream_truncates() {
    let (up_port, _h) = mock_kiro_truncating_upstream().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(
            &json!({"model": "kiro/claude-sonnet-4.5", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 200, "expected 200, got {status}; body: {body}");
    // saw the partial content...
    assert!(body.contains("content_block_delta"), "body: {body}");
    // ...and a terminal ERROR event, NOT a clean message_stop.
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

// ---------------------------------------------------------------------------
// Step 2b: non-stream tool-call-only aggregation (Task 8 review nit)
// ---------------------------------------------------------------------------

/// The OpenAI-format non-stream response for a tool-call-only completion:
/// `aggregate_openai_chunks` (server.rs) always builds `message.content` as
/// a `String` (`String::new()` when there's no text), so a client asking for
/// the raw OpenAI shape sees `content: ""` where a real OpenAI upstream
/// would send `content: null`. This is a pre-existing finding from the Task
/// 8 review, not something this task fixes — asserted explicitly here (not
/// silently passed) so a future fix is visible as an intentional test
/// change, not a silent regression.
#[tokio::test]
async fn openai_client_non_stream_tool_only_response_is_well_formed() {
    let (up_port, _h) = mock_kiro_upstream_tool_only().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model": "kiro/claude-sonnet-4.5",
                      "messages": [{"role": "user", "content": "weather in paris"}]}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let raw = resp.text().await.unwrap();
    assert_eq!(status, 200, "expected 200, got {status}; body: {raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    let msg = &body["choices"][0]["message"];
    assert_eq!(msg["tool_calls"][0]["id"], "t1", "body: {body}");
    assert_eq!(msg["tool_calls"][0]["function"]["name"], "get_weather");
    let args: Value = serde_json::from_str(
        msg["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments must be a JSON string"),
    )
    .unwrap();
    assert_eq!(args["city"], "Paris");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    // FINDING (Task 8 nit, not fixed here): content is "" instead of null.
    assert_eq!(
        msg["content"], "",
        "if this now fails, the nit was fixed upstream of this test — update task-9-report.md"
    );

    srv.stop().await;
}

/// The SAME tool-only upstream fixture served to an Anthropic client: the
/// translation layer (`translate::openai_to_anthropic_response`) already
/// guards against an empty text block (`if !text.is_empty()`), so the
/// content array here must contain ONLY the `tool_use` block — no spurious
/// empty `text` block.
#[tokio::test]
async fn anthropic_client_non_stream_tool_only_response_has_no_empty_text_block() {
    let (up_port, _h) = mock_kiro_upstream_tool_only().await;
    let (_store, key, port, srv) = setup_kiro(up_port).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(&json!({"model": "kiro/claude-sonnet-4.5", "max_tokens": 64,
                      "messages": [{"role": "user", "content": "weather in paris"}]}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let raw = resp.text().await.unwrap();
    assert_eq!(status, 200, "expected 200, got {status}; body: {raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    let content = body["content"]
        .as_array()
        .unwrap_or_else(|| panic!("content must be an array: {body}"));
    assert_eq!(
        content.len(),
        1,
        "expected exactly one (tool_use) block, no empty text block: {body}"
    );
    assert_eq!(content[0]["type"], "tool_use");
    assert_eq!(content[0]["name"], "get_weather");

    srv.stop().await;
}

// ---------------------------------------------------------------------------
// Step 2c: 403 -> refresh -> retry
// ---------------------------------------------------------------------------

/// A kiro connection with a refresh token whose first upstream call 403s:
/// the router refreshes once (via the `oauth_token_url_override` seam now
/// threaded into `refresh_kiro`) and retries, and the retried call carries
/// the newly-refreshed bearer token.
#[tokio::test]
async fn kiro_403_triggers_refresh_and_retries_with_new_bearer() {
    let (up_port, _h_up, auths) = mock_kiro_upstream_403_then_ok().await;
    let (refresh_port, _h_refresh) = mock_kiro_refresh_server().await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    let now = chrono::Utc::now().timestamp_millis();
    connections::add_connection(
        &store,
        connections::ConnectionRow {
            id: "k403".into(),
            provider: "kiro".into(),
            auth_type: "oauth".into(),
            label: "kiro".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                access_token: Some("stale-at".into()),
                refresh_token: Some("rt-1".into()),
                // Far enough out that the PROACTIVE ensure_fresh path is a
                // no-op — this test is specifically about the REACTIVE
                // (post-403) path refreshing unconditionally anyway.
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
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
    srv.set_kiro_base_override(Some(format!("http://127.0.0.1:{up_port}{KIRO_PATH}")));
    srv.set_oauth_token_url_override(Some(format!(
        "http://127.0.0.1:{refresh_port}/refreshToken"
    )));
    let port = srv.start(0).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key.key)
        .json(
            &json!({"model": "kiro/claude-sonnet-4.5", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("event: message_stop"),
        "stream should complete after the refresh+retry: {body}"
    );
    assert!(body.contains("Hello from Kiro"), "body: {body}");

    let seen = auths.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        2,
        "expected exactly one retry (two upstream calls)"
    );
    assert_eq!(seen[0].as_deref(), Some("Bearer stale-at"));
    assert_eq!(
        seen[1].as_deref(),
        Some("Bearer tok2"),
        "the retried call must carry the newly refreshed access token"
    );

    let stored = connections::get_connection(&store, "k403")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.data.access_token.as_deref(), Some("tok2"));

    srv.stop().await;
}
