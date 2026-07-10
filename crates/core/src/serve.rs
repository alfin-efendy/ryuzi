//! A minimal HTTP surface over the embedded [`ControlPlane`], mirroring
//! opencode's `serve`. Exposes session listing, transcript, prompt, and a live
//! Server-Sent-Events stream of [`CoreEvent`]s so external clients (or a remote
//! `attach`) can drive and observe sessions.

use crate::control::ControlPlane;
use crate::plugins::{CorePlugin, PluginSource};
use crate::settings::SettingsStore;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;

/// Build the HTTP router over a control plane.
pub fn router(cp: Arc<ControlPlane>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{pk}/messages", get(list_messages))
        .route("/sessions/{pk}/prompt", post(prompt))
        .route("/projects/{id}/session", post(start))
        .route("/events", get(events))
        .route("/plugins", get(list_plugins))
        .route("/plugins/{id}", get(get_plugin))
        .with_state(cp)
}

/// Bind `127.0.0.1:port` and serve until the process exits.
pub async fn serve(cp: Arc<ControlPlane>, port: u16) -> anyhow::Result<u16> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let bound = listener.local_addr()?.port();
    let app = router(cp);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(bound)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "ryuzi", "version": env!("CARGO_PKG_VERSION") }))
}

async fn list_sessions(State(cp): State<Arc<ControlPlane>>) -> impl IntoResponse {
    match cp.list_sessions(None).await {
        Ok(sessions) => Json(json!({ "sessions": sessions })).into_response(),
        Err(e) => err(&e),
    }
}

async fn list_messages(
    State(cp): State<Arc<ControlPlane>>,
    Path(pk): Path<String>,
) -> impl IntoResponse {
    match cp.list_messages(&pk).await {
        Ok(messages) => Json(json!({ "messages": messages })).into_response(),
        Err(e) => err(&e),
    }
}

async fn prompt(
    State(cp): State<Arc<ControlPlane>>,
    Path(pk): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let text = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    match cp.continue_session(&pk, text, &[]).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(&e),
    }
}

async fn start(
    State(cp): State<Arc<ControlPlane>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let text = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    match cp.start_session(&id, text, "http", &[]).await {
        Ok(session) => Json(json!({ "session": session })).into_response(),
        Err(e) => err(&e),
    }
}

/// Live SSE stream of core events.
async fn events(
    State(cp): State<Arc<ControlPlane>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use futures::StreamExt;
    let rx = cp.subscribe();
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
async fn list_plugins(State(cp): State<Arc<ControlPlane>>) -> impl IntoResponse {
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
async fn get_plugin(
    State(cp): State<Arc<ControlPlane>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        ControlPlane::new(store, Registries::new()).await
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
        let _ = router(cp);
    }

    #[tokio::test]
    async fn serve_binds_an_ephemeral_port() {
        let cp = test_cp().await;
        let port = serve(cp, 0).await.unwrap();
        assert!(port > 0);
    }

    #[tokio::test]
    async fn list_plugins_shows_anthropic_enabled_with_provider_capability() {
        let cp = test_cp_with_plugins().await;
        let port = serve(cp, 0).await.unwrap();

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
        let port = serve(cp, 0).await.unwrap();

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
        let port = serve(cp, 0).await.unwrap();

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
        let port = serve(cp, 0).await.unwrap();

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
}
