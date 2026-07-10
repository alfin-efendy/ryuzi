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
use std::collections::HashMap;
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
        .route("/plugins/doctor", get(plugins_doctor))
        .route("/plugins/install", post(install_plugin))
        .route("/plugins/install/confirm", post(confirm_plugin_install))
        .route("/plugins/update-all", post(update_all_plugins))
        .route(
            "/plugins/{id}",
            get(get_plugin).delete(uninstall_plugin_route),
        )
        .route("/plugins/{id}/update", post(update_plugin_route))
        .route("/plugins/{id}/pin", post(pin_plugin_route))
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
/// current enablement), enriched with its `plugin_installs`/
/// `plugin_attach_status` ledger rows (when present) and the daemon-wide
/// `restartRequired` flag. See [`plugin_summary`], [`merge_install_record`],
/// [`merge_attach_status`], and [`CorePlugin::capabilities`].
///
/// The install/attach ledgers are fetched exactly once (`list_plugin_installs`/
/// `list_plugin_attach`) and indexed by plugin id, rather than queried per
/// plugin, so this stays O(1) round-trips regardless of the plugin count.
async fn list_plugins(State(cp): State<Arc<ControlPlane>>) -> impl IntoResponse {
    let settings = SettingsStore::new(cp.store().clone());
    let installs: HashMap<String, crate::store::PluginInstallRecord> =
        match cp.store().list_plugin_installs().await {
            Ok(rows) => rows.into_iter().map(|r| (r.plugin_id.clone(), r)).collect(),
            Err(e) => return err(&e),
        };
    let attach: HashMap<String, crate::store::PluginAttachStatus> =
        match cp.store().list_plugin_attach().await {
            Ok(rows) => rows.into_iter().map(|r| (r.plugin_id.clone(), r)).collect(),
            Err(e) => return err(&e),
        };
    let restart_required = cp.plugins_restart_required();

    let mut entries = Vec::new();
    for plugin in cp.plugins().list() {
        match cp
            .plugins()
            .is_enabled(&settings, &plugin.manifest.id)
            .await
        {
            Ok(enabled) => {
                let mut value = plugin_summary(&plugin, enabled);
                if let Some(map) = value.as_object_mut() {
                    map.insert("restartRequired".to_string(), json!(restart_required));
                    if let Some(rec) = installs.get(&plugin.manifest.id) {
                        merge_install_record(map, rec);
                    }
                    if let Some(status) = attach.get(&plugin.manifest.id) {
                        merge_attach_status(map, status);
                    }
                }
                entries.push(value);
            }
            Err(e) => return err(&e),
        }
    }
    Json(entries).into_response()
}

/// `GET /plugins/{id}` — the plugin's full manifest (via `PluginManifest`'s
/// own `Serialize`, so new manifest fields show up automatically) with
/// `enabled`, `source`, and `restartRequired` merged in as extra top-level
/// keys, plus its `plugin_installs`/`plugin_attach_status` ledger rows (when
/// present — see [`merge_install_record`], [`merge_attach_status`]). The
/// manifest carries no secret VALUES (only setting/env key names — see
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
        map.insert(
            "restartRequired".to_string(),
            json!(cp.plugins_restart_required()),
        );
        match cp.store().get_plugin_install(&id).await {
            Ok(Some(rec)) => merge_install_record(map, &rec),
            Ok(None) => {}
            Err(e) => return err(&e),
        }
        match cp.store().get_plugin_attach(&id).await {
            Ok(Some(status)) => merge_attach_status(map, &status),
            Ok(None) => {}
            Err(e) => return err(&e),
        }
    }
    Json(value).into_response()
}

/// Merge a `plugin_installs` ledger row into a plugin's JSON summary/manifest
/// object. The record's origin lands under the DISTINCT `sourceSpec` key (a
/// git URL / source spec), deliberately NOT the existing `source` field —
/// `source` stays the stable [`source_label`] enum tag (`"builtin" |
/// "catalog" | "skill-pack"`) so consumers matching on those labels keep
/// working even once a plugin has a ledger row.
fn merge_install_record(
    map: &mut serde_json::Map<String, Value>,
    rec: &crate::store::PluginInstallRecord,
) {
    map.insert("sourceSpec".to_string(), json!(rec.source_spec));
    map.insert("resolvedCommit".to_string(), json!(rec.resolved_commit));
    map.insert("pinned".to_string(), json!(rec.pinned));
    map.insert("installedAt".to_string(), json!(rec.installed_at));
    map.insert("updatedAt".to_string(), json!(rec.updated_at));
    map.insert("trustTier".to_string(), json!(rec.trust_tier));
}

/// Merge a `plugin_attach_status` ledger row into a plugin's JSON summary/
/// manifest object.
fn merge_attach_status(
    map: &mut serde_json::Map<String, Value>,
    status: &crate::store::PluginAttachStatus,
) {
    map.insert("attachOutcome".to_string(), json!(status.outcome));
    map.insert("attachReason".to_string(), json!(status.reason));
}

/// `GET /plugins/doctor` — read-only aggregation of plugin health findings
/// (unconfigured/reconnect-required/missing-binary/attach-failed). Never
/// mutates state; see `crate::plugins::doctor::plugin_doctor`.
async fn plugins_doctor(State(cp): State<Arc<ControlPlane>>) -> impl IntoResponse {
    match crate::plugins::doctor::plugin_doctor(&cp).await {
        Ok(findings) => Json(findings).into_response(),
        Err(e) => err(&e),
    }
}

/// `POST /plugins/install` `{source}` — phase 1 of the two-phase tiered trust
/// gate. Curated sources install immediately; arbitrary sources stop at a
/// `TrustPrompt` for the caller to show the user before `confirm_install` can
/// proceed. Marks the daemon dirty (`restartRequired`) only when the install
/// actually completed.
async fn install_plugin(
    State(cp): State<Arc<ControlPlane>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let source = body
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    match crate::skills_install::begin_install(&source, cp.store()).await {
        Ok(crate::skills_install::BeginInstall::Completed(pack)) => {
            cp.mark_plugins_restart_required();
            Json(json!({ "completed": true, "plugin": pack })).into_response()
        }
        Ok(crate::skills_install::BeginInstall::NeedsConfirmation(trust)) => {
            Json(json!({ "completed": false, "trust": trust })).into_response()
        }
        Err(e) => err(&e),
    }
}

/// `POST /plugins/install/confirm` `{token}` — phase 2 of the trust gate:
/// completes a staged install (or update) after the user has acknowledged
/// its `TrustPrompt`.
async fn confirm_plugin_install(
    State(cp): State<Arc<ControlPlane>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    match crate::skills_install::confirm_install(&token, cp.store()).await {
        Ok(pack) => {
            cp.mark_plugins_restart_required();
            Json(json!({ "plugin": pack })).into_response()
        }
        Err(e) => err(&e),
    }
}

/// `POST /plugins/{id}/update` `{force?}` — update one installed pack to its
/// latest upstream commit. See `UpdateOutcome` for the full set of results
/// (including `NeedsReack`, which routes back through `confirm_install`).
async fn update_plugin_route(
    State(cp): State<Arc<ControlPlane>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    match crate::skills_install::update_installed_pack(&id, force, cp.store()).await {
        Ok(outcome) => {
            // Only an actual reinstall changes what's on disk / loaded;
            // AlreadyCurrent/SkippedPinned/LocalEdits/NeedsReack are no-ops.
            if matches!(outcome, crate::skills_install::UpdateOutcome::Updated) {
                cp.mark_plugins_restart_required();
            }
            Json(outcome).into_response()
        }
        Err(e) => err(&e),
    }
}

/// `POST /plugins/update-all` — update every installed pack, skipping pinned
/// ones. Never fails as a whole: a single pack's error becomes an
/// `UpdateOutcome::Failed` entry so the rest of the batch still runs.
async fn update_all_plugins(State(cp): State<Arc<ControlPlane>>) -> impl IntoResponse {
    match crate::skills_install::update_all_packs(cp.store()).await {
        Ok(list) => {
            // Only mark dirty if at least one pack actually reinstalled.
            if list
                .iter()
                .any(|(_, o)| matches!(o, crate::skills_install::UpdateOutcome::Updated))
            {
                cp.mark_plugins_restart_required();
            }
            Json(
                list.into_iter()
                    .map(|(id, outcome)| json!({ "id": id, "outcome": outcome }))
                    .collect::<Vec<_>>(),
            )
            .into_response()
        }
        Err(e) => err(&e),
    }
}

/// `POST /plugins/{id}/pin` `{pinned, reason?}` — pin (or unpin) an installed
/// pack against future updates. Does not require a restart — pin state does
/// not change what is on disk or loaded in-process.
async fn pin_plugin_route(
    State(cp): State<Arc<ControlPlane>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let pinned = body
        .get("pinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = body.get("reason").and_then(|v| v.as_str());
    match crate::skills_install::set_pack_pin(&id, pinned, reason, cp.store()).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(&e),
    }
}

/// `DELETE /plugins/{id}` — uninstall a recorded skill pack: removes it from
/// disk and deletes its `plugin_installs`/`plugin_attach_status` ledger rows.
async fn uninstall_plugin_route(
    State(cp): State<Arc<ControlPlane>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match crate::skills_install::remove_installed_skill_recorded(&id, cp.store()).await {
        Ok(()) => {
            cp.mark_plugins_restart_required();
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => err(&e),
    }
}

/// The `{id, name, description, categories, verified, experimental, enabled,
/// source, capabilities}` shape `GET /plugins` returns for one plugin.
/// `source` is the stable [`source_label`] enum tag (`"builtin" | "catalog" |
/// "skill-pack"`); a plugin's git origin, when it has a ledger row, is added
/// separately under `sourceSpec` (see [`merge_install_record`]).
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
        "source": source_label(&plugin.source),
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
            runtime: None,
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
    async fn install_confirm_and_doctor_routes_exist() {
        let cp = test_cp_with_plugins().await;
        let port = serve(cp, 0).await.unwrap();
        // doctor returns a JSON array (possibly empty) with 200.
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/plugins/doctor"))
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let _: Vec<serde_json::Value> = resp.json().await.unwrap();
        // update-all on a fresh DB returns an empty outcome list.
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/plugins/update-all"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let outcomes: Vec<serde_json::Value> = resp.json().await.unwrap();
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn get_plugin_enrichment_keeps_source_enum_and_adds_source_spec() {
        // Seed a plugin_installs ledger row for a builtin id, then confirm the
        // enrichment adds record fields under DISTINCT keys and leaves the
        // stable `source` enum label untouched (regression guard for the
        // `source`/`sourceSpec` collision).
        let cp = test_cp_with_plugins().await;
        let store = cp.store().clone();
        store
            .upsert_plugin_install(&crate::store::PluginInstallRecord {
                plugin_id: "anthropic".to_string(),
                kind: "plugin_pack".to_string(),
                source_spec: "https://github.com/acme/anthropic-pack".to_string(),
                resolved_commit: Some("abc123".to_string()),
                fingerprint: "sha256:deadbeef".to_string(),
                installed_at: 1_700_000_000,
                updated_at: 1_700_000_500,
                pinned: false,
                pin_reason: None,
                trust_tier: "acknowledged".to_string(),
                trust_ack_at: Some(1_700_000_000),
                trust_ack_summary: None,
            })
            .await
            .unwrap();

        let port = serve(cp, 0).await.unwrap();
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/plugins/anthropic"))
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();

        // `source` stays the enum label — NOT overwritten by the git spec.
        assert_eq!(body["source"], "builtin");
        // The record's origin lands under the distinct `sourceSpec` key.
        assert_eq!(body["sourceSpec"], "https://github.com/acme/anthropic-pack");
        assert_eq!(body["trustTier"], "acknowledged");
        assert_eq!(body["installedAt"], 1_700_000_000_i64);
        assert_eq!(body["resolvedCommit"], "abc123");
        assert_eq!(body["pinned"], false);

        // The same enrichment is visible in the LIST payload's entry.
        let list: Vec<Value> = reqwest::get(format!("http://127.0.0.1:{port}/plugins"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let entry = list
            .iter()
            .find(|p| p["id"] == "anthropic")
            .expect("anthropic present in /plugins");
        assert_eq!(entry["source"], "builtin");
        assert_eq!(
            entry["sourceSpec"],
            "https://github.com/acme/anthropic-pack"
        );
        assert_eq!(entry["trustTier"], "acknowledged");
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
