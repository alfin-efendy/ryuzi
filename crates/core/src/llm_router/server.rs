//! The local endpoint server: Anthropic + OpenAI compatible surface on
//! 127.0.0.1, gated by endpoint keys, routed to provider connections.
use crate::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderDescriptor};
use crate::llm_router::{connections, keys, oauth, sse::SseParser, translate};
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

/// Anthropic's Claude-Code-branded system prefix, required by the
/// anthropic-oauth (Claude subscription) upstream — that's the wire's own
/// contract for the OAuth token, not a client-visible feature; no other
/// header/body cloaking is added (spec #2).
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Ensure the outgoing Anthropic `system` field begins with the Claude-Code
/// prefix block: a bare string is wrapped into `[prefix, {type:text,text:the
/// old string}]`; an array gets the prefix prepended (skipped if already
/// present); an absent/other value is replaced with `[prefix]`.
fn inject_claude_system_prompt(body: &mut Value) {
    let prefix = json!({"type": "text", "text": CLAUDE_CODE_SYSTEM_PROMPT});
    let current = body.get("system").cloned().unwrap_or(Value::Null);
    let new_system = match current {
        Value::String(s) => json!([prefix, {"type": "text", "text": s}]),
        Value::Array(mut arr) => {
            if arr.first() != Some(&prefix) {
                arr.insert(0, prefix);
            }
            Value::Array(arr)
        }
        _ => json!([prefix]),
    };
    body["system"] = new_system;
}

fn upstream_request(
    state: &AppState,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    if connections::is_oauth(&target.conn) {
        return oauth_upstream_request(state, target, body);
    }
    if target.desc.no_auth {
        return free_upstream_request(state, target, body);
    }
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

/// OAuth-authenticated upstream request (anthropic-oauth or openai-oauth).
/// The credential is ALWAYS `data.access_token` — never `data.api_key`,
/// which oauth connections don't populate.
fn oauth_upstream_request(
    state: &AppState,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let access_token = target.conn.data.access_token.clone().unwrap_or_default();
    match target.conn.provider.as_str() {
        "anthropic-oauth" => {
            // Same base-resolution as the api-key path (honors a per-connection
            // override, which is how tests point this at a mock upstream) —
            // in production that's just the descriptor's real Anthropic base.
            let base = connections::effective_base_url(target.desc, &target.conn)
                .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
            let mut anthropic_body = body.clone();
            inject_claude_system_prompt(&mut anthropic_body);
            Ok(state
                .http
                .post(format!("{base}/messages"))
                .json(&anthropic_body)
                .header("authorization", format!("Bearer {access_token}"))
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "oauth-2025-04-20"))
        }
        "openai-oauth" => {
            // Codex's Responses wire is a fixed protocol endpoint, distinct
            // from the descriptor's placeholder `base_url` (see the NOTE on
            // the `openai-oauth` catalog entry in registry.rs) — Codex CLI
            // never talks to anything else, so this isn't override-able.
            const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
            let mut req = state
                .http
                .post(format!("{CODEX_BASE}/responses"))
                .json(body)
                .header("authorization", format!("Bearer {access_token}"))
                .header("originator", "codex_cli_rs")
                .header("session_id", uuid::Uuid::new_v4().to_string());
            if let Some(account_id) = target
                .conn
                .data
                .provider_specific
                .as_ref()
                .and_then(|v| v.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
            {
                req = req.header("chatgpt-account-id", account_id);
            }
            Ok(req)
        }
        other => Err(anyhow::anyhow!(
            "no OAuth upstream wiring for provider `{other}`"
        )),
    }
}

/// Free-tier passthrough (opencode-free): no real credential, just the
/// wire's own placeholder bearer + client-id header.
fn free_upstream_request(
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
    Ok(state
        .http
        .post(format!("{base}{path}"))
        .json(body)
        .header("authorization", "Bearer public")
        .header("x-opencode-client", "desktop"))
}

/// Build + send the upstream request; on a 401/403 from an OAuth-backed
/// target, refresh once via `force_refresh` and retry the same request. The
/// retry is kept only if the refresh itself succeeds — a failed refresh
/// falls through to the original (failed) response so the caller's normal
/// error handling still fires. This covers non-stream calls directly and
/// gives streaming calls a pre-stream retry (called before any response
/// bytes are read); a 401 that arrives mid-stream is NOT retried.
async fn send_upstream(
    state: &AppState,
    target: &mut RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = upstream_request(state, target, body)?.send().await?;
    if matches!(resp.status().as_u16(), 401 | 403) && connections::is_oauth(&target.conn) {
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
            return Ok(upstream_request(state, target, body)?.send().await?);
        }
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Handlers
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
    let mut target = match route_model(&state.store, &requested).await {
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
    let mut target = match route_model(&state.store, &requested).await {
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
    let mut target = match route_model(&state.store, &requested).await {
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
    // resolution above). Pass the client's ORIGINAL Responses body straight
    // through (only rewriting `model` to the bare upstream id), same-format
    // proxy style, just like `proxy_passthrough` elsewhere.
    if target.conn.provider == "openai-oauth" {
        let mut passthrough_body = body.clone();
        passthrough_body["model"] = json!(target.upstream_model);
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
    let resp = send_upstream(state, target, body).await.map_err(|e| {
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
    let resp = match send_upstream(state, target, body).await {
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
    let resp = match send_upstream(state, target, upstream_body).await {
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
    let resp = match send_upstream(state, target, upstream_body).await {
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
    let resp = match send_upstream(state, target, upstream_body).await {
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
    fn inject_system_prompt_wraps_string_system() {
        let mut body = json!({"system": "be nice", "messages": []});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sys[1]["text"], "be nice");
    }

    #[test]
    fn inject_system_prompt_prepends_to_array_system() {
        let mut body = json!({"system": [{"type": "text", "text": "custom block"}]});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sys[1]["text"], "custom block");
    }

    #[test]
    fn inject_system_prompt_is_idempotent_when_already_present() {
        let mut body = json!({"system": [
            {"type": "text", "text": CLAUDE_CODE_SYSTEM_PROMPT},
            {"type": "text", "text": "custom block"}
        ]});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2, "must not duplicate an already-present prefix");
        assert_eq!(sys[1]["text"], "custom block");
    }

    #[test]
    fn inject_system_prompt_sets_when_absent() {
        let mut body = json!({"messages": []});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
    }

    #[tokio::test]
    async fn oauth_request_for_anthropic_uses_access_token_and_injects_system_prompt() {
        let state = test_state().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let conn = mk_conn(
            "c1",
            "anthropic-oauth",
            "oauth",
            ConnectionData {
                // Must NOT be read for oauth — access_token is the only
                // legitimate credential source.
                api_key: Some("should-not-be-used".into()),
                access_token: Some("at-secret".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-x".into(),
        };
        let body = json!({"model": "claude-x", "system": "be helpful", "messages": []});
        let req = upstream_request(&state, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(req.url().as_str(), "https://api.anthropic.com/v1/messages");
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-secret"
        );
        assert_eq!(
            req.headers().get("anthropic-beta").unwrap(),
            "oauth-2025-04-20"
        );
        assert_eq!(
            req.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["system"][0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sent["system"][1]["text"], "be helpful");
    }

    #[tokio::test]
    async fn oauth_request_for_openai_hits_codex_responses_with_account_and_session_headers() {
        let state = test_state().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let conn = mk_conn(
            "c2",
            "openai-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("at-codex".into()),
                provider_specific: Some(json!({"chatgpt_account_id": "acct-1"})),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2-codex".into(),
        };
        let body = json!({"model": "gpt-5.2-codex", "input": "hi"});
        let req = upstream_request(&state, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-codex"
        );
        assert_eq!(req.headers().get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acct-1");
        assert!(req.headers().get("session_id").is_some());
    }

    #[tokio::test]
    async fn oauth_request_for_openai_omits_account_header_when_absent() {
        let state = test_state().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let conn = mk_conn(
            "c3",
            "openai-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("at-codex".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2-codex".into(),
        };
        let body = json!({"model": "gpt-5.2-codex", "input": "hi"});
        let req = upstream_request(&state, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert!(req.headers().get("chatgpt-account-id").is_none());
    }

    #[tokio::test]
    async fn free_provider_uses_public_bearer_and_opencode_client_header() {
        let state = test_state().await;
        let desc = registry::descriptor("opencode-free").unwrap();
        let conn = mk_conn("c4", "opencode-free", "none", ConnectionData::default());
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "grok-code".into(),
        };
        let body = json!({"model": "grok-code", "messages": []});
        let req = upstream_request(&state, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer public");
        assert_eq!(req.headers().get("x-opencode-client").unwrap(), "desktop");
    }

    #[tokio::test]
    async fn api_key_provider_is_unaffected_by_oauth_free_branches() {
        let state = test_state().await;
        let desc = registry::descriptor("openai").unwrap();
        let conn = mk_conn(
            "c5",
            "openai",
            "api_key",
            ConnectionData {
                api_key: Some("sk-live".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2".into(),
        };
        let body = json!({"model": "gpt-5.2", "messages": []});
        let req = upstream_request(&state, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer sk-live"
        );
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
