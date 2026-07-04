//! The local endpoint server: Anthropic + OpenAI compatible surface on
//! 127.0.0.1, gated by endpoint keys, routed to provider connections.
use crate::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderDescriptor};
use crate::llm_router::{connections, keys, sse::SseParser, translate};
use crate::store::Store;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

pub const DEFAULT_PORT: u16 = 21128;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouterStatus {
    pub running: bool,
    pub port: u16,
}

struct Inner {
    shutdown: Option<oneshot::Sender<()>>,
    port: u16,
}

pub struct RouterServer {
    store: Arc<Store>,
    inner: Mutex<Inner>,
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    http: reqwest::Client,
}

impl RouterServer {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            inner: Mutex::new(Inner {
                shutdown: None,
                port: 0,
            }),
        }
    }

    pub fn status(&self) -> RouterStatus {
        let g = self.inner.lock().unwrap();
        RouterStatus {
            running: g.shutdown.is_some(),
            port: g.port,
        }
    }

    /// Bind 127.0.0.1:`port` (0 = ephemeral) and serve. Returns bound port.
    pub async fn start(&self, port: u16) -> anyhow::Result<u16> {
        self.stop().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| anyhow::anyhow!("could not bind port {port}: {e}"))?;
        let bound = listener.local_addr()?.port();
        let state = AppState {
            store: self.store.clone(),
            http: reqwest::Client::new(),
        };
        let app = Router::new()
            .route("/v1/messages", post(handle_messages))
            .route("/v1/messages/count_tokens", post(handle_count_tokens))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/v1/responses", post(handle_responses))
            .route("/v1/models", get(handle_models))
            // Agent conversations with inline images exceed axum's 2 MB default.
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(state);
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        let mut g = self.inner.lock().unwrap();
        g.shutdown = Some(tx);
        g.port = bound;
        drop(g);
        let prune_store = self.store.clone();
        tokio::spawn(async move {
            let cutoff = crate::paths::now_ms() - crate::llm_router::usage::PRUNE_AFTER_MS;
            let _ = prune_store.prune_request_log(cutoff).await;
        });
        Ok(bound)
    }

    pub async fn stop(&self) {
        let tx = { self.inner.lock().unwrap().shutdown.take() };
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// Auth + error shapes
// ---------------------------------------------------------------------------

fn presented_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn anthropic_error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(json!({"type": "error", "error": {
        "type": if status == StatusCode::UNAUTHORIZED { "authentication_error" }
                else if status == StatusCode::NOT_FOUND { "not_found_error" }
                else { "api_error" },
        "message": msg }})),
    )
        .into_response()
}

fn openai_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error": {
        "message": msg,
        "type": if status == StatusCode::UNAUTHORIZED { "invalid_request_error" } else { "api_error" },
        "code": null }})))
        .into_response()
}

async fn check_auth(
    state: &AppState,
    headers: &HeaderMap,
    err: fn(StatusCode, &str) -> Response,
) -> Result<(), Response> {
    let Some(k) = presented_key(headers) else {
        return Err(err(
            StatusCode::UNAUTHORIZED,
            "missing API key (x-api-key or Authorization: Bearer)",
        ));
    };
    match keys::verify_key(&state.store, &k).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(err(StatusCode::UNAUTHORIZED, "invalid API key")),
        Err(e) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Model routing
// ---------------------------------------------------------------------------

pub struct RouteTarget {
    pub conn: connections::ConnectionRow,
    pub desc: &'static ProviderDescriptor,
    pub upstream_model: String,
}

pub async fn route_model(store: &Store, requested: &str) -> anyhow::Result<Option<RouteTarget>> {
    let conns = connections::list_connections(store).await?;
    let enabled: Vec<_> = conns.into_iter().filter(|c| c.enabled).collect();
    if let Some((prov, model)) = requested.split_once('/') {
        for conn in enabled {
            if conn.provider == prov {
                if let Some(desc) = registry::descriptor(&conn.provider) {
                    return Ok(Some(RouteTarget {
                        conn,
                        desc,
                        upstream_model: model.to_string(),
                    }));
                }
            }
        }
        return Ok(None);
    }
    // Bare model: first (highest-priority) connection listing it.
    for conn in enabled {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if connections::effective_models(desc, &conn)
            .iter()
            .any(|m| m == requested)
        {
            return Ok(Some(RouteTarget {
                conn,
                desc,
                upstream_model: requested.to_string(),
            }));
        }
    }
    Ok(None)
}

fn upstream_request(
    state: &AppState,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let base = connections::effective_base_url(target.desc, &target.conn)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
    let path = match target.desc.format {
        ApiFormat::OpenAi => "/chat/completions",
        ApiFormat::Anthropic => "/messages",
    };
    let mut req = state.http.post(format!("{base}{path}")).json(body);
    let key = target.conn.data.api_key.clone().unwrap_or_default();
    req = match target.desc.auth {
        AuthScheme::XApiKey => req
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        AuthScheme::Bearer => req.header("authorization", format!("Bearer {key}")),
        AuthScheme::None => req,
    };
    Ok(req)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Client speaks Anthropic. `client_fmt` differs from `handle_chat` only in
/// error shape + translation direction.
async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, anthropic_error).await {
        return r;
    }
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let target = match route_model(&state.store, &requested).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return anthropic_error(
                StatusCode::NOT_FOUND,
                &format!(
                "no enabled connection serves model '{requested}' — add one in Models → Providers"
            ),
            )
        }
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let stream = body["stream"].as_bool().unwrap_or(false);
    body["model"] = json!(target.upstream_model);

    match target.desc.format {
        ApiFormat::Anthropic => {
            let started = crate::paths::now_ms();
            let resp = proxy_passthrough(&state, &target, &body, anthropic_error).await;
            crate::llm_router::usage::record(
                &state.store,
                &target.conn.id,
                &target.conn.provider,
                &target.upstream_model,
                "anthropic",
                crate::llm_router::usage::Usage::default(),
                resp.status().as_u16(),
                started,
                None,
            );
            resp
        }
        ApiFormat::OpenAi => {
            let upstream_body = match translate::anthropic_to_openai_request(&body) {
                Ok(b) => b,
                Err(e) => return anthropic_error(StatusCode::BAD_REQUEST, &e.to_string()),
            };
            if stream {
                let ctx = RecordCtx {
                    conn_id: target.conn.id.clone(),
                    provider: target.conn.provider.clone(),
                    model: target.upstream_model.clone(),
                    client_format: "anthropic".to_string(),
                    started: crate::paths::now_ms(),
                };
                stream_openai_upstream_to_anthropic(&state, &target, &upstream_body, ctx).await
            } else {
                let started = crate::paths::now_ms();
                match send_json(&state, &target, &upstream_body, anthropic_error).await {
                    Ok(v) => {
                        let u = crate::llm_router::usage::usage_from_openai(&v);
                        crate::llm_router::usage::record(
                            &state.store,
                            &target.conn.id,
                            &target.conn.provider,
                            &target.upstream_model,
                            "anthropic",
                            u,
                            200,
                            started,
                            None,
                        );
                        Json(translate::openai_to_anthropic_response(&v)).into_response()
                    }
                    Err(r) => r,
                }
            }
        }
    }
}

/// Client speaks OpenAI.
async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, openai_error).await {
        return r;
    }
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let target = match route_model(&state.store, &requested).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return openai_error(
                StatusCode::NOT_FOUND,
                &format!("no enabled connection serves model '{requested}'"),
            )
        }
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let stream = body["stream"].as_bool().unwrap_or(false);
    body["model"] = json!(target.upstream_model);

    match target.desc.format {
        ApiFormat::OpenAi => {
            let started = crate::paths::now_ms();
            let resp = proxy_passthrough(&state, &target, &body, openai_error).await;
            crate::llm_router::usage::record(
                &state.store,
                &target.conn.id,
                &target.conn.provider,
                &target.upstream_model,
                "openai",
                crate::llm_router::usage::Usage::default(),
                resp.status().as_u16(),
                started,
                None,
            );
            resp
        }
        ApiFormat::Anthropic => {
            let upstream_body = match translate::openai_to_anthropic_request(&body) {
                Ok(b) => b,
                Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e.to_string()),
            };
            if stream {
                let ctx = RecordCtx {
                    conn_id: target.conn.id.clone(),
                    provider: target.conn.provider.clone(),
                    model: target.upstream_model.clone(),
                    client_format: "openai".to_string(),
                    started: crate::paths::now_ms(),
                };
                stream_anthropic_upstream_to_openai(&state, &target, &upstream_body, ctx).await
            } else {
                let started = crate::paths::now_ms();
                match send_json(&state, &target, &upstream_body, openai_error).await {
                    Ok(v) => {
                        let u = crate::llm_router::usage::usage_from_anthropic(&v);
                        crate::llm_router::usage::record(
                            &state.store,
                            &target.conn.id,
                            &target.conn.provider,
                            &target.upstream_model,
                            "openai",
                            u,
                            200,
                            started,
                            None,
                        );
                        Json(translate::anthropic_to_openai_response(&v)).into_response()
                    }
                    Err(r) => r,
                }
            }
        }
    }
}

/// Client speaks the OpenAI Responses API. Translated to internal chat, routed
/// like /v1/chat/completions, and re-encoded as Responses.
async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, openai_error).await {
        return r;
    }
    let mut chat = crate::llm_router::responses::responses_request_to_chat(&body);
    let requested = chat["model"].as_str().unwrap_or("").to_string();
    let target = match route_model(&state.store, &requested).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return openai_error(
                StatusCode::NOT_FOUND,
                &format!("no enabled connection serves model '{requested}'"),
            )
        }
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let stream = chat["stream"].as_bool().unwrap_or(false);
    chat["model"] = json!(target.upstream_model);
    let started = crate::paths::now_ms();

    // Normalize the upstream response to OpenAI chat shape, then encode Responses.
    let upstream_body = match target.desc.format {
        ApiFormat::OpenAi => chat.clone(),
        ApiFormat::Anthropic => match translate::openai_to_anthropic_request(&chat) {
            Ok(b) => b,
            Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e.to_string()),
        },
    };

    if stream {
        stream_responses(&state, &target, &upstream_body, started).await
    } else {
        match send_json(&state, &target, &upstream_body, openai_error).await {
            Ok(v) => {
                // normalize to OpenAI chat shape first
                let chat_resp = match target.desc.format {
                    ApiFormat::OpenAi => v,
                    ApiFormat::Anthropic => translate::anthropic_to_openai_response(&v),
                };
                let u = crate::llm_router::usage::usage_from_openai(&chat_resp);
                crate::llm_router::usage::record(
                    &state.store,
                    &target.conn.id,
                    &target.conn.provider,
                    &target.upstream_model,
                    "responses",
                    u,
                    200,
                    started,
                    None,
                );
                Json(crate::llm_router::responses::chat_response_to_responses(
                    &chat_resp,
                ))
                .into_response()
            }
            Err(r) => r,
        }
    }
}

async fn handle_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&state, &headers, openai_error).await {
        return r;
    }
    let conns = match connections::list_connections(&state.store).await {
        Ok(c) => c,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let mut data: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for conn in conns.iter().filter(|c| c.enabled) {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        for m in connections::effective_models(desc, conn) {
            let id = format!("{}/{}", conn.provider, m);
            if seen.insert(id.clone()) {
                data.push(json!({"id": id, "object": "model", "owned_by": conn.provider}));
            }
        }
    }
    Json(json!({"object": "list", "data": data})).into_response()
}

/// Claude Code calls this constantly; a chars/4 local estimate is enough for
/// F1 (spec §4.3) — no upstream round-trip.
async fn handle_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, anthropic_error).await {
        return r;
    }
    let chars = serde_json::to_string(&body["messages"])
        .map(|s| s.len())
        .unwrap_or(0)
        + serde_json::to_string(&body["system"])
            .map(|s| s.len())
            .unwrap_or(0);
    Json(json!({"input_tokens": (chars / 4).max(1)})).into_response()
}

// ---------------------------------------------------------------------------
// Upstream I/O
// ---------------------------------------------------------------------------

async fn send_json(
    state: &AppState,
    target: &RouteTarget,
    body: &Value,
    err: fn(StatusCode, &str) -> Response,
) -> Result<Value, Response> {
    let req = upstream_request(state, target, body)
        .map_err(|e| err(StatusCode::BAD_GATEWAY, &e.to_string()))?;
    let resp = req.send().await.map_err(|e| {
        err(
            StatusCode::BAD_GATEWAY,
            &format!("upstream {}: {e}", target.conn.provider),
        )
    })?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        let msg = v["error"]["message"].as_str().unwrap_or("upstream error");
        return Err(err(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("[{}] {}", target.conn.provider, msg),
        ));
    }
    Ok(v)
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .unwrap()
}

/// Same-format streaming (and non-streaming) proxy: forward status + body.
async fn proxy_passthrough(
    state: &AppState,
    target: &RouteTarget,
    body: &Value,
    err: fn(StatusCode, &str) -> Response,
) -> Response {
    let req = match upstream_request(state, target, body) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                &format!("upstream {}: {e}", target.conn.provider),
            )
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let stream = resp.bytes_stream();
    Response::builder()
        .status(status)
        .header("content-type", ct)
        .body(Body::from_stream(stream))
        .unwrap()
}

fn format_sse(name: &str, data: &Value) -> bytes::Bytes {
    bytes::Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

/// Recording context threaded into the streaming pumps so they can record a
/// usage row once the upstream stream ends (clean or errored).
struct RecordCtx {
    conn_id: String,
    provider: String,
    model: String,
    client_format: String,
    started: i64,
}

/// Spawn a task that pumps `resp`'s bytes through SseParser + `tr`, sending
/// formatted SSE bytes down an mpsc channel that backs the response Body.
fn spawn_openai_to_anthropic_pump(
    resp: reqwest::Response,
    model: String,
    store: Arc<Store>,
    ctx: RecordCtx,
) -> Body {
    use futures::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut parser = SseParser::default();
        let mut tr = translate::OpenAiToAnthropicStream::new(&model);
        let mut stream = resp.bytes_stream();
        let mut errored = false;
        'pump: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    for (name, data) in tr.error_frame(&format!("upstream stream interrupted: {e}"))
                    {
                        let _ = tx.send(Ok(format_sse(&name, &data))).await;
                    }
                    errored = true;
                    break;
                }
            };
            for ev in parser.feed(&chunk) {
                if ev.data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    for (name, data) in tr.feed(&v) {
                        if tx.send(Ok(format_sse(&name, &data))).await.is_err() {
                            // Client disconnected; stop pumping but still
                            // record what we saw so far.
                            break 'pump;
                        }
                    }
                }
            }
        }
        if !errored {
            for (name, data) in tr.finish() {
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
            }
        }
        let (input, output) = tr.usage();
        crate::llm_router::usage::record(
            &store,
            &ctx.conn_id,
            &ctx.provider,
            &ctx.model,
            &ctx.client_format,
            crate::llm_router::usage::Usage { input, output },
            if errored { 502 } else { 200 },
            ctx.started,
            errored.then(|| "stream interrupted".to_string()),
        );
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

fn spawn_anthropic_to_openai_pump(
    resp: reqwest::Response,
    store: Arc<Store>,
    ctx: RecordCtx,
) -> Body {
    use futures::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut parser = SseParser::default();
        let mut tr = translate::AnthropicToOpenAiStream::new();
        let mut stream = resp.bytes_stream();
        let mut errored = false;
        'pump: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    let err = tr.error_frame(&format!("upstream stream interrupted: {e}"));
                    let _ = tx
                        .send(Ok(bytes::Bytes::from(format!("data: {err}\n\n"))))
                        .await;
                    errored = true;
                    break;
                }
            };
            for ev in parser.feed(&chunk) {
                let name = ev.event.as_deref().unwrap_or("");
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    for c in tr.feed(name, &v) {
                        let line = bytes::Bytes::from(format!("data: {c}\n\n"));
                        if tx.send(Ok(line)).await.is_err() {
                            // Client disconnected; stop pumping but still
                            // record what we saw so far.
                            break 'pump;
                        }
                    }
                }
            }
        }
        if !errored && tr.finish() {
            let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
        }
        let (input, output) = tr.usage();
        crate::llm_router::usage::record(
            &store,
            &ctx.conn_id,
            &ctx.provider,
            &ctx.model,
            &ctx.client_format,
            crate::llm_router::usage::Usage { input, output },
            if errored { 502 } else { 200 },
            ctx.started,
            errored.then(|| "stream interrupted".to_string()),
        );
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Client=Anthropic, upstream=OpenAI, stream=true.
async fn stream_openai_upstream_to_anthropic(
    state: &AppState,
    target: &RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Response {
    let model = upstream_body["model"].as_str().unwrap_or("").to_string();
    let req = match upstream_request(state, target, upstream_body) {
        Ok(r) => r,
        Err(e) => return anthropic_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return anthropic_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !resp.status().is_success() {
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let v: Value = resp.json().await.unwrap_or(json!({}));
        let msg = v["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        return anthropic_error(status, &format!("[{}] {msg}", target.conn.provider));
    }
    sse_response(spawn_openai_to_anthropic_pump(
        resp,
        model,
        state.store.clone(),
        ctx,
    ))
}

/// Client=OpenAI, upstream=Anthropic, stream=true.
async fn stream_anthropic_upstream_to_openai(
    state: &AppState,
    target: &RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Response {
    let req = match upstream_request(state, target, upstream_body) {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !resp.status().is_success() {
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let v: Value = resp.json().await.unwrap_or(json!({}));
        let msg = v["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        return openai_error(status, &format!("[{}] {msg}", target.conn.provider));
    }
    sse_response(spawn_anthropic_to_openai_pump(
        resp,
        state.store.clone(),
        ctx,
    ))
}

/// Client=Responses, stream=true. Normalizes the upstream response (OpenAI or
/// Anthropic format) to OpenAI chat chunks, then encodes Responses SSE.
async fn stream_responses(
    state: &AppState,
    target: &RouteTarget,
    upstream_body: &Value,
    started: i64,
) -> Response {
    let req = match upstream_request(state, target, upstream_body) {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !resp.status().is_success() {
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let v: Value = resp.json().await.unwrap_or(json!({}));
        let msg = v["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        return openai_error(status, &format!("[{}] {msg}", target.conn.provider));
    }
    let anthropic_upstream = matches!(target.desc.format, ApiFormat::Anthropic);
    let store = state.store.clone();
    let ctx = RecordCtx {
        conn_id: target.conn.id.clone(),
        provider: target.conn.provider.clone(),
        model: target.upstream_model.clone(),
        client_format: "responses".into(),
        started,
    };
    sse_response(spawn_responses_pump(resp, anthropic_upstream, store, ctx))
}

/// Pumps an upstream SSE response through Responses encoding. For an
/// Anthropic-format upstream, the Anthropic events are first normalized to
/// OpenAI chat chunks via `AnthropicToOpenAiStream`, then fed into
/// `ResponsesStreamState`; an OpenAI-format upstream's chunks are fed
/// directly.
fn spawn_responses_pump(
    resp: reqwest::Response,
    anthropic_upstream: bool,
    store: Arc<Store>,
    ctx: RecordCtx,
) -> Body {
    use futures::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut parser = SseParser::default();
        let mut anth = translate::AnthropicToOpenAiStream::new();
        let mut rs = crate::llm_router::responses::ResponsesStreamState::new();
        let mut stream = resp.bytes_stream();
        let mut errored = false;
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    let (name, data) = rs.error_frame(&format!("upstream stream interrupted: {e}"));
                    let _ = tx.send(Ok(format_sse(&name, &data))).await;
                    errored = true;
                    break;
                }
            };
            for ev in parser.feed(&chunk) {
                // Get OpenAI chunk(s): direct for openai upstream, translated for anthropic.
                let oai_chunks: Vec<Value> = if anthropic_upstream {
                    let name = ev.event.as_deref().unwrap_or("");
                    serde_json::from_str::<Value>(&ev.data)
                        .ok()
                        .map(|v| anth.feed(name, &v))
                        .unwrap_or_default()
                } else if ev.data == "[DONE]" {
                    Vec::new()
                } else {
                    serde_json::from_str::<Value>(&ev.data)
                        .ok()
                        .into_iter()
                        .collect()
                };
                for oai in oai_chunks {
                    for (name, data) in rs.feed(&oai) {
                        if tx.send(Ok(format_sse(&name, &data))).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        if !errored {
            for (name, data) in rs.finish() {
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
            }
        }
        // usage: for openai upstream we didn't accumulate; record zero-token row
        // with status (F2a keeps Responses usage best-effort — the completed
        // event's usage plumbing is a follow-up).
        crate::llm_router::usage::record(
            &store,
            &ctx.conn_id,
            &ctx.provider,
            &ctx.model,
            &ctx.client_format,
            crate::llm_router::usage::Usage::default(),
            if errored { 502 } else { 200 },
            ctx.started,
            errored.then(|| "stream interrupted".to_string()),
        );
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}
