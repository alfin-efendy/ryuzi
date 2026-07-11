//! A minimal HTTP surface over the embedded [`ControlPlane`], mirroring
//! opencode's `serve`. Exposes session listing, transcript, prompt, and a live
//! Server-Sent-Events stream of [`CoreEvent`]s so external clients (or a remote
//! `attach`) can drive and observe sessions.

use crate::control::ControlPlane;
use crate::llm_router::server::RouterServer;
use crate::plugins::{CorePlugin, PluginSource};
use crate::settings::SettingsStore;
use crate::store::Device;
use axum::extract::{ConnectInfo, Path, State};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

/// Wire protocol version reported by `/health` for remote-runner clients to
/// negotiate compatibility against.
pub const PROTOCOL_VERSION: u32 = 1;

/// Shared state for the control API router.
#[derive(Clone)]
pub struct ApiState {
    pub cp: Arc<ControlPlane>,
    pub router_server: Arc<RouterServer>,
    /// The loopback-only control token (see [`require_token`]). Always a
    /// real secret — there is no auth-disable mode.
    pub control_token: String,
}

/// Build the HTTP router over a control plane.
///
/// `POST /pair` is public alongside `/health` — deliberately outside the
/// `authed` sub-router's `require_token` layer, since a device presenting a
/// pairing code has no bearer token yet (it IS the bootstrap; see
/// `crate::pairing`). Its rate limiter (see [`PairLimiter`]) is created
/// fresh here, once per `router()` call, and captured by the route's
/// closure rather than added to `ApiState` — see [`PairLimiter`]'s doc
/// comment for why.
pub fn router(state: ApiState) -> Router {
    let authed = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{pk}/messages", get(list_messages))
        .route("/sessions/{pk}/prompt", post(prompt))
        .route("/projects/{id}/session", post(start))
        .route("/events", get(events))
        .route("/plugins", get(list_plugins))
        .route("/plugins/{id}", get(get_plugin))
        .route("/rpc/{method}", post(rpc))
        .route("/approvals/{request_id}", post(resolve_approval_route))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    let pair_limiter = PairLimiter::new();
    let pair_route = post(
        move |State(state): State<ApiState>, Json(body): Json<PairRequest>| async move {
            pair(state, pair_limiter, body).await
        },
    );
    Router::new()
        .route("/health", get(health))
        .route("/pair", pair_route)
        .merge(authed)
        .with_state(state)
}

/// Reject requests without a valid `Authorization: Bearer <token>` header.
/// Never applied to `GET /health`. Two-tier auth (see [`authorize`] for the
/// pure decision logic this delegates to):
///
/// 1. A bearer whose SHA-256 hash matches a non-revoked `devices.token_hash`
///    row authenticates from ANY peer — this is how a paired remote client
///    (Phase 2 pairing) reaches the control API.
/// 2. Otherwise, the daemon's own `control_token` authenticates ONLY when
///    the peer is loopback (`ConnectInfo`'s `ip.is_loopback()`) — the
///    control token must never be accepted from a remote peer, even if
///    somehow leaked/guessed.
///
/// There is no auth-disable mode: every `ApiState` carries a real
/// `control_token`.
async fn require_token(
    State(state): State<ApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);

    let Some(presented) = presented else {
        return unauthorized();
    };

    let device_hash = crate::update::asset::sha256_hex(presented.as_bytes());
    let device = state
        .cp
        .store()
        .find_device_by_token_hash(&device_hash)
        .await
        .ok()
        .flatten();

    if authorize(
        peer.ip().is_loopback(),
        &presented,
        &state.control_token,
        device.as_ref(),
    ) {
        next.run(req).await
    } else {
        unauthorized()
    }
}

/// Pure two-tier auth decision, factored out of [`require_token`] so the
/// "control token from a non-loopback peer must be rejected" branch is
/// unit-testable without standing up a real non-loopback socket (test
/// servers only ever bind loopback). `device` is the already-resolved,
/// already-revoked-filtered `devices` row for the presented bearer's hash
/// (see `Store::find_device_by_token_hash`) — a `Some` here authenticates
/// unconditionally (device tokens work from any peer); otherwise the
/// `control_token` authenticates only when `peer_is_loopback`.
fn authorize(
    peer_is_loopback: bool,
    presented: &str,
    control_token: &str,
    device: Option<&Device>,
) -> bool {
    if device.is_some() {
        return true;
    }
    peer_is_loopback && crate::control_token::verify(presented, control_token)
}

fn unauthorized() -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "missing or invalid bearer token" })),
    )
        .into_response()
}

/// Configuration for [`serve`]: which address/port to bind, and an optional
/// TLS server config to serve over (remote-runner, Phase 2/3). `tls: None`
/// preserves today's plaintext-loopback behavior; the caller (P2-7) is
/// responsible for enforcing that non-loopback binds require `tls: Some`.
pub struct ServeOpts {
    pub addr: IpAddr,
    pub port: u16,
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

/// Bind `opts.addr:opts.port` and serve until the process exits. Falls back
/// to an ephemeral port (0) if the fixed port is already busy (e.g. a stale
/// `ryuzi serve`) — clients discover the real port from `daemon.json`, never
/// hardcode it.
///
/// Binds a plain [`std::net::TcpListener`] ourselves (rather than going
/// through `axum_server::bind`/`bind_rustls`, which bind internally) so we
/// can read the OS-chosen port via `local_addr()` before handing the
/// listener off — that's what makes the ephemeral-port contract work. The
/// service is built with `ConnectInfo<SocketAddr>` so downstream middleware
/// (peer-IP checks, Phase 3) can see the connecting address. Serving always
/// goes through `axum_server` (for both the TLS and plaintext branches) so
/// there's a single service type regardless of `opts.tls`.
pub async fn serve(state: ApiState, opts: ServeOpts) -> anyhow::Result<u16> {
    let listener = match std::net::TcpListener::bind((opts.addr, opts.port)) {
        Ok(l) => l,
        Err(_) if opts.port != 0 => std::net::TcpListener::bind((opts.addr, 0))?,
        Err(e) => return Err(e.into()),
    };
    listener.set_nonblocking(true)?;
    let bound = listener.local_addr()?.port();
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        let result = match opts.tls {
            Some(cfg) => {
                let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(cfg);
                axum_server::from_tcp_rustls(listener, tls_config)
                    .serve(app)
                    .await
            }
            None => axum_server::from_tcp(listener).serve(app).await,
        };
        if let Err(e) = result {
            tracing::error!("serve: server task exited with error: {e}");
        }
    });
    Ok(bound)
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "ryuzi",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION,
    }))
}

/// `POST /pair` request body: a plaintext pairing code (see
/// `crate::pairing::mint_code`) and a human-readable label for the device
/// being enrolled (stored verbatim in `devices.name`, shown back in device
/// listings — not validated/sanitized here beyond what SQLite's bound
/// parameter already guarantees).
#[derive(Deserialize)]
struct PairRequest {
    code: String,
    device_name: String,
}

/// Fixed-window rate limiter for `POST /pair`, capping it at
/// `PAIR_RATE_LIMIT` requests per rolling `PAIR_RATE_WINDOW_MS`-millisecond
/// window. `/pair` is the one route reachable with no bearer at all (see
/// [`router`]), so it needs its own defense against a code-guessing flood
/// that the `require_token` layer can't provide.
///
/// This is deliberately NOT a field on [`ApiState`]: `ApiState` is
/// constructed at 7+ call sites across `ryuzi-core` (this file's tests,
/// `api/mod.rs`'s test support, `tests/control_api.rs`), `ryuzi-runner`
/// (`daemon_cmd.rs`), and `ryuzi-cockpit` (`engine.rs`, `engine_daemon.rs`).
/// Adding a field there would mean touching every one of those for a
/// concern that only the `/pair` route cares about. Instead, one
/// `PairLimiter` is created per [`router`] call and captured by the
/// `/pair` route's closure (see `router`'s `pair_route`) — scoped to that
/// router/server instance, shared by every request it serves via the
/// `Arc<Mutex<..>>` clones, and touching nothing outside this file.
#[derive(Clone)]
struct PairLimiter(Arc<Mutex<(i64, u32)>>);

/// Requests allowed per window. ~10/min comfortably covers a human retrying
/// a mistyped code a few times, while keeping a brute-force sweep of the
/// 64-hex-char code space computationally irrelevant (the single-use +
/// short-TTL code itself is the real defense; this just caps request
/// throughput).
const PAIR_RATE_LIMIT: u32 = 10;
/// Window length: one minute.
const PAIR_RATE_WINDOW_MS: i64 = 60_000;

impl PairLimiter {
    fn new() -> Self {
        Self(Arc::new(Mutex::new((0, 0))))
    }

    /// `true` iff this request is within budget (and is recorded against
    /// it); `false` once `PAIR_RATE_LIMIT` requests have already landed in
    /// the current window. Fixed-window (not sliding/token-bucket): a new
    /// window starts as soon as `now_ms` has advanced `PAIR_RATE_WINDOW_MS`
    /// or more past the current window's start, at which point the counter
    /// resets to 1. A burst can in principle straddle two adjacent windows
    /// (up to ~2x the nominal rate right at the boundary) — an accepted
    /// simplification for a bootstrap endpoint already guarded by a
    /// short-TTL single-use code.
    fn allow(&self, now_ms: i64) -> bool {
        let mut window = self.0.lock().unwrap();
        if now_ms - window.0 >= PAIR_RATE_WINDOW_MS {
            *window = (now_ms, 1);
            true
        } else if window.1 < PAIR_RATE_LIMIT {
            window.1 += 1;
            true
        } else {
            false
        }
    }
}

/// `POST /pair` handler body. Factored out of the router-registered closure
/// (see `router`'s `pair_route`) so the actual logic is a plain, directly
/// callable async fn rather than living inline in a closure literal;
/// `limiter` is the per-router-instance [`PairLimiter`] that closure
/// captured, NOT part of `ApiState`.
///
/// Rate-limited first (a flood never even reaches `pairing::redeem`, so it
/// can't be used to burn through a target's pairing-code TTL window via
/// sheer request volume); then delegates to `crate::pairing::redeem`, which
/// does the actual single-use/expiry-checked code consumption. `Some(token)`
/// is the device's new bearer token; `None` covers wrong code, already-used
/// code, and expired code alike (see `redeem`'s doc comment for why those
/// are deliberately indistinguishable) — all map to a flat 401.
async fn pair(
    state: ApiState,
    limiter: PairLimiter,
    body: PairRequest,
) -> axum::response::Response {
    if !limiter.allow(crate::paths::now_ms()) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too many pairing attempts, try again shortly" })),
        )
            .into_response();
    }

    match crate::pairing::redeem(
        state.cp.store(),
        &body.code,
        &body.device_name,
        crate::paths::now_ms(),
    )
    .await
    {
        Ok(Some(device_token)) => Json(json!({ "device_token": device_token })).into_response(),
        Ok(None) => (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid or expired pairing code" })),
        )
            .into_response(),
        Err(e) => err(&e),
    }
}

async fn list_sessions(State(state): State<ApiState>) -> impl IntoResponse {
    match state.cp.list_sessions(None).await {
        Ok(sessions) => Json(json!({ "sessions": sessions })).into_response(),
        Err(e) => err(&e),
    }
}

async fn list_messages(State(state): State<ApiState>, Path(pk): Path<String>) -> impl IntoResponse {
    match state.cp.list_messages(&pk).await {
        Ok(messages) => Json(json!({ "messages": messages })).into_response(),
        Err(e) => err(&e),
    }
}

async fn prompt(
    State(state): State<ApiState>,
    Path(pk): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let text = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    match state.cp.continue_session(&pk, text, &[]).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(&e),
    }
}

async fn start(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let text = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    match state.cp.start_session(&id, text, "http", &[]).await {
        Ok(session) => Json(json!({ "session": session })).into_response(),
        Err(e) => err(&e),
    }
}

/// Live SSE stream of core events.
async fn events(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use futures::StreamExt;
    let rx = state.cp.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|ev| async move {
        let ev = ev.ok()?;
        let data = serde_json::to_string(&ev).ok()?;
        Some(Ok(Event::default().data(data)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /plugins` — every installed plugin as a compact summary (identity,
/// categories, verification/experimental flags, computed capabilities, and
/// current enablement). See [`plugin_summary`] and [`CorePlugin::capabilities`].
async fn list_plugins(State(state): State<ApiState>) -> impl IntoResponse {
    let cp = &state.cp;
    let settings = SettingsStore::new(cp.store().clone());
    let mut entries = Vec::new();
    for plugin in cp.plugins().list() {
        match cp
            .plugins()
            .is_enabled(&settings, &plugin.manifest.id)
            .await
        {
            Ok(enabled) => entries.push(plugin_summary(&plugin, enabled)),
            Err(e) => return err(&e),
        }
    }
    Json(entries).into_response()
}

/// `GET /plugins/{id}` — the plugin's full manifest (via `PluginManifest`'s
/// own `Serialize`, so new manifest fields show up automatically) with
/// `enabled` and `source` merged in as extra top-level keys. The manifest
/// carries no secret VALUES (only setting/env key names — see
/// `ryuzi_plugin_sdk::AuthSpec`), so this is safe to return verbatim; do not
/// add settings-value lookups here.
async fn get_plugin(State(state): State<ApiState>, Path(id): Path<String>) -> impl IntoResponse {
    let cp = &state.cp;
    let Some(plugin) = cp.plugins().get(&id) else {
        return not_found(&id);
    };
    let settings = SettingsStore::new(cp.store().clone());
    let enabled = match cp.plugins().is_enabled(&settings, &id).await {
        Ok(enabled) => enabled,
        Err(e) => return err(&e),
    };

    let mut value = match serde_json::to_value(&plugin.manifest) {
        Ok(value) => value,
        Err(e) => return err(&e.into()),
    };
    if let Some(map) = value.as_object_mut() {
        map.insert("enabled".to_string(), json!(enabled));
        map.insert("source".to_string(), json!(source_label(&plugin.source)));
    }
    Json(value).into_response()
}

/// `POST /rpc/{method}` — the generic RPC entry point. `method` is a Rust
/// snake_case command name (see `crate::api::dispatch`); the request body is
/// that command's params object. Errors from `dispatch` are surfaced with
/// the `ApiError`'s own status code, not always 500.
async fn rpc(
    State(state): State<ApiState>,
    Path(method): Path<String>,
    Json(params): Json<Value>,
) -> impl IntoResponse {
    match crate::api::dispatch(&state, &method, params).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            axum::http::StatusCode::from_u16(e.status)
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({ "error": e.message })),
        )
            .into_response(),
    }
}

/// `POST /approvals/{request_id}` — resolve a pending tool-permission
/// approval (see `ApprovalHub`) with body `{"response": ApprovalResponse}`.
/// A missing or malformed `response` leniently denies via
/// `ApprovalResponse::once(false)`. `resolved` is `false` if no approval with
/// this id was pending (already resolved, unknown id, or the request timed
/// out).
async fn resolve_approval_route(
    State(state): State<ApiState>,
    Path(request_id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let response = body
        .get("response")
        .cloned()
        .and_then(|v| serde_json::from_value::<crate::domain::ApprovalResponse>(v).ok())
        .unwrap_or_else(|| crate::domain::ApprovalResponse::once(false));
    let resolved = state.cp.resolve_approval(&request_id, response);
    Json(json!({ "resolved": resolved })).into_response()
}

/// The `{id, name, description, categories, verified, experimental, enabled,
/// capabilities}` shape `GET /plugins` returns for one plugin.
fn plugin_summary(plugin: &CorePlugin, enabled: bool) -> Value {
    let m = &plugin.manifest;
    json!({
        "id": m.id,
        "name": m.name,
        "description": m.description,
        "categories": m.categories,
        "verified": m.verified,
        "experimental": m.experimental,
        "enabled": enabled,
        "capabilities": plugin.capabilities(),
    })
}

fn source_label(source: &PluginSource) -> &'static str {
    match source {
        PluginSource::Builtin => "builtin",
        PluginSource::Catalog => "catalog",
        PluginSource::SkillPack(_) => "skill-pack",
    }
}

fn not_found(id: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("unknown plugin: {id}") })),
    )
        .into_response()
}

fn err(e: &anyhow::Error) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::{Connector, ConnectorCtx};
    use crate::domain::McpServerSpec;
    use crate::plugins::{CorePlugin, PluginSource, Registries};
    use async_trait::async_trait;
    use ryuzi_plugin_sdk::PluginManifest;
    use std::net::Ipv4Addr;

    /// Plaintext-loopback `ServeOpts` for tests that don't exercise TLS.
    fn opts(port: u16) -> ServeOpts {
        ServeOpts {
            addr: Ipv4Addr::LOCALHOST.into(),
            port,
            tls: None,
        }
    }

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        ControlPlane::new(store, Registries::new()).await
    }

    /// The control token every test `ApiState` uses — there is no
    /// auth-disable mode, so every test needs a real one.
    const TEST_CONTROL_TOKEN: &str = "sekrit";

    /// `ApiState` wrapping `cp` with the shared test control token. Used
    /// both by tests that don't exercise the bearer-token middleware at all
    /// (e.g. `serve_binds_an_ephemeral_port`) and by tests that hit `/health`
    /// only (public) — as well as ones that authenticate explicitly.
    fn state_for(cp: Arc<ControlPlane>) -> ApiState {
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            control_token: TEST_CONTROL_TOKEN.to_string(),
        }
    }

    async fn test_state() -> ApiState {
        state_for(test_cp().await)
    }

    /// A connector that contributes no MCP servers — enough to exercise the
    /// connector-only branch of `PluginHost::is_enabled` (`plugin.<id>.
    /// enabled`, defaulting to `false`) without depending on a real
    /// integration.
    struct NoopConnector;

    #[async_trait]
    impl Connector for NoopConnector {
        async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![])
        }
    }

    fn minimal_manifest(id: &str, name: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: name.to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            provider: None,
        }
    }

    /// Every model-provider/CLI-agent builtin (via `install_builtins`, which
    /// includes the `anthropic` provider) plus one connector-only test
    /// plugin so `/plugins`' enabled-by-default-false branch has something to
    /// exercise.
    async fn test_cp_with_plugins() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let mut regs = Registries::new();
        crate::plugins::install_builtins(&mut regs);
        regs.add_plugin(CorePlugin {
            manifest: minimal_manifest("acme-test-connector", "Acme Test Connector"),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(NoopConnector)),
            source: PluginSource::Builtin,
        });
        ControlPlane::new(store, regs).await
    }

    #[tokio::test]
    async fn health_reports_ok() {
        let cp = test_cp().await;
        let Json(v) = health().await;
        assert_eq!(v["status"], "ok");
        assert_eq!(v["service"], "ryuzi");
        // Router builds without panicking.
        let _ = router(state_for(cp));
    }

    #[tokio::test]
    async fn serve_binds_an_ephemeral_port() {
        let cp = test_cp().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();
        assert!(port > 0);
    }

    #[tokio::test]
    async fn list_plugins_shows_anthropic_enabled_with_provider_capability() {
        let cp = test_cp_with_plugins().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();

        let body: Vec<Value> = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/plugins"))
            .bearer_auth(TEST_CONTROL_TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let anthropic = body
            .iter()
            .find(|p| p["id"] == "anthropic")
            .expect("anthropic plugin present in /plugins");
        assert_eq!(anthropic["name"], "Anthropic");
        assert_eq!(anthropic["enabled"], true);
        assert_eq!(anthropic["capabilities"], json!(["provider"]));
    }

    #[tokio::test]
    async fn get_plugin_returns_manifest_fields_plus_enabled_and_source() {
        let cp = test_cp_with_plugins().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();

        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/plugins/anthropic"))
            .bearer_auth(TEST_CONTROL_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();

        assert_eq!(body["id"], "anthropic");
        assert_eq!(body["contract"], 1);
        assert_eq!(body["provider"]["format"], "anthropic");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["source"], "builtin");
    }

    #[tokio::test]
    async fn unknown_plugin_id_is_404_with_error_envelope() {
        let cp = test_cp_with_plugins().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();

        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/plugins/nope"))
            .bearer_auth(TEST_CONTROL_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "unknown plugin: nope");
    }

    #[tokio::test]
    async fn connector_only_plugin_is_disabled_until_setting_flips_true() {
        let cp = test_cp_with_plugins().await;
        // Keep a handle to write the setting directly after the server (which
        // consumes an `Arc<ControlPlane>` into its router state) is started.
        let store = cp.store().clone();
        let port = serve(state_for(cp), opts(0)).await.unwrap();

        let fetch = || {
            let url = format!("http://127.0.0.1:{port}/plugins");
            async move {
                reqwest::Client::new()
                    .get(url)
                    .bearer_auth(TEST_CONTROL_TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .json::<Vec<Value>>()
                    .await
                    .unwrap()
            }
        };

        let before = fetch().await;
        let entry = before
            .iter()
            .find(|p| p["id"] == "acme-test-connector")
            .expect("connector-only test plugin present");
        assert_eq!(entry["enabled"], false);
        assert_eq!(entry["capabilities"], json!(["connector"]));

        store
            .set_setting_raw("plugin.acme-test-connector.enabled", "true")
            .await
            .unwrap();

        let after = fetch().await;
        let entry = after
            .iter()
            .find(|p| p["id"] == "acme-test-connector")
            .unwrap();
        assert_eq!(entry["enabled"], true);
    }

    #[tokio::test]
    async fn authed_routes_reject_missing_or_wrong_token() {
        let state = test_state().await;
        let port = serve(state, opts(0)).await.unwrap();
        let client = reqwest::Client::new();

        let r = client
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

        let r = client
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

        let r = client
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .bearer_auth("sekrit")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn health_needs_no_token_and_reports_version() {
        let state = test_state().await;
        let port = serve(state, opts(0)).await.unwrap();
        let v: Value = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["protocol_version"], 1);
    }

    #[tokio::test]
    async fn busy_port_falls_back_to_ephemeral() {
        let state = test_state().await;
        let first = serve(state.clone(), opts(0)).await.unwrap();
        // Ask for the port that's now busy — must succeed on a different one.
        let second = serve(test_state().await, opts(first)).await.unwrap();
        assert_ne!(first, second);
    }

    /// Builds a real ring-backed `rustls::ServerConfig` from a self-signed
    /// `TlsMaterial` — same construction `tls::pair_is_valid` uses internally
    /// to validate a cert/key pair, duplicated here (that helper is private)
    /// so this test can hand a genuine `Arc<ServerConfig>` to `ServeOpts`.
    fn ring_server_config(material: &crate::tls::TlsMaterial) -> Arc<rustls::ServerConfig> {
        let cert = rustls::pki_types::CertificateDer::from(material.cert_der.clone());
        let key = rustls::pki_types::PrivateKeyDer::try_from(material.key_der.clone())
            .expect("valid private key DER");
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("self-signed cert/key pair builds a ServerConfig");
        Arc::new(cfg)
    }

    /// `serve` over TLS binds and returns a real port without panicking, on
    /// the ephemeral-port path AND with a genuine `Arc<rustls::ServerConfig>`
    /// wired in. A full TLS handshake round-trip is P2-9; this just proves
    /// the `axum_server::from_tcp_rustls` branch stands up the listener and
    /// the ephemeral-port contract still holds when `tls: Some`.
    #[tokio::test]
    async fn serve_binds_with_tls_config() {
        let dir = tempfile::tempdir().unwrap();
        let material = crate::tls::load_or_generate(dir.path()).unwrap();
        let tls_cfg = ring_server_config(&material);

        let cp = test_cp().await;
        let port = serve(
            state_for(cp),
            ServeOpts {
                addr: Ipv4Addr::LOCALHOST.into(),
                port: 0,
                tls: Some(tls_cfg),
            },
        )
        .await
        .unwrap();
        assert!(port > 0);
    }

    // ---- P2-5: two-tier auth (device tokens + loopback control token) ----

    /// A minimal non-revoked `Device` row for `authorize` unit tests — the
    /// exact field values don't matter, only that it's `Some`.
    fn fake_device() -> Device {
        Device {
            id: "dev-1".to_string(),
            name: "test-device".to_string(),
            created_at: 0,
            last_seen: None,
            revoked: false,
        }
    }

    #[test]
    fn authorize_allows_loopback_peer_with_valid_control_token() {
        assert!(authorize(
            true,
            "the-control-token",
            "the-control-token",
            None
        ));
    }

    #[test]
    fn authorize_rejects_control_token_from_non_loopback_peer() {
        // The whole point of the two-tier scheme: the control token must
        // never authenticate a remote peer, even with the exact right value.
        assert!(!authorize(
            false,
            "the-control-token",
            "the-control-token",
            None
        ));
    }

    #[test]
    fn authorize_allows_a_resolved_device_from_any_peer() {
        let device = fake_device();
        // Loopback...
        assert!(authorize(
            true,
            "device-secret",
            "the-control-token",
            Some(&device)
        ));
        // ...and non-loopback: device tokens work from anywhere.
        assert!(authorize(
            false,
            "device-secret",
            "the-control-token",
            Some(&device)
        ));
    }

    #[test]
    fn authorize_rejects_unknown_bearer() {
        assert!(!authorize(true, "nope", "the-control-token", None));
        assert!(!authorize(false, "nope", "the-control-token", None));
    }

    /// A bearer whose SHA-256 hash matches a non-revoked `devices` row
    /// authenticates via the real middleware end-to-end (not just the pure
    /// `authorize` decision), from the loopback peer a `serve()`-bound test
    /// server always presents.
    #[tokio::test]
    async fn device_token_authenticates_through_the_real_middleware() {
        let cp = test_cp().await;
        let store = cp.store().clone();
        let raw_token = "device-secret-abc";
        store
            .insert_device(
                "dev-1",
                "test-device",
                &crate::update::asset::sha256_hex(raw_token.as_bytes()),
            )
            .await
            .unwrap();

        let port = serve(state_for(cp), opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .bearer_auth(raw_token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
    }

    /// A revoked device's token must no longer authenticate — even though it
    /// once did, and even though `find_device_by_token_hash` already filters
    /// revoked rows at the store layer, this exercises that guarantee
    /// through the full middleware, not just `store.rs`'s own tests.
    #[tokio::test]
    async fn revoked_device_token_is_rejected_by_the_real_middleware() {
        let cp = test_cp().await;
        let store = cp.store().clone();
        let raw_token = "device-secret-xyz";
        store
            .insert_device(
                "dev-2",
                "test-device-2",
                &crate::update::asset::sha256_hex(raw_token.as_bytes()),
            )
            .await
            .unwrap();
        store.revoke_device("dev-2").await.unwrap();

        let port = serve(state_for(cp), opts(0)).await.unwrap();
        let r = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .bearer_auth(raw_token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    // ---- P2-6: POST /pair (public bootstrap route + rate limit) ----

    /// A pre-seeded, valid pairing code redeems a `device_token` with no
    /// bearer at all (proving the route really is public, not just
    /// reachable with a control-token-less request that happens to also
    /// work), and the SAME code is rejected the second time — single-use,
    /// enforced end-to-end through the real HTTP route, not just
    /// `pairing::redeem`'s own unit tests.
    #[tokio::test]
    async fn pair_redeems_a_seeded_code_and_rejects_reuse() {
        let cp = test_cp().await;
        let store = cp.store().clone();
        let now = crate::paths::now_ms();
        let code = crate::pairing::mint_code(&store, 60_000, now)
            .await
            .unwrap();

        let port = serve(state_for(cp), opts(0)).await.unwrap();
        let client = reqwest::Client::new();

        let r = client
            .post(format!("http://127.0.0.1:{port}/pair"))
            .json(&json!({ "code": code, "device_name": "alfin-laptop" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);
        let body: Value = r.json().await.unwrap();
        let token = body["device_token"]
            .as_str()
            .expect("device_token present on success");
        assert_eq!(token.len(), 64);

        // The device the pairing just created authenticates on the authed
        // API, using the token /pair just handed back.
        let r = client
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::OK);

        // Same code again: already consumed.
        let r = client
            .post(format!("http://127.0.0.1:{port}/pair"))
            .json(&json!({ "code": code, "device_name": "alfin-laptop" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// An unknown/wrong code is a flat 401, with no bearer token presented
    /// at all — confirms `/pair` sits outside `require_token`'s layer.
    #[tokio::test]
    async fn pair_rejects_an_unknown_code_with_no_bearer_needed() {
        let cp = test_cp().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();

        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/pair"))
            .json(&json!({ "code": "not-a-real-code", "device_name": "some-device" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// The fixed-window limiter allows exactly `PAIR_RATE_LIMIT` requests
    /// and rejects the next one with 429 — exercised through the real route
    /// (each request uses a bad code, so every allowed request is itself a
    /// 401; the 11th is a 429 from the limiter before `pairing::redeem` is
    /// even called).
    #[tokio::test]
    async fn pair_rate_limits_after_ten_requests_in_the_window() {
        let cp = test_cp().await;
        let port = serve(state_for(cp), opts(0)).await.unwrap();
        let client = reqwest::Client::new();
        let body = json!({ "code": "nope", "device_name": "flooder" });

        for i in 0..PAIR_RATE_LIMIT {
            let r = client
                .post(format!("http://127.0.0.1:{port}/pair"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(
                r.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "request {i} should still be within the rate-limit budget"
            );
        }

        let r = client
            .post(format!("http://127.0.0.1:{port}/pair"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn pair_limiter_resets_after_the_window_elapses() {
        let limiter = PairLimiter::new();
        let start = 1_700_000_000_000_i64;
        for _ in 0..PAIR_RATE_LIMIT {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start), "budget exhausted within the window");

        // A new window: the budget is back.
        assert!(limiter.allow(start + PAIR_RATE_WINDOW_MS));
    }
}
