//! The local endpoint server: Anthropic + OpenAI compatible surface on
//! 127.0.0.1, gated by endpoint keys, routed to provider connections.
use crate::llm_router::client::{route_model_for_body, send_upstream, RouteTarget, UpstreamCtx};
use crate::llm_router::registry::{self, ApiFormat};
use crate::llm_router::{connections, keys, oauth, routes, sse::SseParser, translate};
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
    oauth_token_url_override: Mutex<Option<String>>,
    kiro_base_override: Mutex<Option<String>>,
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    http: reqwest::Client,
    /// Test-only override for the OAuth token endpoint used by the reactive
    /// (post-401) refresh path. `None` in production, which uses each
    /// provider's static `registry::oauth_config` token_url.
    oauth_token_url_override: Option<String>,
    /// Test-only override for the kiro `generateAssistantResponse` endpoint
    /// (kiro's URL is hard-coded, unlike the other providers' base URLs,
    /// which already have a per-connection `base_url_override`). `None` in
    /// production, which uses `kiro_endpoints(..)[0]`.
    kiro_base_override: Option<String>,
}

impl AppState {
    /// The axum-free upstream context, for calling into
    /// [`crate::llm_router::client`]. Cheap: an `Arc` clone, a reference-counted
    /// `reqwest::Client` clone, and a small `Option<String>` clone.
    fn ctx(&self) -> UpstreamCtx {
        UpstreamCtx {
            store: self.store.clone(),
            http: self.http.clone(),
            oauth_token_url_override: self.oauth_token_url_override.clone(),
        }
    }
}

impl RouterServer {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            inner: Mutex::new(Inner {
                shutdown: None,
                port: 0,
            }),
            oauth_token_url_override: Mutex::new(None),
            kiro_base_override: Mutex::new(None),
        }
    }

    /// Test-only seam: point the reactive (post-401) OAuth refresh path at a
    /// mock token endpoint instead of the real, static registry URL. Must be
    /// called before [`start`](Self::start) — it's read once when the server
    /// starts. Never set this in production code; `None` (the default) uses
    /// each provider's real token endpoint.
    #[doc(hidden)]
    pub fn set_oauth_token_url_override(&self, url: Option<String>) {
        *self.oauth_token_url_override.lock().unwrap() = url;
    }

    /// Test-only seam: point kiro's upstream request at a mock endpoint
    /// instead of the real kiro.dev/CodeWhisperer URL. Must be called before
    /// [`start`](Self::start) — it's read once when the server starts. Never
    /// set this in production code; `None` (the default) uses
    /// `kiro_endpoints(..)[0]`.
    #[doc(hidden)]
    pub fn set_kiro_base_override(&self, url: Option<String>) {
        *self.kiro_base_override.lock().unwrap() = url;
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
            oauth_token_url_override: self.oauth_token_url_override.lock().unwrap().clone(),
            kiro_base_override: self.kiro_base_override.lock().unwrap().clone(),
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
// Codex Responses body normalization
// ---------------------------------------------------------------------------

const CODEX_DEFAULT_INSTRUCTIONS: &str =
    "You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer.";
const CODEX_ALLOWED_RESPONSE_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "stream",
    "store",
    "reasoning",
    "service_tier",
    "include",
    "prompt_cache_key",
    "client_metadata",
    "text",
];

fn normalize_responses_input(input: Value) -> Value {
    match input {
        Value::String(s) => {
            let text = if s.is_empty() { "..." } else { s.as_str() };
            json!([{"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": text}]}])
        }
        Value::Array(items) if !items.is_empty() => Value::Array(items),
        _ => json!([{"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "..."}]}]),
    }
}

fn convert_codex_system_items_to_developer(body: &mut Value) {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let is_system = item
            .get("role")
            .and_then(Value::as_str)
            .map(|r| r == "system")
            .unwrap_or(false)
            && item
                .get("type")
                .and_then(Value::as_str)
                .map(|t| t == "message")
                .unwrap_or(true);
        if is_system {
            item["role"] = json!("developer");
        }
    }
}

fn strip_codex_stored_item_refs(body: &mut Value) {
    fn is_server_id(s: &str) -> bool {
        ["rs_", "fc_", "resp_", "msg_"]
            .iter()
            .any(|prefix| s.starts_with(prefix))
    }

    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    items.retain_mut(|item| {
        if item.as_str().map(is_server_id).unwrap_or(false) {
            return false;
        }
        if item
            .get("type")
            .and_then(Value::as_str)
            .map(|t| t == "item_reference")
            .unwrap_or(false)
        {
            return false;
        }
        if item
            .get("id")
            .and_then(Value::as_str)
            .map(is_server_id)
            .unwrap_or(false)
        {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("id");
            }
        }
        true
    });
}

fn normalize_codex_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    tools.retain_mut(|tool| {
        let Some(obj) = tool.as_object_mut() else {
            return false;
        };
        let tool_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if tool_type != "function" {
            return matches!(
                tool_type,
                "custom"
                    | "image_generation"
                    | "web_search"
                    | "web_search_preview"
                    | "file_search"
                    | "computer"
                    | "computer_use_preview"
                    | "code_interpreter"
                    | "mcp"
                    | "local_shell"
                    | "tool_search"
            );
        }

        if obj.contains_key("function") {
            let Some(function) = obj.get("function").and_then(Value::as_object) else {
                return false;
            };
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                return false;
            };
            let name = name.trim().chars().take(128).collect::<String>();
            if name.is_empty() {
                return false;
            }
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            obj.clear();
            obj.insert("type".into(), json!("function"));
            obj.insert("name".into(), json!(name));
            if !description.is_empty() {
                obj.insert("description".into(), json!(description));
            }
            obj.insert("parameters".into(), parameters);
        }
        true
    });
}

fn codex_virtual_model_to_upstream(model: &str) -> (String, Option<&'static str>) {
    let mut upstream = model.strip_suffix("-review").unwrap_or(model).to_string();
    for effort in ["xhigh", "high", "medium", "low", "none"] {
        let suffix = format!("-{effort}");
        if upstream.ends_with(&suffix) {
            let new_len = upstream.len() - suffix.len();
            upstream.truncate(new_len);
            return (upstream, Some(effort));
        }
    }
    (upstream, None)
}

fn normalize_codex_responses_body(body: &mut Value, upstream_model: &str, cache_key: Option<&str>) {
    let (model, model_effort) = codex_virtual_model_to_upstream(upstream_model);
    body["model"] = json!(model);
    let input = body.get("input").cloned().unwrap_or(Value::Null);
    body["input"] = normalize_responses_input(input);
    convert_codex_system_items_to_developer(body);
    strip_codex_stored_item_refs(body);
    normalize_codex_tools(body);

    body["stream"] = json!(true);
    body["store"] = json!(false);
    if body
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        body["instructions"] = json!(CODEX_DEFAULT_INSTRUCTIONS);
    }
    if body.get("prompt_cache_key").is_none() {
        if let Some(key) = cache_key {
            body["prompt_cache_key"] = json!(key);
        }
    }

    if body.get("reasoning").is_none() {
        let effort = body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(model_effort)
            .unwrap_or("low");
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    } else if body
        .get("reasoning")
        .and_then(|r| r.get("summary"))
        .is_none()
    {
        body["reasoning"]["summary"] = json!("auto");
    }
    if body
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        .map(|e| e != "none")
        .unwrap_or(false)
    {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }

    for key in [
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "logprobs",
        "top_logprobs",
        "n",
        "seed",
        "max_tokens",
        "max_completion_tokens",
        "max_output_tokens",
        "user",
        "prompt_cache_retention",
        "metadata",
        "stream_options",
        "safety_identifier",
        "previous_response_id",
        "reasoning_effort",
    ] {
        if let Some(obj) = body.as_object_mut() {
            obj.remove(key);
        }
    }
    if let Some(obj) = body.as_object_mut() {
        obj.retain(|k, _| CODEX_ALLOWED_RESPONSE_FIELDS.contains(&k.as_str()));
    }
}

// ---------------------------------------------------------------------------
// Handlers
//
// Model routing, the Claude-Code system-prompt injection, upstream request
// construction (api-key / OAuth / free), and the 401/403 refresh-and-retry
// `send_upstream` now live in `crate::llm_router::client` so the native agent
// runtime can share them in-process. This module keeps the axum handlers,
// auth, error shaping, and SSE byte pumps.
// ---------------------------------------------------------------------------

/// openai-oauth (Codex) speaks the Responses wire only — no chat→Responses
/// request translator exists in F2b (deferred to F3 if ever needed), so a
/// client routing it through `/v1/messages` or `/v1/chat/completions` gets
/// pointed at the route that actually works.
const OPENAI_OAUTH_WRONG_ROUTE_MSG: &str =
    "the OpenAI (ChatGPT) connection speaks the Responses API — point your tool at /v1/responses";

fn reconnect_message(provider: &str) -> String {
    format!(
        "the {provider} connection needs to be reconnected — its login session has expired; reconnect it from Models → Providers"
    )
}

/// Refresh `target.conn`'s OAuth token before it's used, proactively. A
/// terminal refresh failure (no refresh token, or the provider rejected it
/// for good) sets `needs_relogin`; surface that as a client-format auth
/// error instead of proceeding with a token we know is dead. Any other
/// refresh failure (e.g. a transient network hiccup) is swallowed — the
/// caller proceeds with whatever token it has, and the reactive 401 path in
/// `send_upstream` gets a second chance at it.
async fn ensure_fresh_or_reconnect_error(
    state: &AppState,
    target: &mut RouteTarget,
    err: fn(StatusCode, &str) -> Response,
) -> Option<Response> {
    if oauth::refresh::ensure_fresh(&state.store, &state.http, &mut target.conn)
        .await
        .is_err()
        && target.conn.data.needs_relogin == Some(true)
    {
        return Some(err(
            StatusCode::UNAUTHORIZED,
            &reconnect_message(&target.conn.provider),
        ));
    }
    None
}

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
    let mut target = match route_model_for_body(&state.store, &requested, Some(&body)).await {
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
    if target.conn.provider == "kiro" {
        return serve_kiro(&state, target, ClientFormat::Anthropic, &body).await;
    }
    if target.conn.provider == "openai-oauth" {
        return anthropic_error(StatusCode::BAD_REQUEST, OPENAI_OAUTH_WRONG_ROUTE_MSG);
    }
    if let Some(r) = ensure_fresh_or_reconnect_error(&state, &mut target, anthropic_error).await {
        return r;
    }
    let stream = body["stream"].as_bool().unwrap_or(false);
    body["model"] = json!(target.upstream_model);

    match target.desc.format {
        ApiFormat::Anthropic => {
            let started = crate::paths::now_ms();
            let resp = proxy_passthrough(&state, &mut target, &body, anthropic_error).await;
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
                stream_openai_upstream_to_anthropic(&state, &mut target, &upstream_body, ctx).await
            } else {
                let started = crate::paths::now_ms();
                match send_json(&state, &mut target, &upstream_body, anthropic_error).await {
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
    let mut target = match route_model_for_body(&state.store, &requested, Some(&body)).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return openai_error(
                StatusCode::NOT_FOUND,
                &format!("no enabled connection serves model '{requested}'"),
            )
        }
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if target.conn.provider == "kiro" {
        return serve_kiro(&state, target, ClientFormat::OpenAi, &body).await;
    }
    if target.conn.provider == "openai-oauth" {
        return openai_error(StatusCode::BAD_REQUEST, OPENAI_OAUTH_WRONG_ROUTE_MSG);
    }
    if let Some(r) = ensure_fresh_or_reconnect_error(&state, &mut target, openai_error).await {
        return r;
    }
    let stream = body["stream"].as_bool().unwrap_or(false);
    body["model"] = json!(target.upstream_model);

    match target.desc.format {
        ApiFormat::OpenAi => {
            let started = crate::paths::now_ms();
            let resp = proxy_passthrough(&state, &mut target, &body, openai_error).await;
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
                stream_anthropic_upstream_to_openai(&state, &mut target, &upstream_body, ctx).await
            } else {
                let started = crate::paths::now_ms();
                match send_json(&state, &mut target, &upstream_body, openai_error).await {
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
    let mut target = match route_model_for_body(&state.store, &requested, Some(&body)).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return openai_error(
                StatusCode::NOT_FOUND,
                &format!("no enabled connection serves model '{requested}'"),
            )
        }
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if target.conn.provider == "kiro" {
        return serve_kiro(&state, target, ClientFormat::Responses, &body).await;
    }
    if let Some(r) = ensure_fresh_or_reconnect_error(&state, &mut target, openai_error).await {
        return r;
    }

    // Codex (openai-oauth) speaks the Responses wire natively both ways — no
    // chat translation applies (that's only built for routing/model
    // resolution above). Normalize the client's Responses body to the subset
    // accepted by ChatGPT's Codex backend, then proxy it same-format.
    if target.conn.provider == "openai-oauth" {
        let mut passthrough_body = body.clone();
        normalize_codex_responses_body(&mut passthrough_body, &target.upstream_model, None);
        let started = crate::paths::now_ms();
        let resp = proxy_passthrough(&state, &mut target, &passthrough_body, openai_error).await;
        crate::llm_router::usage::record(
            &state.store,
            &target.conn.id,
            &target.conn.provider,
            &target.upstream_model,
            "responses",
            crate::llm_router::usage::Usage::default(),
            resp.status().as_u16(),
            started,
            None,
        );
        return resp;
    }

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
        stream_responses(&state, &mut target, &upstream_body, started).await
    } else {
        match send_json(&state, &mut target, &upstream_body, openai_error).await {
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
    let routes = match routes::list_model_routes(&state.store).await {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    for route in routes.iter().filter(|r| r.enabled && !r.targets.is_empty()) {
        if seen.insert(route.name.clone()) {
            data.push(json!({"id": route.name, "object": "model", "owned_by": "route"}));
        }
    }
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
    target: &mut RouteTarget,
    body: &Value,
    err: fn(StatusCode, &str) -> Response,
) -> Result<Value, Response> {
    let resp = send_upstream(&state.ctx(), target, body)
        .await
        .map_err(|e| {
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
    target: &mut RouteTarget,
    body: &Value,
    err: fn(StatusCode, &str) -> Response,
) -> Response {
    let resp = match send_upstream(&state.ctx(), target, body).await {
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
            if tr.saw_terminal() {
                for (name, data) in tr.finish() {
                    let _ = tx.send(Ok(format_sse(&name, &data))).await;
                }
            } else {
                // Upstream closed cleanly but never sent a terminal chunk —
                // that's a truncated stream, not a completed one. Don't fake
                // a clean finish.
                for (name, data) in tr.error_frame("upstream stream ended without a terminal event")
                {
                    let _ = tx.send(Ok(format_sse(&name, &data))).await;
                }
                errored = true;
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
    target: &mut RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Response {
    let model = upstream_body["model"].as_str().unwrap_or("").to_string();
    let resp = match send_upstream(&state.ctx(), target, upstream_body).await {
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
    target: &mut RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Response {
    let resp = match send_upstream(&state.ctx(), target, upstream_body).await {
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
    target: &mut RouteTarget,
    upstream_body: &Value,
    started: i64,
) -> Response {
    let resp = match send_upstream(&state.ctx(), target, upstream_body).await {
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
        'pump: while let Some(item) = stream.next().await {
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
                            // Client disconnected; stop pumping but still
                            // record what we saw so far.
                            break 'pump;
                        }
                    }
                }
            }
        }
        if !errored {
            if rs.saw_terminal() {
                for (name, data) in rs.finish() {
                    let _ = tx.send(Ok(format_sse(&name, &data))).await;
                }
            } else {
                // Upstream closed cleanly but never sent a terminal chunk —
                // that's a truncated stream, not a completed one. Don't fake
                // a clean finish.
                let (name, data) = rs.error_frame("upstream stream ended without a terminal event");
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
                errored = true;
            }
        }
        // usage: for an Anthropic upstream, tokens come from the translated
        // AnthropicToOpenAiStream; for a native OpenAI upstream, from the
        // Responses encoder's own accumulation of each chunk's `usage` field
        // (present when the upstream provider sends a final usage frame —
        // best-effort, since the Responses request doesn't yet ask for
        // `stream_options.include_usage` the way the Anthropic-client path
        // does in `anthropic_to_openai_request`).
        let usage = if anthropic_upstream {
            let (input, output) = anth.usage();
            crate::llm_router::usage::Usage { input, output }
        } else {
            let (input, output) = rs.usage();
            crate::llm_router::usage::Usage { input, output }
        };
        crate::llm_router::usage::record(
            &store,
            &ctx.conn_id,
            &ctx.provider,
            &ctx.model,
            &ctx.client_format,
            usage,
            if errored { 502 } else { 200 },
            ctx.started,
            errored.then(|| "stream interrupted".to_string()),
        );
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// ---------------------------------------------------------------------------
// Kiro (CodeWhisperer) native routing
// ---------------------------------------------------------------------------

/// Which wire format the calling client speaks — kiro serves all three off
/// the same upstream, so this picks the request translator and the
/// streaming/non-stream encoder on the way back out.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ClientFormat {
    Anthropic,
    OpenAi,
    Responses,
}

/// Ordered kiro upstream endpoints for `generateAssistantResponse`. MVP only
/// ever sends to `[0]` (no fallback loop yet — a future retry across `[1]`,
/// `[2]` on failure can reuse this ordering without change). Account-bound
/// auth (api_key/idc/external_idp) moves the two `amazonaws.com` hosts first
/// because kiro.dev rejects an account-bound bearer token.
fn kiro_endpoints(auth_method: &str, region: &str) -> Vec<String> {
    let kiro_dev = "https://runtime.us-east-1.kiro.dev/generateAssistantResponse".to_string();
    let codewhisperer =
        format!("https://codewhisperer.{region}.amazonaws.com/generateAssistantResponse");
    let q = format!("https://q.{region}.amazonaws.com/generateAssistantResponse");
    if connections::is_account_bound(auth_method) {
        vec![codewhisperer, q, kiro_dev]
    } else {
        vec![kiro_dev, codewhisperer, q]
    }
}

/// Verbatim kiro (CodeWhisperer) upstream request — no fingerprint/cloaking
/// headers (spec §2): just the wire's own contract (AWS event-stream
/// accept, the `x-amz-target` operation name, a static SDK user-agent pair,
/// an invocation id, and the bearer token). `state.kiro_base_override`
/// (test-only) replaces the endpoint verbatim when set; production always
/// resolves `kiro_endpoints(..)[0]`.
fn kiro_upstream_request(
    state: &AppState,
    target: &RouteTarget,
    kiro_body: &Value,
) -> reqwest::RequestBuilder {
    let data = &target.conn.data;
    let auth_method = connections::kiro_auth_method(data);
    let url = match &state.kiro_base_override {
        Some(u) => u.clone(),
        None => kiro_endpoints(&auth_method, &connections::kiro_region(data))
            .into_iter()
            .next()
            .expect("kiro_endpoints always returns at least one URL"),
    };
    let token = data.access_token.clone().unwrap_or_default();
    let mut req = state
        .http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/vnd.amazon.eventstream")
        .header(
            "x-amz-target",
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse",
        )
        .header("user-agent", "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0")
        .header("x-amz-user-agent", "aws-sdk-js/3.0.0 kiro-ide/1.0.0")
        .header("amz-sdk-request", "attempt=1; max=3")
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("authorization", format!("Bearer {token}"));
    if auth_method == "api_key" {
        req = req.header("tokentype", "API_KEY");
    } else if auth_method == "external_idp" {
        req = req.header("TokenType", "EXTERNAL_IDP");
    }
    req.json(kiro_body)
}

/// Send the kiro upstream request; on a 401/403, refresh once via
/// `force_refresh` (kiro connections are oauth, so this dispatches straight
/// to the kiro refresh branch — AWS SSO-OIDC or the social endpoint,
/// whichever the connection's auth method calls for) and retry the same
/// request. Mirrors `send_upstream`'s retry shape exactly: the retry is kept
/// only if the refresh itself succeeds, and a 401 arriving mid-stream is NOT
/// retried (this is only called pre-stream, before any response bytes are
/// read).
async fn send_kiro(
    state: &AppState,
    target: &mut RouteTarget,
    kiro_body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = kiro_upstream_request(state, target, kiro_body)
        .send()
        .await?;
    if matches!(resp.status().as_u16(), 401 | 403) {
        let refreshed = match &state.oauth_token_url_override {
            Some(token_url) => oauth::refresh::force_refresh_with_token_url(
                &state.store,
                &state.http,
                &mut target.conn,
                token_url,
            )
            .await
            .is_ok(),
            None => oauth::refresh::force_refresh(&state.store, &state.http, &mut target.conn)
                .await
                .is_ok(),
        };
        if refreshed {
            return Ok(kiro_upstream_request(state, target, kiro_body)
                .send()
                .await?);
        }
    }
    Ok(resp)
}

/// Translate the client's own wire-format body into kiro's
/// `generateAssistantResponse` payload. Anthropic and OpenAI clients each
/// have a direct translator; a Responses client has none of its own, so its
/// body is first normalized to the internal chat shape via
/// `responses::responses_request_to_chat`, then routed through the same
/// OpenAI translator.
fn kiro_request_body(
    body: &Value,
    client_format: ClientFormat,
    model: &str,
    data: &connections::ConnectionData,
    conversation_id: &str,
) -> Value {
    match client_format {
        ClientFormat::Anthropic => {
            crate::llm_router::kiro::anthropic_request_to_kiro(body, model, data, conversation_id)
        }
        ClientFormat::OpenAi => {
            crate::llm_router::kiro::openai_request_to_kiro(body, model, data, conversation_id)
        }
        ClientFormat::Responses => {
            let chat = crate::llm_router::responses::responses_request_to_chat(body);
            crate::llm_router::kiro::openai_request_to_kiro(&chat, model, data, conversation_id)
        }
    }
}

/// Forward one OpenAI chunk (from `KiroToOpenAiStream`) down `tx`, re-encoded
/// per `client_format`: an OpenAI client gets it verbatim as an SSE `data:`
/// line; Anthropic/Responses clients get it translated through their own
/// streaming encoder — the same encoders the other pumps use, so the
/// per-event shape matches exactly. `Err(())` means the client disconnected
/// (send failed); the caller stops pumping but keeps recording.
async fn forward_openai_chunk(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    client_format: ClientFormat,
    oai: &Value,
    anth: &mut translate::OpenAiToAnthropicStream,
    rs: &mut crate::llm_router::responses::ResponsesStreamState,
) -> Result<(), ()> {
    match client_format {
        ClientFormat::OpenAi => {
            let line = bytes::Bytes::from(format!("data: {oai}\n\n"));
            tx.send(Ok(line)).await.map_err(|_| ())
        }
        ClientFormat::Anthropic => {
            for (name, data) in anth.feed(oai) {
                if tx.send(Ok(format_sse(&name, &data))).await.is_err() {
                    return Err(());
                }
            }
            Ok(())
        }
        ClientFormat::Responses => {
            for (name, data) in rs.feed(oai) {
                if tx.send(Ok(format_sse(&name, &data))).await.is_err() {
                    return Err(());
                }
            }
            Ok(())
        }
    }
}

/// Close out a clean stream (kiro sent its `messageStopEvent` terminal
/// frame): OpenAI gets `data: [DONE]`; Anthropic/Responses get their own
/// encoder's `finish()` (message_stop / response.completed).
async fn emit_kiro_finish(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    client_format: ClientFormat,
    anth: &mut translate::OpenAiToAnthropicStream,
    rs: &mut crate::llm_router::responses::ResponsesStreamState,
) {
    match client_format {
        ClientFormat::OpenAi => {
            let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
        }
        ClientFormat::Anthropic => {
            for (name, data) in anth.finish() {
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
            }
        }
        ClientFormat::Responses => {
            for (name, data) in rs.finish() {
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
            }
        }
    }
}

/// Terminal error frame in the client's own format — used both for a
/// mid-stream transport error and for a clean EOF that never sent a
/// `messageStopEvent` (a truncated stream, not a completed one; do not
/// follow it with `emit_kiro_finish`).
async fn emit_kiro_error(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    client_format: ClientFormat,
    anth: &mut translate::OpenAiToAnthropicStream,
    rs: &mut crate::llm_router::responses::ResponsesStreamState,
    kiro: &crate::llm_router::kiro::KiroToOpenAiStream,
    message: &str,
) {
    match client_format {
        ClientFormat::OpenAi => {
            let err = kiro.error_frame(message);
            let _ = tx
                .send(Ok(bytes::Bytes::from(format!("data: {err}\n\n"))))
                .await;
        }
        ClientFormat::Anthropic => {
            for (name, data) in anth.error_frame(message) {
                let _ = tx.send(Ok(format_sse(&name, &data))).await;
            }
        }
        ClientFormat::Responses => {
            let (name, data) = rs.error_frame(message);
            let _ = tx.send(Ok(format_sse(&name, &data))).await;
        }
    }
}

/// Pump a kiro upstream response (AWS event-stream framing) into the
/// client's own streaming format. One `AwsEventStreamParser` +
/// `KiroToOpenAiStream` decodes the wire; the resulting OpenAI chunks are
/// re-encoded per `client_format` and sent down the channel backing the
/// response body — reusing the exact terminal/error contract
/// `spawn_anthropic_to_openai_pump`/`spawn_responses_pump` already have.
fn spawn_kiro_pump(
    resp: reqwest::Response,
    client_format: ClientFormat,
    model: String,
    store: Arc<Store>,
    ctx: RecordCtx,
) -> Body {
    use futures::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut parser = crate::llm_router::aws_stream::AwsEventStreamParser::default();
        let mut kiro = crate::llm_router::kiro::KiroToOpenAiStream::new(&model);
        let mut anth = translate::OpenAiToAnthropicStream::new(&model);
        let mut rs = crate::llm_router::responses::ResponsesStreamState::new();
        let mut stream = resp.bytes_stream();
        let mut errored = false;
        'pump: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    emit_kiro_error(
                        &tx,
                        client_format,
                        &mut anth,
                        &mut rs,
                        &kiro,
                        &format!("upstream stream interrupted: {e}"),
                    )
                    .await;
                    errored = true;
                    break;
                }
            };
            for frame in parser.feed(&chunk) {
                for oai in kiro.feed(&frame) {
                    if forward_openai_chunk(&tx, client_format, &oai, &mut anth, &mut rs)
                        .await
                        .is_err()
                    {
                        // Client disconnected; stop pumping but still record
                        // what we saw so far.
                        break 'pump;
                    }
                }
            }
        }
        if !errored {
            if kiro.saw_terminal() {
                emit_kiro_finish(&tx, client_format, &mut anth, &mut rs).await;
            } else {
                // Upstream closed cleanly but never sent a messageStopEvent —
                // that's a truncated stream, not a completed one.
                errored = true;
                emit_kiro_error(
                    &tx,
                    client_format,
                    &mut anth,
                    &mut rs,
                    &kiro,
                    "upstream stream ended without a terminal event",
                )
                .await;
            }
        }
        let (input, output) = kiro.usage();
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

/// Fold a fully-drained sequence of `KiroToOpenAiStream` chunks (through its
/// terminal chunk) into a single non-stream `chat.completion` value — kiro's
/// wire has no non-stream mode of its own, so a client that asked for one
/// still needs its content/tool_calls/finish_reason assembled from the same
/// chunks the streaming path would have sent one at a time.
fn aggregate_openai_chunks(chunks: &[Value], model: &str, input: i64, output: i64) -> Value {
    let id = chunks
        .first()
        .and_then(|c| c["id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4()));
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = "stop".to_string();
    for c in chunks {
        let delta = &c["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str() {
            content.push_str(t);
        }
        for tc in delta["tool_calls"].as_array().cloned().unwrap_or_default() {
            let idx = tc["index"].as_u64().unwrap_or(0) as usize;
            while tool_calls.len() <= idx {
                tool_calls.push(json!({
                    "id": "", "type": "function",
                    "function": {"name": "", "arguments": ""}
                }));
            }
            if let Some(tc_id) = tc["id"].as_str() {
                tool_calls[idx]["id"] = json!(tc_id);
            }
            if let Some(name) = tc["function"]["name"].as_str() {
                tool_calls[idx]["function"]["name"] = json!(name);
            }
            if let Some(args) = tc["function"]["arguments"].as_str() {
                let existing = tool_calls[idx]["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                tool_calls[idx]["function"]["arguments"] = json!(format!("{existing}{args}"));
            }
        }
        if let Some(fr) = c["choices"][0]["finish_reason"].as_str() {
            finish_reason = fr.to_string();
        }
    }
    let mut message = json!({"role": "assistant", "content": content});
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    json!({
        "id": id, "object": "chat.completion", "model": model,
        "created": crate::paths::now_ms() / 1000,
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason, "logprobs": null}],
        "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output},
    })
}

/// Non-stream kiro response: buffer the whole (always-streaming) kiro
/// upstream body, decode it through the same `AwsEventStreamParser` +
/// `KiroToOpenAiStream` pipeline as the streaming path, then fold the
/// resulting chunks into one non-stream value re-encoded in the client's own
/// format (mirrors `openai_to_anthropic_response`/`chat_response_to_responses`,
/// which already do this same OpenAI-chat -> client-format step for the
/// other providers' non-stream replies).
async fn kiro_non_stream_response(
    resp: reqwest::Response,
    client_format: ClientFormat,
    model: &str,
    err: fn(StatusCode, &str) -> Response,
) -> Result<(Value, crate::llm_router::usage::Usage), Response> {
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, &format!("[kiro] {e}")))?;
    let mut parser = crate::llm_router::aws_stream::AwsEventStreamParser::default();
    let mut kiro = crate::llm_router::kiro::KiroToOpenAiStream::new(model);
    let mut chunks: Vec<Value> = Vec::new();
    for frame in parser.feed(&bytes) {
        chunks.extend(kiro.feed(&frame));
    }
    if !kiro.saw_terminal() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            "[kiro] upstream stream ended without a terminal event",
        ));
    }
    let (input, output) = kiro.usage();
    let agg = aggregate_openai_chunks(&chunks, model, input, output);
    let out = match client_format {
        ClientFormat::OpenAi => agg,
        ClientFormat::Anthropic => translate::openai_to_anthropic_response(&agg),
        ClientFormat::Responses => crate::llm_router::responses::chat_response_to_responses(&agg),
    };
    Ok((out, crate::llm_router::usage::Usage { input, output }))
}

/// Route a kiro (CodeWhisperer free-tier) connection natively: translate the
/// client's own wire-format body to Kiro's `generateAssistantResponse`
/// payload, send it with the verbatim kiro headers (refreshing once on a
/// 401/403 like `send_upstream`), and pump the AWS event-stream response
/// back out re-encoded in the client's own format (or aggregate it into one
/// JSON body for a non-stream request). Kiro serves all three client formats
/// off the same upstream, so — unlike `openai-oauth` — there's no
/// wrong-route guard here.
async fn serve_kiro(
    state: &AppState,
    mut target: RouteTarget,
    client_format: ClientFormat,
    body: &Value,
) -> Response {
    let err_fn: fn(StatusCode, &str) -> Response = match client_format {
        ClientFormat::Anthropic => anthropic_error,
        ClientFormat::OpenAi | ClientFormat::Responses => openai_error,
    };
    if let Some(r) = ensure_fresh_or_reconnect_error(state, &mut target, err_fn).await {
        return r;
    }

    let model = target.upstream_model.clone();
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let kiro_body = kiro_request_body(
        body,
        client_format,
        &model,
        &target.conn.data,
        &conversation_id,
    );

    let stream = body["stream"].as_bool().unwrap_or(false);
    let started = crate::paths::now_ms();
    let resp = match send_kiro(state, &mut target, &kiro_body).await {
        Ok(r) => r,
        Err(e) => return err_fn(StatusCode::BAD_GATEWAY, &format!("upstream kiro: {e}")),
    };
    if !resp.status().is_success() {
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let body_text = resp
            .text()
            .await
            .unwrap_or_else(|_| "upstream error".to_string());
        return err_fn(status, &format!("[kiro] {body_text}"));
    }

    let ctx = RecordCtx {
        conn_id: target.conn.id.clone(),
        provider: target.conn.provider.clone(),
        model: model.clone(),
        client_format: match client_format {
            ClientFormat::Anthropic => "anthropic",
            ClientFormat::OpenAi => "openai",
            ClientFormat::Responses => "responses",
        }
        .to_string(),
        started,
    };

    if stream {
        sse_response(spawn_kiro_pump(
            resp,
            client_format,
            model,
            state.store.clone(),
            ctx,
        ))
    } else {
        match kiro_non_stream_response(resp, client_format, &model, err_fn).await {
            Ok((v, usage)) => {
                crate::llm_router::usage::record(
                    &state.store,
                    &ctx.conn_id,
                    &ctx.provider,
                    &ctx.model,
                    &ctx.client_format,
                    usage,
                    200,
                    ctx.started,
                    None,
                );
                Json(v).into_response()
            }
            Err(r) => {
                crate::llm_router::usage::record(
                    &state.store,
                    &ctx.conn_id,
                    &ctx.provider,
                    &ctx.model,
                    &ctx.client_format,
                    crate::llm_router::usage::Usage::default(),
                    502,
                    ctx.started,
                    Some("stream interrupted".to_string()),
                );
                r
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::client::route_model;
    use crate::llm_router::connections::{ConnectionData, ConnectionRow};

    fn mk_conn(id: &str, provider: &str, auth_type: &str, data: ConnectionData) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: auth_type.into(),
            label: "t".into(),
            priority: 0,
            enabled: true,
            data,
            created_at: 0,
            updated_at: 0,
        }
    }

    async fn test_state() -> AppState {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        AppState {
            store,
            http: reqwest::Client::new(),
            oauth_token_url_override: None,
            kiro_base_override: None,
        }
    }

    #[test]
    fn codex_oauth_responses_body_is_normalized_for_backend() {
        let mut body = json!({
            "model": "openai-oauth/gpt-5.3-codex-high",
            "input": "hi",
            "temperature": 0.2,
            "max_output_tokens": 128,
            "previous_response_id": "resp_old",
            "stream": false
        });

        normalize_codex_responses_body(&mut body, "gpt-5.3-codex-high", Some("session-1"));

        assert_eq!(body["model"], "gpt-5.3-codex");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(body["instructions"]
            .as_str()
            .unwrap_or("")
            .contains("Codex"));
        assert_eq!(body["prompt_cache_key"], "session-1");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body["input"].as_array().is_some());
    }

    #[tokio::test]
    async fn route_model_resolves_named_route_to_first_available_target() {
        let state = test_state().await;
        let mut first = mk_conn(
            "c1",
            "openai",
            "api_key",
            ConnectionData {
                api_key: Some("sk-live".into()),
                models_override: Some(vec!["gpt-first".into()]),
                ..Default::default()
            },
        );
        first.enabled = false;
        let second = mk_conn(
            "c2",
            "anthropic",
            "api_key",
            ConnectionData {
                api_key: Some("sk-live".into()),
                models_override: Some(vec!["claude-fallback".into()]),
                ..Default::default()
            },
        );
        connections::add_connection(&state.store, first)
            .await
            .unwrap();
        connections::add_connection(&state.store, second)
            .await
            .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "r1".into(),
                name: "smart".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c1".into(),
                        model: "gpt-first".into(),
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c2".into(),
                        model: "claude-fallback".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let target = route_model(&state.store, "smart").await.unwrap().unwrap();

        assert_eq!(target.conn.id, "c2");
        assert_eq!(target.conn.provider, "anthropic");
        assert_eq!(target.upstream_model, "claude-fallback");
    }

    #[tokio::test]
    async fn route_model_round_robin_rotates_targets_across_requests() {
        let state = test_state().await;
        for (id, model) in [("c1", "gpt-one"), ("c2", "gpt-two")] {
            connections::add_connection(
                &state.store,
                mk_conn(
                    id,
                    "openai",
                    "api_key",
                    ConnectionData {
                        api_key: Some("sk-live".into()),
                        models_override: Some(vec![model.into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "rr".into(),
                name: "balanced".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c1".into(),
                        model: "gpt-one".into(),
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c2".into(),
                        model: "gpt-two".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let first = route_model(&state.store, "balanced")
            .await
            .unwrap()
            .unwrap();
        let second = route_model(&state.store, "balanced")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(first.upstream_model, "gpt-one");
        assert_eq!(second.upstream_model, "gpt-two");
    }

    #[tokio::test]
    async fn provider_model_round_robin_rotates_accounts_for_same_provider() {
        let state = test_state().await;
        for id in ["c1", "c2"] {
            connections::add_connection(
                &state.store,
                mk_conn(
                    id,
                    "openai",
                    "api_key",
                    ConnectionData {
                        api_key: Some("sk-live".into()),
                        models_override: Some(vec!["gpt-shared".into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        crate::llm_router::routes::save_provider_account_route(
            &state.store,
            "openai",
            crate::llm_router::routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();

        let first = route_model(&state.store, "openai/gpt-shared")
            .await
            .unwrap()
            .unwrap();
        let second = route_model(&state.store, "openai/gpt-shared")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(first.conn.id, "c1");
        assert_eq!(second.conn.id, "c2");
    }

    #[tokio::test]
    async fn named_route_auto_switches_to_target_matching_request_capability() {
        let state = test_state().await;
        for (id, model) in [("c1", "text-only"), ("c2", "gpt-4o")] {
            connections::add_connection(
                &state.store,
                mk_conn(
                    id,
                    "openai",
                    "api_key",
                    ConnectionData {
                        api_key: Some("sk-live".into()),
                        models_override: Some(vec![model.into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "cap".into(),
                name: "smart-vision".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c1".into(),
                        model: "text-only".into(),
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        connection_id: "c2".into(),
                        model: "gpt-4o".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let body = json!({
            "model": "smart-vision",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}]
            }]
        });
        let target = route_model_for_body(&state.store, "smart-vision", Some(&body))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(target.conn.id, "c2");
        assert_eq!(target.upstream_model, "gpt-4o");
    }

    // -----------------------------------------------------------------
    // Kiro upstream (Task 8)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn kiro_upstream_request_uses_bearer_and_verbatim_headers_no_fingerprint() {
        let state = test_state().await;
        let desc = registry::descriptor("kiro").unwrap();
        let conn = mk_conn(
            "k1",
            "kiro",
            "oauth",
            ConnectionData {
                access_token: Some("kiro-at".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-sonnet-5".into(),
        };
        let kiro_body = json!({"conversationState": {}});
        let req = kiro_upstream_request(&state, &target, &kiro_body)
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer kiro-at"
        );
        assert_eq!(
            req.headers().get("x-amz-target").unwrap(),
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse"
        );
        assert_eq!(
            req.headers().get("accept").unwrap(),
            "application/vnd.amazon.eventstream"
        );
        assert_eq!(
            req.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(
            req.headers().get("user-agent").unwrap(),
            "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0"
        );
        assert_eq!(
            req.headers().get("x-amz-user-agent").unwrap(),
            "aws-sdk-js/3.0.0 kiro-ide/1.0.0"
        );
        assert_eq!(
            req.headers().get("amz-sdk-request").unwrap(),
            "attempt=1; max=3"
        );
        assert!(req.headers().get("amz-sdk-invocation-id").is_some());
        // Neither account-bound tokentype header applies to builder-id auth.
        assert!(req.headers().get("tokentype").is_none());
        assert!(req.headers().get("TokenType").is_none());
        // No fingerprint/cloaking headers beyond the verbatim wire contract.
        let names: std::collections::HashSet<&str> =
            req.headers().keys().map(|k| k.as_str()).collect();
        let allowed: std::collections::HashSet<&str> = [
            "content-type",
            "accept",
            "x-amz-target",
            "user-agent",
            "x-amz-user-agent",
            "amz-sdk-request",
            "amz-sdk-invocation-id",
            "authorization",
        ]
        .into_iter()
        .collect();
        assert!(
            names.is_subset(&allowed),
            "unexpected headers: {:?}",
            names.difference(&allowed).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn kiro_upstream_request_adds_tokentype_header_for_api_key_auth() {
        let state = test_state().await;
        let desc = registry::descriptor("kiro").unwrap();
        let conn = mk_conn(
            "k2",
            "kiro",
            "oauth",
            ConnectionData {
                access_token: Some("at".into()),
                provider_specific: Some(json!({"authMethod": "api_key"})),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-sonnet-5".into(),
        };
        let req = kiro_upstream_request(&state, &target, &json!({}))
            .build()
            .unwrap();
        // Header names are case-insensitive, so `get` finds it either way —
        // just assert the one value that was actually inserted.
        assert_eq!(req.headers().get("tokentype").unwrap(), "API_KEY");
        // Account-bound auth: amazonaws host, not kiro.dev.
        assert!(req
            .url()
            .as_str()
            .contains("codewhisperer.us-east-1.amazonaws.com"));
    }

    #[tokio::test]
    async fn kiro_upstream_request_adds_external_idp_tokentype_header() {
        let state = test_state().await;
        let desc = registry::descriptor("kiro").unwrap();
        let conn = mk_conn(
            "k3",
            "kiro",
            "oauth",
            ConnectionData {
                access_token: Some("at".into()),
                provider_specific: Some(json!({"authMethod": "external_idp"})),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-sonnet-5".into(),
        };
        let req = kiro_upstream_request(&state, &target, &json!({}))
            .build()
            .unwrap();
        // Header names are case-insensitive; only one tokentype header is
        // ever inserted (never both branches).
        assert_eq!(req.headers().get("TokenType").unwrap(), "EXTERNAL_IDP");
    }

    #[tokio::test]
    async fn kiro_upstream_request_honors_base_override() {
        let mut state = test_state().await;
        state.kiro_base_override = Some("http://127.0.0.1:9999/mock-kiro".into());
        let desc = registry::descriptor("kiro").unwrap();
        let conn = mk_conn(
            "k4",
            "kiro",
            "oauth",
            ConnectionData {
                access_token: Some("at".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-sonnet-5".into(),
        };
        let req = kiro_upstream_request(&state, &target, &json!({}))
            .build()
            .unwrap();
        assert_eq!(req.url().as_str(), "http://127.0.0.1:9999/mock-kiro");
    }

    #[test]
    fn kiro_endpoints_orders_amazonaws_first_for_account_bound_auth() {
        let default_order = kiro_endpoints("builder-id", "us-east-1");
        assert_eq!(
            default_order,
            vec![
                "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
                "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
            ]
        );
        let account_bound = kiro_endpoints("api_key", "us-east-1");
        assert_eq!(
            account_bound,
            vec![
                "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
            ]
        );
        // idc / external_idp are also account-bound; region flows into the
        // amazonaws hosts (kiro.dev's own host stays pinned to us-east-1).
        assert_eq!(
            kiro_endpoints("idc", "eu-west-1")[0],
            "https://codewhisperer.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
    }

    #[test]
    fn kiro_request_body_routes_anthropic_and_openai_clients_directly() {
        let data = ConnectionData::default();
        let anthropic_body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let out = kiro_request_body(&anthropic_body, ClientFormat::Anthropic, "m", &data, "c1");
        assert_eq!(out["conversationState"]["conversationId"], "c1");
        assert!(
            out["conversationState"]["currentMessage"]["userInputMessage"]["content"]
                .as_str()
                .unwrap()
                .contains("hi")
        );

        let openai_body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let out2 = kiro_request_body(&openai_body, ClientFormat::OpenAi, "m", &data, "c2");
        assert_eq!(out2["conversationState"]["conversationId"], "c2");
    }

    #[test]
    fn kiro_request_body_routes_responses_client_through_chat_translation_first() {
        let data = ConnectionData::default();
        let body = json!({"model": "m", "input": "hello there", "stream": false});
        let out = kiro_request_body(
            &body,
            ClientFormat::Responses,
            "claude-sonnet-5",
            &data,
            "conv-x",
        );
        assert_eq!(out["conversationState"]["conversationId"], "conv-x");
        let content = out["conversationState"]["currentMessage"]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(content.contains("hello there"));
    }

    #[test]
    fn aggregate_openai_chunks_folds_content_and_usage() {
        let chunks = vec![
            json!({"id": "chatcmpl-1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "lo"}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let v = aggregate_openai_chunks(&chunks, "claude-sonnet-5", 10, 4);
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["id"], "chatcmpl-1");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["prompt_tokens"], 10);
        assert_eq!(v["usage"]["completion_tokens"], 4);
        assert_eq!(v["usage"]["total_tokens"], 14);
    }

    #[test]
    fn aggregate_openai_chunks_folds_tool_call_fragments() {
        let chunks = vec![
            json!({"id": "chatcmpl-2", "choices": [{"index": 0, "delta": {"role": "assistant",
                "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": ""}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"city\":\"Paris\"}"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ];
        let v = aggregate_openai_chunks(&chunks, "m", 0, 0);
        let tc = &v["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["function"]["name"], "get_weather");
        assert_eq!(tc["function"]["arguments"], "{\"city\":\"Paris\"}");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    }
}
