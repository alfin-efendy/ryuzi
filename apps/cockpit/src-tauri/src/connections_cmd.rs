//! Providers tab commands: catalog + thin proxies to the engine daemon's
//! connections RPC family (credentialed connections CRUD, test, OAuth /
//! device-flow sign-in). `list_provider_catalog` stays local (static
//! catalog, no engine round-trip needed) and so does `begin_oauth_manual`
//! (keeps its `AppHandle` + system-browser open — it never touches the
//! store, so there's nothing to proxy).
//!
//! Behavior change from the pre-daemon version: `connect_oauth` /
//! `reconnect_oauth` no longer take an `AppHandle` or open the system
//! browser directly — the engine broadcasts `CoreEvent::OauthAuthorizeUrl`
//! over SSE, and the per-runner bridge (`engine_manager::spawn_bridge`'s
//! `oauthAuthorizeUrl` arm) opens the browser on receipt.
//! `start_kiro_device_flow` / `start_device_flow` open the browser AFTER the
//! RPC returns, using the verification URL in the returned `DeviceFlowInfo`.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::llm_router::oauth;
use ryuzi_core::llm_router::quota::{CodexResetCreditResult, ProviderQuotaInfo};
use ryuzi_core::llm_router::registry::{self, ApiFormat, ProviderCategory};
use ryuzi_core::llm_router::routes::{
    ModelRouteInfo, ModelRouteStrategy, ModelRouteTargetCapability, ProviderAccountRouteInfo,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

pub use ryuzi_core::api::types::{
    ConnectionInfo, DeviceFlowInfo, ManualStartInfo, ModelStatusEntry, ModelStatusInfo,
    RefreshModelsResult, TestResult,
};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    /// Vendor family id (a catalog id). Entries sharing a family render as one
    /// provider card; the entry whose id == family is the display head.
    pub family: String,
    pub color: String,
    pub initial: String,
    pub category: String,
    pub format: String,
    pub requires_base_url: bool,
    pub models: Vec<String>,
    pub free_tier: bool,
    pub risk_notice: bool,
    pub uses_device_grant: bool,
}

#[tauri::command]
#[specta::specta]
pub async fn list_provider_catalog() -> R<Vec<CatalogEntry>> {
    Ok(registry::CATALOG
        .iter()
        .map(|d| CatalogEntry {
            id: d.id.into(),
            name: d.name.into(),
            family: d.family.into(),
            color: d.color.into(),
            initial: d.initial.into(),
            category: if d.device_flow.is_some() {
                "device".into()
            } else {
                match d.category {
                    ProviderCategory::ApiKey => "api_key".into(),
                    ProviderCategory::OAuth => "oauth".into(),
                    ProviderCategory::Free => "free".into(),
                }
            },
            format: match d.format {
                ApiFormat::Anthropic => "anthropic".into(),
                ApiFormat::OpenAi => "openai".into(),
            },
            requires_base_url: d.requires_base_url,
            models: d.models.iter().map(|s| s.to_string()).collect(),
            free_tier: d.free_tier,
            risk_notice: d.risk_notice,
            uses_device_grant: d.device_grant.is_some(),
        })
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn list_connections(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_connections", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn add_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    label: String,
    api_key: String,
    base_url: Option<String>,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "add_connection",
            serde_json::json!({
                "provider": provider, "label": label,
                "api_key": api_key, "base_url": base_url,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn rename_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "rename_connection",
            serde_json::json!({ "id": id, "label": label }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_connection_enabled(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    enabled: bool,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_connection_enabled",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("remove_connection", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn move_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    dir: i32,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "move_connection",
            serde_json::json!({ "id": id, "dir": dir }),
        )
        .await
}

/// Hit the upstream's model-list endpoint to distinguish bad credentials
/// (401/403) from network trouble, and persist the discovered model ids.
#[tauri::command]
#[specta::specta]
pub async fn test_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<TestResult> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("test_connection", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn test_connection_model(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    model: String,
) -> R<TestResult> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "test_connection_model",
            serde_json::json!({ "id": id, "model": model }),
        )
        .await
}

/// Persisted per-model probe verdicts for a vendor family — hydrates the
/// provider Models card so earlier Test All results show immediately.
#[tauri::command]
#[specta::specta]
pub async fn list_model_statuses(
    engine: Engine<'_>,
    runner_id: Option<String>,
    family: String,
) -> R<Vec<ModelStatusInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_model_statuses",
            serde_json::json!({ "family": family }),
        )
        .await
}

/// Every persisted probe verdict, unfiltered — the family-scoped
/// `list_model_statuses` above stays for the provider Models card; this
/// variant feeds the app-wide picker filter.
#[tauri::command]
#[specta::specta]
pub async fn list_all_model_statuses(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ModelStatusEntry>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_all_model_statuses", serde_json::json!({}))
        .await
}

/// Re-fetch the live model list for every enabled connection in a vendor
/// family, persisting discoveries. Unlike the add/update-time best-effort
/// refresh, failures are returned to the UI instead of being swallowed.
#[tauri::command]
#[specta::specta]
pub async fn refresh_provider_models(
    engine: Engine<'_>,
    runner_id: Option<String>,
    family: String,
) -> R<Vec<RefreshModelsResult>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "refresh_provider_models",
            serde_json::json!({ "family": family }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_provider_quota(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<ProviderQuotaInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("connection_provider_quota", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn reset_codex_credit(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<CodexResetCreditResult> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("reset_codex_credit", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_model_routes(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ModelRouteInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_model_routes", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_model_route_target_capabilities(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ModelRouteTargetCapability>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_model_route_target_capabilities",
            serde_json::json!({}),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn save_model_route(
    engine: Engine<'_>,
    runner_id: Option<String>,
    route: ModelRouteInfo,
) -> R<Vec<ModelRouteInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("save_model_route", serde_json::json!({ "route": route }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_model_route(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<ModelRouteInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("delete_model_route", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn provider_account_route(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
) -> R<ProviderAccountRouteInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "provider_account_route",
            serde_json::json!({ "provider": provider }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_provider_account_route(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    strategy: ModelRouteStrategy,
) -> R<ProviderAccountRouteInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_provider_account_route",
            serde_json::json!({ "provider": provider, "strategy": strategy }),
        )
        .await
}

/// Drive the full interactive OAuth flow: a thin proxy to the daemon's
/// `connect_oauth` RPC, which binds a loopback listener and awaits the
/// callback (up to 5 minutes) before persisting the resulting connection.
/// The provider's authorize URL is opened in the system browser
/// client-side — by the per-runner SSE bridge
/// (`engine_manager::spawn_bridge`'s `oauthAuthorizeUrl` arm) on receipt of
/// `CoreEvent::OauthAuthorizeUrl` — not by this function directly.
#[tauri::command]
#[specta::specta]
pub async fn connect_oauth(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "connect_oauth",
            serde_json::json!({ "provider": provider, "label": label }),
        )
        .await
}

/// Reconnect an existing `needs_relogin` OAuth connection: drives the same
/// browser flow as [`connect_oauth`], but updates the connection in place
/// (same id/priority/label) instead of inserting a new row — otherwise the
/// stale, dead connection would keep shadowing the fresh one in
/// `route_model`'s `priority ASC` ordering.
#[tauri::command]
#[specta::specta]
pub async fn reconnect_oauth(
    engine: Engine<'_>,
    runner_id: Option<String>,
    connection_id: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "reconnect_oauth",
            serde_json::json!({ "connection_id": connection_id }),
        )
        .await
}

/// Start the manual (paste) OAuth fallback for environments where the
/// loopback listener can't receive the provider's redirect: opens the
/// authorize URL and hands back the PKCE material so the UI can show a paste
/// field, completed later via [`complete_oauth_manual`].
#[tauri::command]
#[specta::specta]
pub async fn begin_oauth_manual(app: tauri::AppHandle, provider: String) -> R<ManualStartInfo> {
    let m = oauth::callback::begin_manual(&provider).map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    let _ = app.opener().open_url(m.authorize_url.clone(), None::<&str>);
    Ok(ManualStartInfo {
        authorize_url: m.authorize_url,
        verifier: m.verifier,
        state: m.state,
        redirect_uri: m.redirect_uri,
    })
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn complete_oauth_manual(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    label: String,
    verifier: String,
    state: String,
    pasted: String,
    redirect_uri: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "complete_oauth_manual",
            serde_json::json!({
                "provider": provider, "label": label, "verifier": verifier,
                "state": state, "pasted": pasted, "redirect_uri": redirect_uri,
            }),
        )
        .await
}

/// Add a `no_auth` connection for a Free-category provider — refuses any
/// other category (those need the api-key or OAuth flows above).
#[tauri::command]
#[specta::specta]
pub async fn add_free_connection(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "add_free_connection",
            serde_json::json!({ "provider": provider, "label": label }),
        )
        .await
}

/// Start Kiro's AWS SSO-OIDC device-code flow: registers a public client,
/// starts a device authorization, opens the browser to the verification URL,
/// and stashes the in-flight state under a fresh `flow_id` for
/// [`await_kiro_device_flow`] to poll. Does not touch the store — nothing is
/// persisted until the user completes the browser step.
#[tauri::command]
#[specta::specta]
pub async fn start_kiro_device_flow(
    engine: Engine<'_>,
    runner_id: Option<String>,
    app: tauri::AppHandle,
) -> R<DeviceFlowInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    let info: DeviceFlowInfo = client
        .rpc("start_kiro_device_flow", serde_json::json!({}))
        .await?;
    let _ = app
        .opener()
        .open_url(info.verification_uri_complete.clone(), None::<&str>);
    Ok(info)
}

/// Poll the token endpoint for a flow started by [`start_kiro_device_flow`]
/// until the user completes the browser step (or the code expires/is
/// denied), then persist the resulting `kiro`/`oauth` connection. The flow
/// state is consumed from [`FLOWS`] up front — a `flow_id` can only ever be
/// awaited once, success or failure.
#[tauri::command]
#[specta::specta]
pub async fn await_kiro_device_flow(
    engine: Engine<'_>,
    runner_id: Option<String>,
    label: String,
    flow_id: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "await_kiro_device_flow",
            serde_json::json!({ "label": label, "flow_id": flow_id }),
        )
        .await
}

/// Import Kiro OAuth tokens from an already-installed, logged-in Kiro IDE
/// (its AWS SSO token cache + optional client registration + profile.json),
/// so a connection can be created without running the device-code flow.
/// Validates the imported refresh token with one [`oauth::refresh::force_refresh`]
/// call (which mints a fresh access token) before persisting — a dead/expired
/// import token surfaces its error instead of a connection that can't route.
#[tauri::command]
#[specta::specta]
pub async fn import_kiro_token(
    engine: Engine<'_>,
    runner_id: Option<String>,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("import_kiro_token", serde_json::json!({ "label": label }))
        .await
}

/// Start an RFC 8628 device-authorization grant for a device-grant provider
/// (Qwen, GitHub Copilot): request a device code, open the verification URL,
/// and stash in-flight state under a fresh `flow_id` for `await_device_flow`.
/// Errors for providers that are not device-grant (e.g. `kiro`, which uses
/// `start_kiro_device_flow`).
#[tauri::command]
#[specta::specta]
pub async fn start_device_flow(
    engine: Engine<'_>,
    runner_id: Option<String>,
    app: tauri::AppHandle,
    provider: String,
) -> R<DeviceFlowInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    let info: DeviceFlowInfo = client
        .rpc(
            "start_device_flow",
            serde_json::json!({ "provider": provider }),
        )
        .await?;
    let _ = app
        .opener()
        .open_url(info.verification_uri_complete.clone(), None::<&str>);
    Ok(info)
}

/// Poll a device-grant flow started by `start_device_flow` until completion,
/// then persist the connection with `auth_type = "oauth"`. GitHub Copilot runs
/// the Copilot-token exchange (leg 2) before persisting; Qwen captures its
/// shard `resource_url`.
#[tauri::command]
#[specta::specta]
pub async fn await_device_flow(
    engine: Engine<'_>,
    runner_id: Option<String>,
    provider: String,
    label: String,
    flow_id: String,
) -> R<Vec<ConnectionInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "await_device_flow",
            serde_json::json!({ "provider": provider, "label": label, "flow_id": flow_id }),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn catalog_marks_device_grant_providers() {
        let cat = list_provider_catalog().await.unwrap();
        let qwen = cat.iter().find(|e| e.id == "qwen").unwrap();
        assert_eq!(qwen.category, "oauth");
        assert!(qwen.uses_device_grant);
        let anth = cat.iter().find(|e| e.id == "anthropic-oauth").unwrap();
        assert!(!anth.uses_device_grant);
    }

    #[tokio::test]
    async fn catalog_exposes_free_tier_and_risk_notice() {
        let catalog = list_provider_catalog().await.unwrap();
        let by_id = |id: &str| catalog.iter().find(|e| e.id == id).unwrap();
        assert!(by_id("openrouter").free_tier);
        assert!(by_id("nvidia").free_tier);
        assert!(!by_id("anthropic").free_tier);
        assert!(by_id("kiro").risk_notice);
        assert!(!by_id("openai").risk_notice);
        // serde camelCase contract the frontend relies on
        let v = serde_json::to_value(by_id("kiro")).unwrap();
        assert_eq!(v["riskNotice"], serde_json::Value::Bool(true));
        assert_eq!(v["freeTier"], serde_json::Value::Bool(false));
    }
}
