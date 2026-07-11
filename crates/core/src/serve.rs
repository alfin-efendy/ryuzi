//! A minimal HTTP surface over the embedded [`ControlPlane`], mirroring
//! opencode's `serve`. Exposes session listing, transcript, prompt, and a live
//! Server-Sent-Events stream of [`CoreEvent`]s so external clients (or a remote
//! `attach`) can drive and observe sessions.

use crate::control::ControlPlane;
use crate::llm_router::server::RouterServer;
use crate::plugins::{CorePlugin, PluginSource};
use crate::settings::SettingsStore;
use axum::extract::{Path, State};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;

/// Shared state for the control API router.
#[derive(Clone)]
pub struct ApiState {
    pub cp: Arc<ControlPlane>,
    pub router_server: Arc<RouterServer>,
    /// `None` disables auth (tests, legacy embedded serve).
    pub token: Option<String>,
}

/// Build the HTTP router over a control plane.
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
    Router::new()
        .route("/health", get(health))
        .merge(authed)
        .with_state(state)
}

/// Reject requests without a valid `Authorization: Bearer <token>` header.
/// Never applied to `GET /health`. A `None` token (tests, legacy embedded
/// serve) disables auth entirely.
async fn require_token(
    State(state): State<ApiState>,
    req: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let Some(expected) = &state.token else {
        return next.run(req).await; // auth disabled
    };
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(p) if crate::control_token::verify(p, expected) => next.run(req).await,
        _ => (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or invalid bearer token" })),
        )
            .into_response(),
    }
}

/// Bind `127.0.0.1:port` and serve until the process exits. Falls back to an
/// ephemeral port (0) if the fixed port is already busy (e.g. a stale
/// `ryuzi serve`) — clients discover the real port from `daemon.json`, never
/// hardcode it.
pub async fn serve(state: ApiState, port: u16) -> anyhow::Result<u16> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(_) if port != 0 => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
        Err(e) => return Err(e.into()),
    };
    let bound = listener.local_addr()?.port();
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(bound)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "ryuzi", "version": env!("CARGO_PKG_VERSION") }))
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
        PluginSource::Catalog | PluginSource::RemoteCatalog => "catalog",
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

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        ControlPlane::new(store, Registries::new()).await
    }

    /// Auth-disabled `ApiState` for pre-existing tests that don't exercise
    /// the bearer-token middleware.
    fn no_auth_state(cp: Arc<ControlPlane>) -> ApiState {
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            token: None,
        }
    }

    async fn test_state() -> ApiState {
        let cp = test_cp().await;
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            token: Some("sekrit".to_string()),
        }
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
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
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
            extension: None,
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
        let _ = router(no_auth_state(cp));
    }

    #[tokio::test]
    async fn serve_binds_an_ephemeral_port() {
        let cp = test_cp().await;
        let port = serve(no_auth_state(cp), 0).await.unwrap();
        assert!(port > 0);
    }

    #[tokio::test]
    async fn list_plugins_shows_anthropic_enabled_with_provider_capability() {
        let cp = test_cp_with_plugins().await;
        let port = serve(no_auth_state(cp), 0).await.unwrap();

        let body: Vec<Value> = reqwest::get(format!("http://127.0.0.1:{port}/plugins"))
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
        let port = serve(no_auth_state(cp), 0).await.unwrap();

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/plugins/anthropic"))
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
        let port = serve(no_auth_state(cp), 0).await.unwrap();

        let resp = reqwest::get(format!("http://127.0.0.1:{port}/plugins/nope"))
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
        let port = serve(no_auth_state(cp), 0).await.unwrap();

        let fetch = || {
            let url = format!("http://127.0.0.1:{port}/plugins");
            async move {
                reqwest::get(url)
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
        let port = serve(state, 0).await.unwrap();
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
        let port = serve(state, 0).await.unwrap();
        let v: Value = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn busy_port_falls_back_to_ephemeral() {
        let state = test_state().await;
        let first = serve(state.clone(), 0).await.unwrap();
        // Ask for the port that's now busy — must succeed on a different one.
        let second = serve(test_state().await, first).await.unwrap();
        assert_ne!(first, second);
    }
}
