//! The local endpoint server: Anthropic + OpenAI compatible surface on
//! 127.0.0.1, gated by endpoint keys, routed to provider connections.
use crate::llm_router::client::{
    ensure_fresh_for_attempt, route_models_for_body, send_upstream, RouteTarget, UpstreamCtx,
};
use crate::llm_router::codex::normalize_codex_responses_body;
use crate::llm_router::model_effort;
use crate::llm_router::registry::{self, ApiFormat};
use crate::llm_router::{
    claude_cloak, connections, keys, oauth, routes, sse::SseParser, translate,
};
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
            kiro_base_override: self.kiro_base_override.clone(),
            mimo_bootstrap_url_override: None,
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

fn response_effort(body: &Value) -> Option<String> {
    [
        body.pointer("/reasoning/effort"),
        body.get("reasoning_effort"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|effort| !effort.is_empty())
    .map(str::to_string)
}

fn anthropic_effort(body: &Value) -> Option<String> {
    body.pointer("/output_config/effort")
        .and_then(Value::as_str)
        .filter(|effort| !effort.trim().is_empty())
        .map(str::to_string)
}

fn resolved_effort(
    target: &RouteTarget,
    policy: &model_effort::TurnEffortPolicy,
) -> Option<String> {
    crate::llm_router::client::target_effort(target, policy).value
}

fn apply_responses_effort(
    body: &mut Value,
    target: &RouteTarget,
    policy: &model_effort::TurnEffortPolicy,
) -> Option<String> {
    let effort = resolved_effort(target, policy);
    if target.route_target_key.is_some() {
        if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
            reasoning.remove("effort");
        }
        if let Some(object) = body.as_object_mut() {
            object.remove("reasoning_effort");
        }
    }
    effort
}

fn apply_kiro_effort_policy(body: &mut Value) {
    crate::llm_router::client::strip_kiro_effort(body);
}

/// Client speaks Anthropic. `client_fmt` differs from `handle_chat` only in
/// error shape + translation direction.
async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, anthropic_error).await {
        return r;
    }
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let targets = match route_models_for_body(&state.store, &requested, Some(&body)).await {
        Ok(t) => t,
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    // openai-oauth speaks the Responses wire only — skip those targets here;
    // error only if they were the ONLY candidates (spec: skip, not reject).
    let had_any = !targets.is_empty();
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|t| t.conn.provider != "openai-oauth")
        .collect();
    if targets.is_empty() {
        if had_any {
            return anthropic_error(StatusCode::BAD_REQUEST, OPENAI_OAUTH_WRONG_ROUTE_MSG);
        }
        return anthropic_error(
            StatusCode::NOT_FOUND,
            &format!(
                "no enabled connection serves model '{requested}' — add one in Models → Providers"
            ),
        );
    }

    let policy = match model_effort::build_request_effort_policy(
        &state.store,
        &requested,
        anthropic_effort(&body),
    )
    .await
    {
        Ok(policy) => policy,
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let mut failures = Vec::new();
    for mut target in targets {
        // Kiro's CodeWhisperer protocol has no effort field. Strip every
        // client-surface spelling before its dedicated translation pipeline.
        if target.conn.provider == "kiro" {
            let mut attempt_body = body.clone();
            apply_kiro_effort_policy(&mut attempt_body);
            return serve_kiro(&state, target, ClientFormat::Anthropic, &attempt_body).await;
        }
        if let Err(failure) = ensure_fresh_for_attempt(&state.ctx(), &mut target).await {
            let try_next = crate::llm_router::client::should_try_next_target(&failure);
            failures.push(failure);
            if try_next {
                continue;
            }
            break;
        }
        // Captured once per iteration so the `Failed` arm below can record a
        // usage row for the failed attempt (the success branches capture their
        // own `started` closer to the send).
        let started = crate::paths::now_ms();
        let stream = body["stream"].as_bool().unwrap_or(false);
        let mut attempt_body = body.clone();
        attempt_body["model"] = json!(target.upstream_model);

        let outcome: Result<Response, AttemptError> = match target.desc.format {
            ApiFormat::Anthropic => {
                crate::llm_router::client::apply_anthropic_effort(
                    &mut attempt_body,
                    &target,
                    &policy,
                );
                let started = crate::paths::now_ms();
                match proxy_passthrough(&state, &mut target, &attempt_body).await {
                    Ok(resp) => {
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
                        Ok(resp)
                    }
                    Err(e) => Err(e),
                }
            }
            ApiFormat::OpenAi => {
                let mut upstream_body = match translate::anthropic_to_openai_request(&attempt_body)
                {
                    Ok(b) => b,
                    Err(e) => return anthropic_error(StatusCode::BAD_REQUEST, &e.to_string()),
                };
                crate::llm_router::client::apply_openai_effort(
                    &mut upstream_body,
                    &target,
                    &policy,
                    body.get("thinking").is_some(),
                );
                crate::llm_router::client::apply_max_completion_tokens(
                    target.desc,
                    &mut upstream_body,
                );
                if stream {
                    let ctx = RecordCtx {
                        conn_id: target.conn.id.clone(),
                        provider: target.conn.provider.clone(),
                        model: target.upstream_model.clone(),
                        client_format: "anthropic".to_string(),
                        started: crate::paths::now_ms(),
                    };
                    stream_openai_upstream_to_anthropic(&state, &mut target, &upstream_body, ctx)
                        .await
                } else {
                    let started = crate::paths::now_ms();
                    match send_json(&state, &mut target, &upstream_body).await {
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
                            Ok(Json(translate::openai_to_anthropic_response(&v)).into_response())
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        };
        match outcome {
            Ok(resp) => return resp,
            Err(AttemptError::Fatal(resp)) => return resp,
            Err(AttemptError::Failed(failure)) => {
                // Record the failed HTTP attempt too, so per-account error
                // visibility isn't lost when we fail over (or give up).
                crate::llm_router::usage::record(
                    &state.store,
                    &target.conn.id,
                    &target.conn.provider,
                    &target.upstream_model,
                    "anthropic",
                    crate::llm_router::usage::Usage::default(),
                    failure.status.unwrap_or(502),
                    started,
                    Some(failure.message.clone()),
                );
                let try_next = crate::llm_router::client::should_try_next_target(&failure);
                failures.push(failure);
                if try_next {
                    continue;
                }
                break;
            }
        }
    }
    give_up(anthropic_error, &requested, &failures)
}

/// Client speaks OpenAI.
async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers, openai_error).await {
        return r;
    }
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let targets = match route_models_for_body(&state.store, &requested, Some(&body)).await {
        Ok(t) => t,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    // openai-oauth speaks the Responses wire only — skip those targets here;
    // error only if they were the ONLY candidates (spec: skip, not reject).
    let had_any = !targets.is_empty();
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|t| t.conn.provider != "openai-oauth")
        .collect();
    if targets.is_empty() {
        if had_any {
            return openai_error(StatusCode::BAD_REQUEST, OPENAI_OAUTH_WRONG_ROUTE_MSG);
        }
        return openai_error(
            StatusCode::NOT_FOUND,
            &format!("no enabled connection serves model '{requested}'"),
        );
    }

    let policy = match model_effort::build_request_effort_policy(
        &state.store,
        &requested,
        response_effort(&body),
    )
    .await
    {
        Ok(policy) => policy,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let mut failures = Vec::new();
    for mut target in targets {
        // Kiro's CodeWhisperer protocol has no effort field. Strip every
        // client-surface spelling before its dedicated translation pipeline.
        if target.conn.provider == "kiro" {
            let mut attempt_body = body.clone();
            apply_kiro_effort_policy(&mut attempt_body);
            return serve_kiro(&state, target, ClientFormat::OpenAi, &attempt_body).await;
        }
        if let Err(failure) = ensure_fresh_for_attempt(&state.ctx(), &mut target).await {
            let try_next = crate::llm_router::client::should_try_next_target(&failure);
            failures.push(failure);
            if try_next {
                continue;
            }
            break;
        }
        // Captured once per iteration so the `Failed` arm below can record a
        // usage row for the failed attempt (the success branches capture their
        // own `started` closer to the send).
        let started = crate::paths::now_ms();
        let stream = body["stream"].as_bool().unwrap_or(false);
        let mut attempt_body = body.clone();
        attempt_body["model"] = json!(target.upstream_model);
        crate::llm_router::client::apply_openai_effort(&mut attempt_body, &target, &policy, false);

        let outcome: Result<Response, AttemptError> = match target.desc.format {
            ApiFormat::OpenAi => {
                let started = crate::paths::now_ms();
                match proxy_passthrough(&state, &mut target, &attempt_body).await {
                    Ok(resp) => {
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
                        Ok(resp)
                    }
                    Err(e) => Err(e),
                }
            }
            ApiFormat::Anthropic => {
                let mut upstream_body = match translate::openai_to_anthropic_request(&attempt_body)
                {
                    Ok(b) => b,
                    Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e.to_string()),
                };
                crate::llm_router::client::apply_anthropic_effort(
                    &mut upstream_body,
                    &target,
                    &policy,
                );
                if stream {
                    let ctx = RecordCtx {
                        conn_id: target.conn.id.clone(),
                        provider: target.conn.provider.clone(),
                        model: target.upstream_model.clone(),
                        client_format: "openai".to_string(),
                        started: crate::paths::now_ms(),
                    };
                    stream_anthropic_upstream_to_openai(&state, &mut target, &upstream_body, ctx)
                        .await
                } else {
                    let started = crate::paths::now_ms();
                    match send_json(&state, &mut target, &upstream_body).await {
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
                            Ok(Json(translate::anthropic_to_openai_response(&v)).into_response())
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        };
        match outcome {
            Ok(resp) => return resp,
            Err(AttemptError::Fatal(resp)) => return resp,
            Err(AttemptError::Failed(failure)) => {
                // Record the failed HTTP attempt too, so per-account error
                // visibility isn't lost when we fail over (or give up).
                crate::llm_router::usage::record(
                    &state.store,
                    &target.conn.id,
                    &target.conn.provider,
                    &target.upstream_model,
                    "openai",
                    crate::llm_router::usage::Usage::default(),
                    failure.status.unwrap_or(502),
                    started,
                    Some(failure.message.clone()),
                );
                let try_next = crate::llm_router::client::should_try_next_target(&failure);
                failures.push(failure);
                if try_next {
                    continue;
                }
                break;
            }
        }
    }
    give_up(openai_error, &requested, &failures)
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
    let chat = crate::llm_router::responses::responses_request_to_chat(&body);
    let requested = chat["model"].as_str().unwrap_or("").to_string();
    let targets = match route_models_for_body(&state.store, &requested, Some(&body)).await {
        Ok(t) => t,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    // No openai-oauth filter here — this endpoint serves Codex natively.
    if targets.is_empty() {
        return openai_error(
            StatusCode::NOT_FOUND,
            &format!("no enabled connection serves model '{requested}'"),
        );
    }

    let stream = chat["stream"].as_bool().unwrap_or(false);
    let policy = match model_effort::build_request_effort_policy(
        &state.store,
        &requested,
        response_effort(&body),
    )
    .await
    {
        Ok(policy) => policy,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let mut failures = Vec::new();
    for mut target in targets {
        // Kiro's CodeWhisperer protocol has no effort field. Strip every
        // client-surface spelling before its dedicated translation pipeline.
        if target.conn.provider == "kiro" {
            let mut attempt_body = body.clone();
            apply_kiro_effort_policy(&mut attempt_body);
            return serve_kiro(&state, target, ClientFormat::Responses, &attempt_body).await;
        }
        if let Err(failure) = ensure_fresh_for_attempt(&state.ctx(), &mut target).await {
            let try_next = crate::llm_router::client::should_try_next_target(&failure);
            failures.push(failure);
            if try_next {
                continue;
            }
            break;
        }

        // Codex (openai-oauth) speaks the Responses wire natively both ways —
        // no chat translation applies. Normalize the client's Responses body
        // to the subset accepted by ChatGPT's Codex backend, then proxy it
        // same-format.
        if target.conn.provider == "openai-oauth" {
            let mut passthrough_body = body.clone();
            let effort = apply_responses_effort(&mut passthrough_body, &target, &policy);
            normalize_codex_responses_body(
                &mut passthrough_body,
                &target.upstream_model,
                effort.as_deref(),
                None,
            );
            let started = crate::paths::now_ms();
            match proxy_passthrough(&state, &mut target, &passthrough_body).await {
                Ok(resp) => {
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
                Err(AttemptError::Fatal(resp)) => return resp,
                Err(AttemptError::Failed(failure)) => {
                    // Record the failed HTTP attempt too, so per-account error
                    // visibility isn't lost when we fail over (or give up).
                    crate::llm_router::usage::record(
                        &state.store,
                        &target.conn.id,
                        &target.conn.provider,
                        &target.upstream_model,
                        "responses",
                        crate::llm_router::usage::Usage::default(),
                        failure.status.unwrap_or(502),
                        started,
                        Some(failure.message.clone()),
                    );
                    let try_next = crate::llm_router::client::should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    break;
                }
            }
        }

        let mut attempt_chat = chat.clone();
        attempt_chat["model"] = json!(target.upstream_model);
        crate::llm_router::client::apply_openai_effort(&mut attempt_chat, &target, &policy, false);
        let started = crate::paths::now_ms();

        // Normalize the upstream response to OpenAI chat shape, then encode Responses.
        let upstream_body = match target.desc.format {
            ApiFormat::OpenAi => {
                let mut b = attempt_chat.clone();
                crate::llm_router::client::apply_max_completion_tokens(target.desc, &mut b);
                b
            }
            ApiFormat::Anthropic => match translate::openai_to_anthropic_request(&attempt_chat) {
                Ok(mut body) => {
                    crate::llm_router::client::apply_anthropic_effort(&mut body, &target, &policy);
                    body
                }
                Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e.to_string()),
            },
        };

        let outcome: Result<Response, AttemptError> = if stream {
            stream_responses(&state, &mut target, &upstream_body, started).await
        } else {
            match send_json(&state, &mut target, &upstream_body).await {
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
                    Ok(
                        Json(crate::llm_router::responses::chat_response_to_responses(
                            &chat_resp,
                        ))
                        .into_response(),
                    )
                }
                Err(e) => Err(e),
            }
        };
        match outcome {
            Ok(resp) => return resp,
            Err(AttemptError::Fatal(resp)) => return resp,
            Err(AttemptError::Failed(failure)) => {
                // Record the failed HTTP attempt too, so per-account error
                // visibility isn't lost when we fail over (or give up).
                crate::llm_router::usage::record(
                    &state.store,
                    &target.conn.id,
                    &target.conn.provider,
                    &target.upstream_model,
                    "responses",
                    crate::llm_router::usage::Usage::default(),
                    failure.status.unwrap_or(502),
                    started,
                    Some(failure.message.clone()),
                );
                let try_next = crate::llm_router::client::should_try_next_target(&failure);
                failures.push(failure);
                if try_next {
                    continue;
                }
                break;
            }
        }
    }
    give_up(openai_error, &requested, &failures)
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
            let id = format!("{}/{}", desc.family, m);
            if seen.insert(id.clone()) {
                data.push(json!({"id": id, "object": "model", "owned_by": desc.family}));
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

/// The outcome of a single upstream attempt that didn't succeed.
enum AttemptError {
    /// Return to the client immediately, bypassing failover. Transport-level
    /// send errors no longer take this arm (see `connect_upstream`/
    /// `send_json`, which now classify them as retryable `Failed` instead) —
    /// nothing constructs `Fatal` today, but the variant and its match arms
    /// are kept as the non-retryable escape hatch for a future error class
    /// that genuinely shouldn't fail over (e.g. a request-shape/translation
    /// error caught here rather than earlier).
    #[allow(dead_code)]
    Fatal(Response),
    /// Candidate for failover — try the next target if the predicate allows.
    Failed(crate::llm_router::client::UpstreamAttemptFailure),
}

/// All targets exhausted (or the failure was non-retryable): shape the
/// accumulated failures into a client-format error. Status = the last
/// failure's upstream status, else 502.
fn give_up(
    err: fn(StatusCode, &str) -> Response,
    requested: &str,
    failures: &[crate::llm_router::client::UpstreamAttemptFailure],
) -> Response {
    let status = failures
        .iter()
        .rev()
        .find_map(|f| f.status)
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    err(
        status,
        &crate::llm_router::client::fallback_error(requested, failures).to_string(),
    )
}

/// Send the upstream request and pre-check status. A non-2xx becomes a
/// retryable `Failed`; a transport-level send error (DNS/TLS/connect/reset)
/// is also a retryable `Failed` of the transport class — matching the native
/// path, which rotates past send errors.
async fn connect_upstream(
    state: &AppState,
    target: &mut RouteTarget,
    body: &Value,
) -> Result<reqwest::Response, AttemptError> {
    match send_upstream(&state.ctx(), target, body).await {
        Ok(resp) if resp.status().is_success() => Ok(resp),
        Ok(resp) => Err(AttemptError::Failed(
            crate::llm_router::client::upstream_status_failure(target.conn.provider.clone(), resp)
                .await,
        )),
        Err(e) => Err(AttemptError::Failed(
            crate::llm_router::client::UpstreamAttemptFailure::transport(
                target.conn.provider.clone(),
                format!("upstream send failed: {e}"),
            ),
        )),
    }
}

async fn send_json(
    state: &AppState,
    target: &mut RouteTarget,
    body: &Value,
) -> Result<Value, AttemptError> {
    let tool_map = claude_cloak::tool_name_map_for(&target.conn.provider, body);
    let resp = send_upstream(&state.ctx(), target, body)
        .await
        .map_err(|e| {
            AttemptError::Failed(
                crate::llm_router::client::UpstreamAttemptFailure::transport(
                    target.conn.provider.clone(),
                    format!("upstream send failed: {e}"),
                ),
            )
        })?;
    let status = resp.status();
    let mut v: Value = resp.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        let msg = v["error"]["message"].as_str().unwrap_or("upstream error");
        return Err(AttemptError::Failed(
            crate::llm_router::client::UpstreamAttemptFailure {
                provider: target.conn.provider.clone(),
                message: msg.to_string(),
                status: Some(status.as_u16()),
                category: crate::llm_router::provenance::classify_failure(
                    Some(status.as_u16()),
                    false,
                    msg,
                ),
            },
        ));
    }
    claude_cloak::decloak_response(&mut v, &tool_map);
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
/// A non-2xx upstream becomes a retryable `Failed` (via `connect_upstream`)
/// rather than being forwarded verbatim — the caller decides whether to fail
/// over or shape the accumulated failures into a `give_up` error.
async fn proxy_passthrough(
    state: &AppState,
    target: &mut RouteTarget,
    body: &Value,
) -> Result<Response, AttemptError> {
    let tool_map = claude_cloak::tool_name_map_for(&target.conn.provider, body);
    let resp = connect_upstream(state, target, body).await?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if !tool_map.is_empty() {
        if ct.contains("text/event-stream") || body["stream"].as_bool().unwrap_or(false) {
            return Ok(sse_response(spawn_anthropic_passthrough_decloak_pump(
                resp, tool_map,
            )));
        }
        let mut v: Value = resp.json().await.unwrap_or(json!({}));
        claude_cloak::decloak_response(&mut v, &tool_map);
        return Ok(Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap());
    }
    let stream = resp.bytes_stream();
    Ok(Response::builder()
        .status(status)
        .header("content-type", ct)
        .body(Body::from_stream(stream))
        .unwrap())
}

fn spawn_anthropic_passthrough_decloak_pump(
    resp: reqwest::Response,
    tool_map: claude_cloak::ToolNameMap,
) -> Body {
    use futures::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut parser = SseParser::default();
        let mut stream = resp.bytes_stream();
        'pump: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    let data = json!({"type": "error", "error": {"type": "api_error", "message": format!("upstream stream interrupted: {e}")}});
                    let _ = tx.send(Ok(format_sse("error", &data))).await;
                    break;
                }
            };
            for ev in parser.feed(&chunk) {
                if ev.data == "[DONE]" {
                    continue;
                }
                let name = ev.event.clone().unwrap_or_default();
                let Ok(mut v) = serde_json::from_str::<Value>(&ev.data) else {
                    continue;
                };
                claude_cloak::decloak_event(&name, &mut v, &tool_map);
                if tx.send(Ok(format_sse(&name, &v))).await.is_err() {
                    break 'pump;
                }
            }
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
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
    tool_map: claude_cloak::ToolNameMap,
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
                if let Ok(mut v) = serde_json::from_str::<Value>(&ev.data) {
                    claude_cloak::decloak_event(name, &mut v, &tool_map);
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

/// Client=Anthropic, upstream=OpenAI, stream=true. Pre-stream only: a non-2xx
/// upstream becomes a retryable `Failed` (streaming retries happen before any
/// pump spawns; a mid-stream error still ends the SSE with an error frame).
async fn stream_openai_upstream_to_anthropic(
    state: &AppState,
    target: &mut RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Result<Response, AttemptError> {
    let model = upstream_body["model"].as_str().unwrap_or("").to_string();
    let resp = connect_upstream(state, target, upstream_body).await?;
    Ok(sse_response(spawn_openai_to_anthropic_pump(
        resp,
        model,
        state.store.clone(),
        ctx,
    )))
}

/// Client=OpenAI, upstream=Anthropic, stream=true. Pre-stream retry only.
async fn stream_anthropic_upstream_to_openai(
    state: &AppState,
    target: &mut RouteTarget,
    upstream_body: &Value,
    ctx: RecordCtx,
) -> Result<Response, AttemptError> {
    let tool_map = claude_cloak::tool_name_map_for(&target.conn.provider, upstream_body);
    let resp = connect_upstream(state, target, upstream_body).await?;
    Ok(sse_response(spawn_anthropic_to_openai_pump(
        resp,
        state.store.clone(),
        ctx,
        tool_map,
    )))
}

/// Client=Responses, stream=true. Normalizes the upstream response (OpenAI or
/// Anthropic format) to OpenAI chat chunks, then encodes Responses SSE.
/// Pre-stream retry only.
async fn stream_responses(
    state: &AppState,
    target: &mut RouteTarget,
    upstream_body: &Value,
    started: i64,
) -> Result<Response, AttemptError> {
    let resp = connect_upstream(state, target, upstream_body).await?;
    let anthropic_upstream = matches!(target.desc.format, ApiFormat::Anthropic);
    let tool_map = claude_cloak::tool_name_map_for(&target.conn.provider, upstream_body);
    let store = state.store.clone();
    let ctx = RecordCtx {
        conn_id: target.conn.id.clone(),
        provider: target.conn.provider.clone(),
        model: target.upstream_model.clone(),
        client_format: "responses".into(),
        started,
    };
    Ok(sse_response(spawn_responses_pump(
        resp,
        anthropic_upstream,
        store,
        ctx,
        tool_map,
    )))
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
    tool_map: claude_cloak::ToolNameMap,
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
                        .map(|mut v| {
                            claude_cloak::decloak_event(name, &mut v, &tool_map);
                            anth.feed(name, &v)
                        })
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
            // Kiro terminates plain-text turns with `messageStopEvent` but
            // tool-use turns with `metadataEvent {stopReason:"TOOL_USE"}` (both
            // now set the terminal in `KiroToOpenAiStream::feed`). A clean EOF
            // with neither is still a valid completion (matches 9router, which
            // finishes on EOF) — feed the EOF finish chunk through, then emit
            // the client-format finish. A genuine mid-stream break is a
            // transport `Err` handled above, not this path.
            if !kiro.saw_terminal() {
                for oai in kiro.finish() {
                    let _ =
                        forward_openai_chunk(&tx, client_format, &oai, &mut anth, &mut rs).await;
                }
            }
            emit_kiro_finish(&tx, client_format, &mut anth, &mut rs).await;
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
    use crate::llm_router::client::{route_model, route_model_for_body};
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
                        provider: "openai".into(),
                        model: "gpt-first".into(),
                        effort: None,
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-fallback".into(),
                        effort: None,
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
                        provider: "openai".into(),
                        model: "gpt-one".into(),
                        effort: None,
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "gpt-two".into(),
                        effort: None,
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
                        provider: "openai".into(),
                        model: "text-only".into(),
                        effort: None,
                    },
                    crate::llm_router::routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "gpt-4o".into(),
                        effort: None,
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
            route_target_key: None,
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
            route_target_key: None,
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
            route_target_key: None,
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
            route_target_key: None,
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
    fn kiro_request_body_ignores_client_effort_fields() {
        let data = ConnectionData::default();
        let body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "output_config": {"effort": "low"},
            "reasoning_effort": "medium",
            "reasoning": {"effort": "high"}
        });

        let out = kiro_request_body(&body, ClientFormat::OpenAi, "m", &data, "c1");

        assert_eq!(out["inferenceConfig"], json!({"maxTokens": 32000}));
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

    async fn effort_capture_upstream() -> (u16, Arc<Mutex<Vec<Value>>>) {
        use axum::{extract::State, routing::post, Json, Router};

        async fn capture(
            State(captured): State<Arc<Mutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            captured.lock().unwrap().push(body);
            Json(json!({"id": "captured"}))
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(capture))
            .route("/v1/chat/completions", post(capture))
            .route("/responses", post(capture))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, captured)
    }

    fn effort_meta(default: &str) -> crate::llm_router::model_effort::DiscoveredModelMeta {
        crate::llm_router::model_effort::DiscoveredModelMeta {
            display_name: None,
            effort_options: Some(
                ["low", "medium", "high"]
                    .into_iter()
                    .map(
                        |value| crate::llm_router::model_effort::ReasoningEffortOption {
                            value: value.into(),
                            label: value.into(),
                            description: None,
                        },
                    )
                    .collect(),
            ),
            default_effort_advertised: true,
            default_effort: Some(default.into()),
        }
    }

    async fn post_router(port: u16, key: &str, path: &str, body: Value) {
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("x-api-key", key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{response:?}");
        let _ = response.bytes().await.unwrap();
    }

    #[tokio::test]
    async fn messages_named_route_uses_target_effort_over_caller_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "anthropic-route",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["claude-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "claude-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "anthropic-effort-route".into(),
                name: "smart-anthropic".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "claude-route".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/messages",
            json!({
                "model": "smart-anthropic",
                "max_tokens": 10,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "low"}
            }),
        )
        .await;

        assert_eq!(
            captured.lock().unwrap()[0]["output_config"]["effort"],
            "high"
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn chat_named_route_uses_configured_then_provider_default_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "openai-route",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["gpt-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "openai-effort-route".into(),
                name: "smart-openai".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-route".into(),
                    effort: None,
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let preference = crate::llm_router::model_effort::ModelPreferenceKey {
            family: "openai".into(),
            model: "gpt-route".into(),
        };
        crate::llm_router::model_effort::set_preference(&state.store, &preference, Some("medium"))
            .await
            .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();
        let request = || {
            json!({
                "model": "smart-openai",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": "low"
            })
        };

        post_router(port, &key.key, "/v1/chat/completions", request()).await;
        crate::llm_router::model_effort::set_preference(&state.store, &preference, None)
            .await
            .unwrap();
        post_router(port, &key.key, "/v1/chat/completions", request()).await;

        {
            let captured = captured.lock().unwrap();
            assert_eq!(captured[0]["reasoning_effort"], "medium");
            assert_eq!(captured[1]["reasoning_effort"], "high");
        }
        server.stop().await;
    }

    #[tokio::test]
    async fn responses_codex_named_route_replaces_caller_effort_with_target_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "codex-route",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("test-token".into()),
                    expires_at: Some(crate::paths::now_ms() + 100 * 24 * 60 * 60 * 1_000),
                    last_refresh_at: Some(crate::paths::now_ms()),
                    needs_relogin: Some(false),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}")),
                    models_override: Some(vec!["gpt-codex-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-codex-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "codex-effort-route".into(),
                name: "smart-codex".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-codex-route".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/responses",
            json!({
                "model": "smart-codex",
                "input": "hi",
                "reasoning": {"effort": "low", "summary": "detailed"}
            }),
        )
        .await;

        {
            let captured = captured.lock().unwrap();
            assert_eq!(captured[0]["reasoning"]["effort"], "high");
            assert_eq!(captured[0]["reasoning"]["summary"], "detailed");
        }
        server.stop().await;
    }

    #[tokio::test]
    async fn chat_direct_model_keeps_caller_effort_first() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "openai-direct",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["gpt-direct".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-direct".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/chat/completions",
            json!({
                "model": "openai/gpt-direct",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": "low"
            }),
        )
        .await;

        assert_eq!(captured.lock().unwrap()[0]["reasoning_effort"], "low");
        server.stop().await;
    }

    #[tokio::test]
    async fn messages_translation_named_route_uses_target_effort_over_caller_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "openai-messages-route",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["gpt-messages-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-messages-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "openai-messages-effort-route".into(),
                name: "smart-openai-messages".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-messages-route".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/messages",
            json!({
                "model": "smart-openai-messages",
                "max_tokens": 10,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "low"}
            }),
        )
        .await;

        assert_eq!(captured.lock().unwrap()[0]["reasoning_effort"], "high");
        server.stop().await;
    }

    #[tokio::test]
    async fn chat_translation_named_route_uses_target_effort_over_caller_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "anthropic-chat-route",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["claude-chat-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "claude-chat-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "anthropic-chat-effort-route".into(),
                name: "smart-anthropic-chat".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "claude-chat-route".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/chat/completions",
            json!({
                "model": "smart-anthropic-chat",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": "low"
            }),
        )
        .await;

        assert_eq!(
            captured.lock().unwrap()[0]["output_config"]["effort"],
            "high"
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn responses_named_route_uses_target_effort_over_caller_effort() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "openai-responses-route",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["gpt-responses-route".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-responses-route".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        crate::llm_router::routes::save_model_route(
            &state.store,
            crate::llm_router::routes::ModelRouteInfo {
                id: "openai-responses-effort-route".into(),
                name: "smart-openai-responses".into(),
                enabled: true,
                strategy: crate::llm_router::routes::ModelRouteStrategy::Fallback,
                targets: vec![crate::llm_router::routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-responses-route".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/responses",
            json!({
                "model": "smart-openai-responses",
                "input": "hi",
                "reasoning": {"effort": "low"}
            }),
        )
        .await;

        assert_eq!(captured.lock().unwrap()[0]["reasoning_effort"], "high");
        server.stop().await;
    }

    #[tokio::test]
    async fn direct_responses_uses_flat_caller_effort_when_nested_effort_is_empty() {
        let state = test_state().await;
        let (upstream_port, captured) = effort_capture_upstream().await;
        connections::add_connection(
            &state.store,
            mk_conn(
                "openai-direct-responses",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-test".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{upstream_port}/v1")),
                    models_override: Some(vec!["gpt-direct-responses".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "gpt-direct-responses".into(),
                        effort_meta("high"),
                    )])),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        let key = keys::create_key(&state.store, "test").await.unwrap();
        let server = RouterServer::new(state.store.clone());
        let port = server.start(0).await.unwrap();

        post_router(
            port,
            &key.key,
            "/v1/responses",
            json!({
                "model": "openai/gpt-direct-responses",
                "input": "hi",
                "reasoning": {"effort": ""},
                "reasoning_effort": "low"
            }),
        )
        .await;

        assert_eq!(captured.lock().unwrap()[0]["reasoning_effort"], "low");
        server.stop().await;
    }
}
