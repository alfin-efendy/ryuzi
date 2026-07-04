//! End-to-end: mock upstream (OpenAI format) behind the router; client talks
//! Anthropic (/v1/messages) and OpenAI (/v1/chat/completions).
use ryuzi_core::router::{connections, keys, server::RouterServer};
use ryuzi_core::Store;
use serde_json::json;
use std::sync::Arc;

async fn mock_openai_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["messages"][0]["content"], "hi");
            Json(json!({
                "id": "chatcmpl-mock", "object": "chat.completion", "model": body["model"],
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "hello from mock"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 2}
            }))
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
            id: "c1".into(), provider: "custom-openai".into(), auth_type: "api_key".into(),
            label: "mock".into(), priority: 0, enabled: true,
            data: connections::ConnectionData {
                api_key: Some("sk-up".into()),
                base_url_override: Some(format!("http://127.0.0.1:{up_port}/v1")),
                models_override: Some(vec!["mock-model".into()]),
            },
            created_at: 1, updated_at: 1,
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

#[tokio::test]
async fn anthropic_client_routes_to_openai_upstream() {
    let (_store, key, port) = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", &key)
        .json(&json!({"model": "custom-openai/mock-model", "max_tokens": 64,
                      "messages": [{"role": "user", "content": "hi"}]}))
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
        .send().await.unwrap();
    assert_eq!(r1.status(), 401);
    let b1: serde_json::Value = r1.json().await.unwrap();
    assert_eq!(b1["type"], "error");
    let r2 = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer ryz-wrong")
        .json(&json!({"model": "x", "messages": []}))
        .send().await.unwrap();
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
        .send().await.unwrap();
    assert_eq!(r.status(), 404);
    let m: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("x-api-key", &key)
        .send().await.unwrap().json().await.unwrap();
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
        .send().await.unwrap().json().await.unwrap();
    assert!(r["input_tokens"].as_i64().unwrap() >= 2);
}
