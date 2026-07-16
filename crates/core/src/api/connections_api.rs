//! Providers tab RPC family: catalog-backed credentialed connections CRUD,
//! test, OAuth/device-flow sign-in. Moved (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/connections_cmd.rs`; that file keeps its own
//! copy until the proxy rewrite in Tasks 15-16. `list_provider_catalog` and
//! `begin_oauth_manual` stay Cockpit-local (no Store/ControlPlane access).
//!
//! Behavior change from the Tauri original: `connect_oauth`/`reconnect_oauth`
//! no longer take an `AppHandle` or open the system browser directly — they
//! broadcast [`CoreEvent::OauthAuthorizeUrl`] via `state.cp.emit(..)` and
//! Cockpit opens the browser on receipt (Task 15). Likewise
//! `start_kiro_device_flow`/`start_device_flow` drop the AppHandle/browser-open
//! — the verification URL is already in the returned `DeviceFlowInfo` and
//! Cockpit opens it after the RPC returns (Task 16).

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::domain::CoreEvent;
use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
use crate::llm_router::models;
use crate::llm_router::oauth;
use crate::llm_router::probe;
use crate::llm_router::quota::{self, CodexResetCreditResult, ProviderQuotaInfo};
use crate::llm_router::registry::{self, ProviderCategory};
use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy};
use crate::serve::ApiState;
use crate::store::ModelStatusRow;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub(crate) const HANDLES: &[&str] = &[
    "list_connections",
    "add_connection",
    "rename_connection",
    "set_connection_enabled",
    "remove_connection",
    "move_connection",
    "test_connection",
    "test_connection_model",
    "refresh_provider_models",
    "list_model_statuses",
    "list_all_model_statuses",
    "connection_provider_quota",
    "reset_codex_credit",
    "list_model_routes",
    "list_model_route_target_capabilities",
    "save_model_route",
    "delete_model_route",
    "provider_account_route",
    "set_provider_account_route",
    "connect_oauth",
    "reconnect_oauth",
    "complete_oauth_manual",
    "add_free_connection",
    "list_installed_providers",
    "install_provider",
    "uninstall_provider",
    "start_kiro_device_flow",
    "await_kiro_device_flow",
    "import_kiro_token",
    "start_device_flow",
    "await_device_flow",
];

/// In-flight Kiro device-code flow state, stashed between
/// `start_kiro_device_flow` (which registers the client and starts the
/// device authorization) and `await_kiro_device_flow` (which polls the token
/// endpoint) — mirrors `oauth::refresh`'s `REFRESH_LOCKS` shape.
struct KiroFlowState {
    client: oauth::device::RegisteredClient,
    device_code: String,
    interval: i64,
    deadline_ms: i64,
}

static FLOWS: OnceLock<Mutex<HashMap<String, KiroFlowState>>> = OnceLock::new();

fn flows() -> &'static Mutex<HashMap<String, KiroFlowState>> {
    FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// In-flight RFC 8628 device-grant state (Qwen, GitHub Copilot), stashed
/// between `start_device_flow` and `await_device_flow`. Separate from Kiro's
/// `FLOWS` (AWS SSO-OIDC) so Kiro's shipped path is untouched.
struct GrantFlowState {
    provider: String,
    device_code: String,
    /// PKCE verifier — `Some` only for providers with `use_pkce` (Qwen).
    verifier: Option<String>,
    interval: i64,
    deadline_ms: i64,
}

static GRANT_FLOWS: OnceLock<Mutex<HashMap<String, GrantFlowState>>> = OnceLock::new();

fn grant_flows() -> &'static Mutex<HashMap<String, GrantFlowState>> {
    GRANT_FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Deserialize)]
struct AddConnectionP {
    provider: String,
    label: String,
    api_key: String,
    base_url: Option<String>,
}
#[derive(Deserialize)]
struct RenameConnectionP {
    id: String,
    label: String,
}
#[derive(Deserialize)]
struct SetConnectionEnabledP {
    id: String,
    enabled: bool,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct MoveConnectionP {
    id: String,
    dir: i32,
}
#[derive(Deserialize)]
struct TestConnectionModelP {
    id: String,
    model: String,
}
#[derive(Deserialize)]
struct FamilyP {
    family: String,
}
#[derive(Deserialize)]
struct SaveModelRouteP {
    route: ModelRouteInfo,
}
#[derive(Deserialize)]
struct ProviderP {
    provider: String,
}
#[derive(Deserialize)]
struct SetProviderAccountRouteP {
    provider: String,
    strategy: ModelRouteStrategy,
}
#[derive(Deserialize)]
struct ConnectOauthP {
    provider: String,
    label: String,
}
#[derive(Deserialize)]
struct ReconnectOauthP {
    connection_id: String,
}
#[derive(Deserialize)]
struct CompleteOauthManualP {
    provider: String,
    label: String,
    verifier: String,
    state: String,
    pasted: String,
    redirect_uri: String,
}
#[derive(Deserialize)]
struct AddFreeConnectionP {
    provider: String,
    label: String,
}
#[derive(Deserialize)]
struct AwaitKiroDeviceFlowP {
    label: String,
    flow_id: String,
}
#[derive(Deserialize)]
struct ImportKiroTokenP {
    label: String,
}
#[derive(Deserialize)]
struct StartDeviceFlowP {
    provider: String,
}
#[derive(Deserialize)]
struct AwaitDeviceFlowP {
    provider: String,
    label: String,
    flow_id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_connections" => ok(assemble(cp).await?),
        "add_connection" => {
            let a: AddConnectionP = params(p)?;
            ok(add_connection(cp, a.provider, a.label, a.api_key, a.base_url).await?)
        }
        "rename_connection" => {
            let a: RenameConnectionP = params(p)?;
            connections::rename_connection(cp.store(), &a.id, &a.label).await?;
            ok(assemble(cp).await?)
        }
        "set_connection_enabled" => {
            let a: SetConnectionEnabledP = params(p)?;
            connections::set_connection_enabled(cp.store(), &a.id, a.enabled).await?;
            ok(assemble(cp).await?)
        }
        "remove_connection" => {
            let a: IdP = params(p)?;
            ok(remove_connection(cp, a.id).await?)
        }
        "move_connection" => {
            let a: MoveConnectionP = params(p)?;
            ok(move_connection(cp, a.id, a.dir).await?)
        }
        "test_connection" => {
            let a: IdP = params(p)?;
            ok(test_connection(cp, a.id).await?)
        }
        "test_connection_model" => {
            let a: TestConnectionModelP = params(p)?;
            ok(test_connection_model(cp, a.id, a.model).await?)
        }
        "refresh_provider_models" => {
            let a: FamilyP = params(p)?;
            ok(refresh_provider_models(cp, a.family).await?)
        }
        "list_model_statuses" => {
            let a: FamilyP = params(p)?;
            ok(list_model_statuses(cp, a.family).await?)
        }
        "list_all_model_statuses" => ok(list_all_model_statuses(cp).await?),
        "connection_provider_quota" => {
            let a: IdP = params(p)?;
            ok(connection_provider_quota(cp, a.id).await?)
        }
        "reset_codex_credit" => {
            let a: IdP = params(p)?;
            ok(reset_codex_credit(cp, a.id).await?)
        }
        "list_model_routes" => ok(routes::list_model_routes(cp.store()).await?),
        "list_model_route_target_capabilities" => {
            ok(routes::list_model_route_target_capabilities(cp.store()).await?)
        }
        "save_model_route" => {
            let a: SaveModelRouteP = params(p)?;
            routes::save_model_route(cp.store(), a.route).await?;
            ok(routes::list_model_routes(cp.store()).await?)
        }
        "delete_model_route" => {
            let a: IdP = params(p)?;
            routes::delete_model_route(cp.store(), &a.id).await?;
            ok(routes::list_model_routes(cp.store()).await?)
        }
        "provider_account_route" => {
            let a: ProviderP = params(p)?;
            ok(routes::provider_account_route(cp.store(), &a.provider).await?)
        }
        "set_provider_account_route" => {
            let a: SetProviderAccountRouteP = params(p)?;
            ok(routes::save_provider_account_route(cp.store(), &a.provider, a.strategy).await?)
        }
        "connect_oauth" => {
            let a: ConnectOauthP = params(p)?;
            ok(connect_oauth(cp, a.provider, a.label).await?)
        }
        "reconnect_oauth" => {
            let a: ReconnectOauthP = params(p)?;
            ok(reconnect_oauth(cp, a.connection_id).await?)
        }
        "complete_oauth_manual" => {
            let a: CompleteOauthManualP = params(p)?;
            ok(complete_oauth_manual(
                cp,
                a.provider,
                a.label,
                a.verifier,
                a.state,
                a.pasted,
                a.redirect_uri,
            )
            .await?)
        }
        "add_free_connection" => {
            let a: AddFreeConnectionP = params(p)?;
            ok(add_free_connection(cp, a.provider, a.label).await?)
        }
        "list_installed_providers" => {
            ok(crate::llm_router::installed::list_installed_providers(cp.store()).await?)
        }
        "install_provider" => {
            let a: FamilyP = params(p)?;
            ok(crate::llm_router::installed::install_provider(cp.store(), &a.family).await?)
        }
        "uninstall_provider" => {
            let a: FamilyP = params(p)?;
            ok(crate::llm_router::installed::uninstall_provider(cp.store(), &a.family).await?)
        }
        "start_kiro_device_flow" => ok(start_kiro_device_flow().await?),
        "await_kiro_device_flow" => {
            let a: AwaitKiroDeviceFlowP = params(p)?;
            ok(await_kiro_device_flow(cp, a.label, a.flow_id).await?)
        }
        "import_kiro_token" => {
            let a: ImportKiroTokenP = params(p)?;
            ok(import_kiro_token(cp, a.label).await?)
        }
        "start_device_flow" => {
            let a: StartDeviceFlowP = params(p)?;
            ok(start_device_flow(a.provider).await?)
        }
        "await_device_flow" => {
            let a: AwaitDeviceFlowP = params(p)?;
            ok(await_device_flow(cp, a.provider, a.label, a.flow_id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Mask a secret for display: first 3 + last 4 chars, elided in between.
fn to_info(row: &ConnectionRow) -> ConnectionInfo {
    let desc = registry::descriptor(&row.provider);
    ConnectionInfo {
        id: row.id.clone(),
        provider: row.provider.clone(),
        provider_name: desc
            .map(|d| d.name.to_string())
            .unwrap_or_else(|| row.provider.clone()),
        color: desc
            .map(|d| d.color.to_string())
            .unwrap_or_else(|| "#8B8B8B".into()),
        initial: desc
            .map(|d| d.initial.to_string())
            .unwrap_or_else(|| "?".into()),
        auth_type: row.auth_type.clone(),
        label: row.label.clone(),
        priority: row.priority as i32,
        enabled: row.enabled,
        quota_capability: quota::capability(row),
        models: desc
            .map(|d| connections::effective_models(d, row))
            .unwrap_or_default(),
        needs_relogin: row.data.needs_relogin.unwrap_or(false),
    }
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<ConnectionInfo>> {
    Ok(connections::list_connections(cp.store())
        .await?
        .iter()
        .map(to_info)
        .collect())
}

async fn refresh_models_best_effort(cp: &ControlPlane, row: &mut ConnectionRow) {
    let Ok(http) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    else {
        return;
    };
    let _ = models::refresh_connection_models(cp.store(), &http, row).await;
}

fn quota_http_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })
}

fn provider_http_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })
}

/// Ids of OTHER connections for `provider` that are dead
/// (`needs_relogin == Some(true)`) and should be cleared once a fresh one
/// (`keep_id`) is persisted. Never selects `keep_id`, a healthy row, or a row
/// for a different provider.
fn dead_connection_ids(rows: &[ConnectionRow], provider: &str, keep_id: &str) -> Vec<String> {
    rows.iter()
        .filter(|r| r.provider == provider && r.id != keep_id && r.data.needs_relogin == Some(true))
        .map(|r| r.id.clone())
        .collect()
}

/// Pick out the ids of OTHER kiro connections that are dead
/// (`needs_relogin == Some(true)`) and should be cleared once a fresh kiro
/// connection (`keep_id`) has been persisted. Split out from
/// [`remove_dead_kiro_connections`] so the actual selection logic — the fix
/// for the reconnect-shadow bug below — is unit-testable without a live
/// `Store`. Never selects `keep_id` itself, never selects a healthy kiro row
/// (`needs_relogin` `None`/`false`), and never selects a non-kiro row.
fn dead_kiro_connection_ids(rows: &[ConnectionRow], keep_id: &str) -> Vec<String> {
    dead_connection_ids(rows, "kiro", keep_id)
}

/// After persisting a fresh kiro connection (`keep_id`) via
/// [`await_kiro_device_flow`] or [`import_kiro_token`], remove any OTHER
/// kiro connection still flagged `needs_relogin`.
///
/// Without this cleanup, reconnecting Kiro doesn't actually fix anything:
/// `add_connection` always appends the new row at `MAX(priority)+1`, so the
/// OLD dead `needs_relogin` row keeps its lower priority and stays
/// `enabled`. `route_model` (server.rs) returns the FIRST enabled
/// connection for a provider ordered by `priority ASC`, so it keeps
/// resolving to the stale row and every kiro request keeps 401ing — the
/// app's own "Reconnect" / "Import from Kiro IDE" flow would silently leave
/// the user in a still-broken state until they manually deleted the old row.
async fn remove_dead_kiro_connections(cp: &ControlPlane, keep_id: &str) -> anyhow::Result<()> {
    let rows = connections::list_connections(cp.store()).await?;
    for id in dead_kiro_connection_ids(&rows, keep_id) {
        connections::remove_connection(cp.store(), &id).await?;
    }
    Ok(())
}

/// After persisting a fresh `provider` connection (`keep_id`), remove any OTHER
/// `provider` row still flagged `needs_relogin` (see
/// [`remove_dead_kiro_connections`] for why this shadow cleanup is required).
async fn remove_dead_connections(
    cp: &ControlPlane,
    provider: &str,
    keep_id: &str,
) -> anyhow::Result<()> {
    let rows = connections::list_connections(cp.store()).await?;
    for id in dead_connection_ids(&rows, provider, keep_id) {
        connections::remove_connection(cp.store(), &id).await?;
    }
    Ok(())
}

/// True if this connection should be refreshed for `family`: it belongs to the
/// family (via family_of, falling back to a direct provider==family match for
/// single-member families) AND is enabled.
fn is_refresh_target(row: &ConnectionRow, family: &str) -> bool {
    let in_family =
        registry::family_of(&row.provider).map_or(row.provider == family, |f| f == family);
    in_family && row.enabled
}

/// Display label for a refresh outcome: the connection's label, or its provider
/// id when the label is empty.
fn refresh_label(row: &ConnectionRow) -> String {
    if row.label.is_empty() {
        row.provider.clone()
    } else {
        row.label.clone()
    }
}

/// One outcome line per refreshed connection — pure so it's testable
/// without a Store or network.
fn refresh_message(label: &str, outcome: &Result<usize, String>) -> (bool, String) {
    match outcome {
        Ok(n) => (true, format!("{n} models discovered")),
        Err(e) => (false, format!("{label}: {e}")),
    }
}

async fn add_connection(
    cp: &ControlPlane,
    provider: String,
    label: String,
    api_key: String,
    base_url: Option<String>,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let desc = registry::descriptor(&provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider: {provider}")))?;
    if desc.category != ProviderCategory::ApiKey {
        return Err(ApiError::bad_request(format!(
            "{} is coming in a later phase.",
            desc.name
        )));
    }
    if desc.requires_base_url && base_url.as_deref().map(str::is_empty).unwrap_or(true) {
        return Err(ApiError::bad_request(format!(
            "{} requires a base URL",
            desc.name
        )));
    }
    let now = crate::paths::now_ms();
    let mut row = ConnectionRow {
        id: crate::paths::new_id(),
        provider,
        auth_type: "api_key".into(),
        label,
        priority: 0, // add_connection assigns MAX+1
        enabled: true,
        data: ConnectionData {
            api_key: if api_key.is_empty() {
                None
            } else {
                Some(api_key)
            },
            base_url_override: base_url.filter(|s| !s.is_empty()),
            models_override: None,
            ..Default::default()
        },
        created_at: now,
        updated_at: now,
    };
    connections::add_connection(cp.store(), row.clone()).await?;
    refresh_models_best_effort(cp, &mut row).await;
    Ok(assemble(cp).await?)
}

async fn remove_connection(cp: &ControlPlane, id: String) -> Result<Vec<ConnectionInfo>, ApiError> {
    connections::remove_connection(cp.store(), &id).await?;
    Ok(assemble(cp).await?)
}

async fn move_connection(
    cp: &ControlPlane,
    id: String,
    dir: i32,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    connections::move_connection(cp.store(), &id, dir).await?;
    Ok(assemble(cp).await?)
}

/// Map the probe response to the user-facing verdict: 2xx passes, 401/403
/// blames the API key, any other status is surfaced as-is, and a transport
/// failure (`Err` carries its display text) reads as network trouble.
/// The tri-state `status` comes from `models::probe_status` (tested in core).
fn probe_outcome(resp: Result<reqwest::StatusCode, String>) -> TestResult {
    let status = models::probe_status(resp.as_ref().ok().map(|s| s.as_u16()));
    let message = match &resp {
        Ok(s) if s.is_success() => "Connection OK".to_string(),
        Ok(s) if s.as_u16() == 401 || s.as_u16() == 403 => {
            "Rejected: the API key looks invalid for this provider.".to_string()
        }
        Ok(s) => format!("Upstream returned HTTP {s}"),
        Err(e) => format!("Network error: {e}"),
    };
    TestResult {
        ok: status == models::ProbeStatus::Valid,
        status: status.as_str().to_string(),
        message,
    }
}

/// Map the engine's probe outcome onto the wire type — verbatim, so the UI
/// strings and the persisted verdict stay identical to the pre-unification
/// behavior. The probe itself (request building, kiro/codex branches,
/// refresh retry) lives in `crate::llm_router::probe`.
fn to_test_result(outcome: &probe::ProbeOutcome) -> TestResult {
    TestResult {
        ok: outcome.ok,
        status: outcome.status.as_str().to_string(),
        message: outcome.message.clone(),
    }
}

/// Hit the upstream's model-list endpoint to distinguish bad credentials
/// (401/403) from network trouble, and persist the discovered model ids.
async fn test_connection(cp: &ControlPlane, id: String) -> Result<TestResult, ApiError> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown connection: {id}")))?;
    let desc = registry::descriptor(&row.provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider: {}", row.provider)))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;
    let result = match models::fetch_connection_models(cp.store(), &client, desc, &mut row).await {
        Ok((status, discovered, metadata)) => {
            if status.is_success() && !discovered.is_empty() {
                row.data.models_override = Some(discovered);
                row.data.model_meta_overrides = Some(metadata);
                row.updated_at = crate::paths::now_ms();
                let _ = connections::update_connection(cp.store(), row).await;
            }
            probe_outcome(Ok(status))
        }
        Err(e) => probe_outcome(Err(e.to_string())),
    };
    Ok(result)
}

async fn test_connection_model(
    cp: &ControlPlane,
    id: String,
    model: String,
) -> Result<TestResult, ApiError> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Ok(TestResult {
            ok: false,
            status: "invalid".into(),
            message: "Model id is empty".into(),
        });
    }
    let row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown connection: {id}")))?;
    let desc = registry::descriptor(&row.provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider: {}", row.provider)))?;
    let client = provider_http_client()?;
    let result = to_test_result(&probe::probe_model(&client, cp.store(), desc, &row, &model).await);
    // Best-effort persistence of definitive verdicts; upsert_model_status
    // ignores "unknown" so rate limits / outages never clobber a stored
    // valid/invalid record, and a store hiccup must not fail the probe.
    let _ = cp
        .store()
        .upsert_model_status(ModelStatusRow {
            family: desc.family.to_string(),
            model: model.clone(),
            status: result.status.clone(),
            message: result.message.clone(),
            tested_at: crate::paths::now_ms(),
        })
        .await;
    Ok(result)
}

/// Persisted per-model probe verdicts for a vendor family — hydrates the
/// provider Models card so earlier Test All results show immediately.
async fn list_model_statuses(
    cp: &ControlPlane,
    family: String,
) -> Result<Vec<ModelStatusInfo>, ApiError> {
    let rows = cp.store().list_model_statuses(&family).await?;
    Ok(rows
        .into_iter()
        .map(|row| ModelStatusInfo {
            model: row.model,
            status: row.status,
            message: row.message,
            tested_at: row.tested_at,
        })
        .collect())
}

/// Every persisted probe verdict, unfiltered — the family-scoped
/// `list_model_statuses` above stays for the provider Models card; this
/// variant feeds the app-wide picker filter.
async fn list_all_model_statuses(cp: &ControlPlane) -> Result<Vec<ModelStatusEntry>, ApiError> {
    let rows = cp.store().list_all_model_statuses().await?;
    Ok(rows
        .into_iter()
        .map(|row| ModelStatusEntry {
            family: row.family,
            model: row.model,
            status: row.status,
            message: row.message,
            tested_at: row.tested_at,
        })
        .collect())
}

/// Re-fetch the live model list for every enabled connection in a vendor
/// family, persisting discoveries. Unlike the add/update-time best-effort
/// refresh, failures are returned to the UI instead of being swallowed.
async fn refresh_provider_models(
    cp: &ControlPlane,
    family: String,
) -> Result<Vec<RefreshModelsResult>, ApiError> {
    let http = quota_http_client()?;
    let rows = connections::list_connections(cp.store()).await?;
    let mut out = Vec::new();
    for mut row in rows {
        if !is_refresh_target(&row, &family) {
            continue;
        }
        let label = refresh_label(&row);
        let has_endpoint =
            registry::descriptor(&row.provider).is_none_or(|d| d.has_models_endpoint);
        if !has_endpoint {
            out.push(RefreshModelsResult {
                connection_id: row.id.clone(),
                label,
                ok: true,
                message: "Uses a seeded model list — no live catalog endpoint".to_string(),
            });
            continue;
        }
        let outcome = models::refresh_connection_models(cp.store(), &http, &mut row)
            .await
            .map(|models| models.len())
            .map_err(|e| e.to_string());
        let (ok, message) = refresh_message(&label, &outcome);
        out.push(RefreshModelsResult {
            connection_id: row.id.clone(),
            label,
            ok,
            message,
        });
    }
    Ok(out)
}

async fn connection_provider_quota(
    cp: &ControlPlane,
    id: String,
) -> Result<ProviderQuotaInfo, ApiError> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown connection: {id}")))?;
    let http = quota_http_client()?;
    Ok(quota::fetch_provider_quota(cp.store(), &http, &mut row).await?)
}

async fn reset_codex_credit(
    cp: &ControlPlane,
    id: String,
) -> Result<CodexResetCreditResult, ApiError> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown connection: {id}")))?;
    let http = quota_http_client()?;
    Ok(quota::consume_codex_reset_credit(cp.store(), &http, &mut row).await?)
}

/// Drive the full interactive OAuth flow: binds a loopback listener, hands
/// the authorize URL to surfaces via [`CoreEvent::OauthAuthorizeUrl`] (Cockpit
/// opens it in the system browser on receipt), and awaits the callback (up
/// to 5 minutes) before persisting the resulting connection.
async fn connect_oauth(
    cp: &ControlPlane,
    provider: String,
    label: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let http = reqwest::Client::new();
    let provider_for_event = provider.clone();
    let mut row = oauth::callback::run_flow(
        cp.store(),
        &http,
        &provider,
        &label,
        None,
        move |url| {
            cp.emit(CoreEvent::OauthAuthorizeUrl {
                provider: provider_for_event,
                authorize_url: url.to_string(),
            });
        },
        Duration::from_secs(300),
    )
    .await
    .map_err(|e| ApiError {
        status: 500,
        message: e.to_string(),
    })?;
    refresh_models_best_effort(cp, &mut row).await;
    Ok(assemble(cp).await?)
}

/// Reconnect an existing `needs_relogin` OAuth connection: drives the same
/// browser flow as [`connect_oauth`], but updates the connection in place
/// (same id/priority/label) instead of inserting a new row — otherwise the
/// stale, dead connection would keep shadowing the fresh one in
/// `route_model`'s `priority ASC` ordering.
async fn reconnect_oauth(
    cp: &ControlPlane,
    connection_id: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let existing = connections::get_connection(cp.store(), &connection_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown connection: {connection_id}")))?;
    let http = reqwest::Client::new();
    let provider_for_event = existing.provider.clone();
    let mut row = oauth::callback::run_flow(
        cp.store(),
        &http,
        &existing.provider,
        &existing.label,
        Some(&connection_id),
        move |url| {
            cp.emit(CoreEvent::OauthAuthorizeUrl {
                provider: provider_for_event,
                authorize_url: url.to_string(),
            });
        },
        Duration::from_secs(300),
    )
    .await
    .map_err(|e| ApiError {
        status: 500,
        message: e.to_string(),
    })?;
    refresh_models_best_effort(cp, &mut row).await;
    Ok(assemble(cp).await?)
}

#[allow(clippy::too_many_arguments)]
async fn complete_oauth_manual(
    cp: &ControlPlane,
    provider: String,
    label: String,
    verifier: String,
    state: String,
    pasted: String,
    redirect_uri: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let http = reqwest::Client::new();
    let mut row = oauth::callback::complete_manual(
        cp.store(),
        &http,
        &provider,
        &label,
        None,
        &verifier,
        &state,
        &pasted,
        &redirect_uri,
    )
    .await
    .map_err(|e| ApiError {
        status: 500,
        message: e.to_string(),
    })?;
    refresh_models_best_effort(cp, &mut row).await;
    Ok(assemble(cp).await?)
}

/// Add a `no_auth` connection for a Free-category provider — refuses any
/// other category (those need the api-key or OAuth flows above).
async fn add_free_connection(
    cp: &ControlPlane,
    provider: String,
    label: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let desc = registry::descriptor(&provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider: {provider}")))?;
    if desc.category != ProviderCategory::Free {
        return Err(ApiError::bad_request(format!(
            "{} is not a free provider",
            desc.name
        )));
    }
    if desc.device_flow.is_some() {
        return Err(ApiError::bad_request(format!(
            "{} uses device login — connect it from the provider list.",
            desc.name
        )));
    }
    let now = crate::paths::now_ms();
    let mut row = ConnectionRow {
        id: crate::paths::new_id(),
        provider,
        auth_type: "free".into(),
        label,
        priority: 0,
        enabled: true,
        data: ConnectionData::default(),
        created_at: now,
        updated_at: now,
    };
    connections::add_connection(cp.store(), row.clone()).await?;
    refresh_models_best_effort(cp, &mut row).await;
    Ok(assemble(cp).await?)
}

/// Start Kiro's AWS SSO-OIDC device-code flow: registers a public client,
/// starts a device authorization, and stashes the in-flight state under a
/// fresh `flow_id` for [`await_kiro_device_flow`] to poll. Does not touch
/// the store — nothing is persisted until the user completes the browser
/// step. Does not open the browser — Cockpit does that after the RPC
/// returns, using the verification URL in the response.
async fn start_kiro_device_flow() -> Result<DeviceFlowInfo, ApiError> {
    let http = reqwest::Client::new();
    let cfg = registry::device_flow_config("kiro")
        .ok_or_else(|| ApiError::bad_request("kiro device flow is not configured"))?;
    let client = oauth::device::register_client(&http, cfg)
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;
    let auth = oauth::device::start_device_authorization(&http, cfg, &client)
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;

    let flow_id = crate::paths::new_id();
    let deadline_ms = crate::paths::now_ms() + auth.expires_in * 1000;
    flows().lock().unwrap_or_else(|e| e.into_inner()).insert(
        flow_id.clone(),
        KiroFlowState {
            client,
            device_code: auth.device_code.clone(),
            interval: auth.interval,
            deadline_ms,
        },
    );

    Ok(DeviceFlowInfo {
        flow_id,
        user_code: auth.user_code,
        verification_uri: auth.verification_uri,
        verification_uri_complete: auth.verification_uri_complete,
        expires_in: auth.expires_in,
        interval: auth.interval,
    })
}

/// Poll the token endpoint for a flow started by [`start_kiro_device_flow`]
/// until the user completes the browser step (or the code expires/is
/// denied), then persist the resulting `kiro`/`oauth` connection. The flow
/// state is consumed from [`FLOWS`] up front — a `flow_id` can only ever be
/// awaited once, success or failure.
async fn await_kiro_device_flow(
    cp: &ControlPlane,
    label: String,
    flow_id: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let KiroFlowState {
        client,
        device_code,
        mut interval,
        deadline_ms,
    } = flows()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&flow_id)
        .ok_or_else(|| ApiError::bad_request("device sign-in flow not found — start again"))?;

    let http = reqwest::Client::new();
    let cfg = registry::device_flow_config("kiro")
        .ok_or_else(|| ApiError::bad_request("kiro device flow is not configured"))?;

    let tokens = loop {
        if crate::paths::now_ms() > deadline_ms {
            return Err(ApiError::bad_request("device code expired — start again"));
        }
        tokio::time::sleep(Duration::from_secs(interval.max(1) as u64)).await;
        match oauth::device::poll_token_once(&http, cfg.token_url, &client, &device_code)
            .await
            .map_err(|e| ApiError {
                status: 500,
                message: e.to_string(),
            })? {
            oauth::device::PollOutcome::Pending => continue,
            oauth::device::PollOutcome::SlowDown => {
                interval = (interval + 5).min(30);
                continue;
            }
            oauth::device::PollOutcome::Denied => {
                return Err(ApiError::bad_request("sign-in was denied"));
            }
            oauth::device::PollOutcome::Expired => {
                return Err(ApiError::bad_request("device code expired — start again"));
            }
            oauth::device::PollOutcome::Ready(tokens) => break tokens,
        }
    };

    let profile_arn = oauth::device::resolve_profile_arn(&http, &tokens.access_token)
        .await
        .unwrap_or_else(|| connections::default_profile_arn("builder-id").to_string());

    let now = crate::paths::now_ms();
    let new_id = crate::paths::new_id();
    connections::add_connection(
        cp.store(),
        ConnectionRow {
            id: new_id.clone(),
            provider: "kiro".into(),
            auth_type: "oauth".into(),
            label,
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some(tokens.access_token),
                refresh_token: tokens.refresh_token,
                expires_at: Some(tokens.expires_at),
                // The AWS SSO-OIDC client creds registered in
                // `start_kiro_device_flow` MUST be carried here — see
                // [`kiro_device_provider_specific`].
                provider_specific: Some(kiro_device_provider_specific(
                    &client.client_id,
                    &client.client_secret,
                    &profile_arn,
                )),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
    // Sign-in via device flow succeeded — any OTHER kiro row still marked
    // `needs_relogin` is now dead weight that would otherwise keep
    // shadowing this fresh connection in `route_model`. See
    // `remove_dead_kiro_connections`.
    remove_dead_kiro_connections(cp, &new_id).await?;
    Ok(assemble(cp).await?)
}

/// Build the `provider_specific` blob for a Kiro device-flow (builder-id)
/// connection. The AWS SSO-OIDC `clientId`/`clientSecret` registered in
/// [`start_kiro_device_flow`] MUST be carried here: `oauth::refresh::refresh_kiro`
/// selects the AWS SSO-OIDC token endpoint only when
/// `connections::kiro_client_creds` finds BOTH, and otherwise falls through to
/// the kiro.dev SOCIAL refresh endpoint (meant for google/github/imported).
/// Omitting them would make a builder-id connection fail its first refresh
/// (~5 min before the ~1h expiry, or on a 401/403) and flip to `needs_relogin`.
/// For the device flow both are always present — the client was just
/// registered — so they're added unconditionally.
fn kiro_device_provider_specific(
    client_id: &str,
    client_secret: &str,
    profile_arn: &str,
) -> serde_json::Value {
    serde_json::json!({
        "authMethod": "builder-id",
        "profileArn": profile_arn,
        "region": "us-east-1",
        "clientId": client_id,
        "clientSecret": client_secret,
    })
}

/// Import Kiro OAuth tokens from an already-installed, logged-in Kiro IDE
/// (its AWS SSO token cache + optional client registration + profile.json),
/// so a connection can be created without running the device-code flow.
/// Validates the imported refresh token with one [`oauth::refresh::force_refresh`]
/// call (which mints a fresh access token) before persisting — a dead/expired
/// import token surfaces its error instead of a connection that can't route.
async fn import_kiro_token(
    cp: &ControlPlane,
    label: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let imported = oauth::import::read_kiro_ide_cache()?;

    let mut provider_specific = serde_json::json!({
        "authMethod": imported.auth_method,
        "region": imported.region.unwrap_or_else(|| "us-east-1".into()),
    });
    if let Some(arn) = imported.profile_arn {
        provider_specific["profileArn"] = serde_json::json!(arn);
    }
    if let (Some(client_id), Some(client_secret)) = (imported.client_id, imported.client_secret) {
        provider_specific["clientId"] = serde_json::json!(client_id);
        provider_specific["clientSecret"] = serde_json::json!(client_secret);
    }

    let now = crate::paths::now_ms();
    let new_id = crate::paths::new_id();
    let mut row = ConnectionRow {
        id: new_id.clone(),
        provider: "kiro".into(),
        auth_type: "oauth".into(),
        label,
        priority: 0,
        enabled: true,
        data: ConnectionData {
            refresh_token: Some(imported.refresh_token),
            provider_specific: Some(provider_specific),
            ..Default::default()
        },
        created_at: now,
        updated_at: now,
    };

    let http = reqwest::Client::new();
    // This refreshes `row` IN MEMORY before it's ever inserted: `row.id` isn't
    // in the DB yet, so `force_refresh`'s internal `update_connection` (an
    // UPDATE-by-id) is a harmless 0-row no-op here — the fresh access token
    // it mints is captured straight into `row.data` and only actually
    // persisted by the `add_connection` call below.
    oauth::refresh::force_refresh(cp.store(), &http, &mut row)
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: format!("imported Kiro token looks dead: {e}"),
        })?;

    connections::add_connection(cp.store(), row).await?;
    // Import succeeded — clear any OTHER dead kiro row so it doesn't keep
    // shadowing this fresh connection in `route_model`. See
    // `remove_dead_kiro_connections`.
    remove_dead_kiro_connections(cp, &new_id).await?;
    Ok(assemble(cp).await?)
}

/// Start an RFC 8628 device-authorization grant for a device-grant provider
/// (Qwen, GitHub Copilot): request a device code, and stash in-flight state
/// under a fresh `flow_id` for `await_device_flow`. Does not open the
/// browser — Cockpit does that after the RPC returns. Errors for providers
/// that are not device-grant (e.g. `kiro`, which uses `start_kiro_device_flow`).
async fn start_device_flow(provider: String) -> Result<DeviceFlowInfo, ApiError> {
    let cfg = registry::device_grant_config(&provider).ok_or_else(|| {
        ApiError::bad_request(format!("{provider} is not a device-grant provider"))
    })?;
    let http = reqwest::Client::new();
    let pkce = if cfg.use_pkce {
        Some(oauth::pkce::generate())
    } else {
        None
    };
    let auth = oauth::device_grant::request_device_code(&http, cfg, pkce.as_ref())
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;

    let flow_id = crate::paths::new_id();
    let deadline_ms = crate::paths::now_ms() + auth.expires_in * 1000;
    grant_flows()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            flow_id.clone(),
            GrantFlowState {
                provider: provider.clone(),
                device_code: auth.device_code.clone(),
                verifier: pkce.map(|p| p.verifier),
                interval: auth.interval,
                deadline_ms,
            },
        );

    Ok(DeviceFlowInfo {
        flow_id,
        user_code: auth.user_code,
        verification_uri: auth.verification_uri,
        verification_uri_complete: auth.verification_uri_complete,
        expires_in: auth.expires_in,
        interval: auth.interval,
    })
}

/// Poll a device-grant flow started by `start_device_flow` until completion,
/// then persist the connection with `auth_type = "oauth"`. GitHub Copilot runs
/// the Copilot-token exchange (leg 2) before persisting; Qwen captures its
/// shard `resource_url`.
async fn await_device_flow(
    cp: &ControlPlane,
    provider: String,
    label: String,
    flow_id: String,
) -> Result<Vec<ConnectionInfo>, ApiError> {
    let GrantFlowState {
        provider: flow_provider,
        device_code,
        verifier,
        mut interval,
        deadline_ms,
    } = grant_flows()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&flow_id)
        .ok_or_else(|| ApiError::bad_request("device sign-in flow not found — start again"))?;
    if flow_provider != provider {
        return Err(ApiError::bad_request("device flow provider mismatch"));
    }
    let cfg = registry::device_grant_config(&provider).ok_or_else(|| {
        ApiError::bad_request(format!("{provider} is not a device-grant provider"))
    })?;
    let pkce = verifier.map(|v| oauth::pkce::Pkce {
        verifier: v,
        challenge: String::new(),
        state: String::new(),
    });
    let http = reqwest::Client::new();

    let tokens = loop {
        if crate::paths::now_ms() > deadline_ms {
            return Err(ApiError::bad_request("device code expired — start again"));
        }
        tokio::time::sleep(Duration::from_secs(interval.max(1) as u64)).await;
        match oauth::device_grant::poll_token_once(&http, cfg, &device_code, pkce.as_ref())
            .await
            .map_err(|e| ApiError {
                status: 500,
                message: e.to_string(),
            })? {
            oauth::device_grant::GrantPoll::Pending => continue,
            oauth::device_grant::GrantPoll::SlowDown => {
                interval = (interval + 5).min(30);
                continue;
            }
            oauth::device_grant::GrantPoll::Denied => {
                return Err(ApiError::bad_request("sign-in was denied"));
            }
            oauth::device_grant::GrantPoll::Expired => {
                return Err(ApiError::bad_request("device code expired — start again"));
            }
            oauth::device_grant::GrantPoll::Ready(t) => break t,
        }
    };

    let data = build_device_grant_data(cfg, tokens, &http)
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;

    let now = crate::paths::now_ms();
    let new_id = crate::paths::new_id();
    connections::add_connection(
        cp.store(),
        ConnectionRow {
            id: new_id.clone(),
            provider: provider.clone(),
            auth_type: "oauth".into(),
            label,
            priority: 0,
            enabled: true,
            data,
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
    remove_dead_connections(cp, &provider, &new_id).await?;
    Ok(assemble(cp).await?)
}

/// Build `ConnectionData` from a completed device grant. GitHub Copilot swaps
/// the GitHub token for a Copilot token (stored as `access_token`, GitHub token
/// kept as the durable `refresh_token`). Qwen keeps the grant tokens and its
/// `resource_url`.
async fn build_device_grant_data(
    cfg: &registry::DeviceGrantConfig,
    tokens: oauth::device_grant::GrantTokens,
    http: &reqwest::Client,
) -> anyhow::Result<ConnectionData> {
    if let Some(exchange) = cfg.token_exchange.as_ref() {
        let gh_token = tokens.access_token;
        let copilot = oauth::github::exchange_copilot_token(http, &gh_token, exchange.url).await?;
        let mut provider_specific = None;
        if let Some(gh_refresh) = tokens.refresh_token {
            provider_specific = Some(serde_json::json!({ "gh_refresh": gh_refresh }));
        }
        Ok(ConnectionData {
            access_token: Some(copilot.token),
            refresh_token: Some(gh_token),
            expires_at: Some(copilot.expires_at_ms),
            provider_specific,
            ..Default::default()
        })
    } else {
        let resource_url = tokens
            .raw
            .get("resource_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| serde_json::json!({ "resource_url": s }));
        Ok(ConnectionData {
            access_token: Some(tokens.access_token),
            refresh_token: tokens.refresh_token,
            expires_at: Some(tokens.expires_at),
            provider_specific: resource_url,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    fn status(code: u16) -> reqwest::StatusCode {
        reqwest::StatusCode::from_u16(code).unwrap()
    }

    #[tokio::test]
    async fn list_model_route_target_capabilities_dispatches_resolver_backed_anthropic_capability()
    {
        let s = state().await;
        connections::add_connection(
            s.cp.store(),
            ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    models_override: Some(vec!["claude-opus-4-7".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let capabilities: Vec<routes::ModelRouteTargetCapability> = serde_json::from_value(
            dispatch(&s, "list_model_route_target_capabilities", json!({}))
                .await
                .unwrap(),
        )
        .unwrap();
        let capability = capabilities
            .iter()
            .find(|capability| {
                capability.provider == "anthropic" && capability.model == "claude-opus-4-7"
            })
            .unwrap();

        assert_eq!(
            capability
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "max", "xhigh"]
        );
        assert_eq!(capability.provider_default.as_deref(), Some("high"));
    }

    #[test]
    fn success_status_reports_ok() {
        let r = probe_outcome(Ok(status(200)));
        assert!(r.ok);
        assert_eq!(r.message, "Connection OK");
    }

    #[test]
    fn auth_failures_blame_the_key() {
        for code in [401, 403] {
            let r = probe_outcome(Ok(status(code)));
            assert!(!r.ok);
            assert_eq!(
                r.message,
                "Rejected: the API key looks invalid for this provider."
            );
        }
    }

    #[test]
    fn other_statuses_are_surfaced() {
        let r = probe_outcome(Ok(status(500)));
        assert!(!r.ok);
        assert_eq!(
            r.message,
            "Upstream returned HTTP 500 Internal Server Error"
        );
    }

    #[test]
    fn transport_failure_reads_as_network_error() {
        let r = probe_outcome(Err("connection refused".into()));
        assert!(!r.ok);
        assert_eq!(r.message, "Network error: connection refused");
    }

    #[test]
    fn connection_info_exposes_quota_capability_without_credentials_or_cloak_config() {
        let mut row = ConnectionRow {
            id: "c1".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "Claude Code".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData::default(),
            created_at: 0,
            updated_at: 0,
        };
        let claude = serde_json::to_value(to_info(&row)).unwrap();
        assert_eq!(claude["quotaCapability"], "claude");
        assert!(claude.get("baseUrl").is_none());
        assert!(claude.get("keyMasked").is_none());
        assert!(claude.get("claudeCloaking").is_none());

        row.provider = "openai-oauth".into();
        let codex = serde_json::to_value(to_info(&row)).unwrap();
        assert_eq!(codex["quotaCapability"], "codex");
    }

    #[test]
    fn to_test_result_maps_probe_outcome_verbatim() {
        let outcome = probe::ProbeOutcome {
            ok: false,
            status: models::ProbeStatus::Invalid,
            message: "Model m returned HTTP 404 Not Found".into(),
        };
        let r = to_test_result(&outcome);
        assert!(!r.ok);
        assert_eq!(r.status, "invalid");
        assert_eq!(r.message, "Model m returned HTTP 404 Not Found");

        let ok = probe::ProbeOutcome {
            ok: true,
            status: models::ProbeStatus::Valid,
            message: "Model m OK".into(),
        };
        let r = to_test_result(&ok);
        assert!(r.ok);
        assert_eq!(r.status, "valid");
        assert_eq!(r.message, "Model m OK");
    }

    /// A kiro device-flow connection must persist the registered AWS SSO-OIDC
    /// client creds so `refresh_kiro` routes to the AWS token endpoint (via
    /// `kiro_client_creds` finding both) instead of the kiro.dev social one.
    #[test]
    fn device_provider_specific_carries_client_creds_for_refresh() {
        let ps = kiro_device_provider_specific(
            "cid-1",
            "secret-1",
            "arn:aws:codewhisperer:us-east-1:1:profile/X",
        );
        assert_eq!(ps["authMethod"], "builder-id");
        assert_eq!(ps["region"], "us-east-1");
        assert_eq!(
            ps["profileArn"],
            "arn:aws:codewhisperer:us-east-1:1:profile/X"
        );

        // The whole point of the fix: the persisted shape must make
        // `kiro_client_creds` return Some, which is what selects the AWS
        // SSO-OIDC (builder-id) refresh endpoint.
        let data = ConnectionData {
            provider_specific: Some(ps),
            ..Default::default()
        };
        assert_eq!(
            connections::kiro_client_creds(&data),
            Some(("cid-1".to_string(), "secret-1".to_string()))
        );
    }

    fn kiro_row(id: &str, needs_relogin: Option<bool>) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: "kiro".into(),
            auth_type: "oauth".into(),
            label: "Kiro".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                needs_relogin,
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The reconnect-shadow fix, isolated from the store: reconnecting Kiro
    /// (device flow or import) must clear ONLY the other dead kiro rows —
    /// not the just-created row, not a healthy kiro row, and not a dead row
    /// belonging to some other provider.
    #[test]
    fn dead_kiro_connection_ids_targets_only_other_dead_kiro_rows() {
        let rows = vec![
            kiro_row("dead-1", Some(true)),
            kiro_row("healthy-none", None),
            kiro_row("healthy-false", Some(false)),
            kiro_row("new-1", Some(true)), // the just-persisted row itself
            ConnectionRow {
                id: "other-provider-dead".into(),
                provider: "anthropic-oauth".into(),
                auth_type: "oauth".into(),
                label: "Other".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    needs_relogin: Some(true),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        ];
        assert_eq!(
            dead_kiro_connection_ids(&rows, "new-1"),
            vec!["dead-1".to_string()]
        );
    }

    #[test]
    fn dead_kiro_connection_ids_empty_when_no_dead_rows() {
        let rows = vec![
            kiro_row("healthy-1", None),
            kiro_row("healthy-2", Some(false)),
        ];
        assert!(dead_kiro_connection_ids(&rows, "healthy-1").is_empty());
    }

    #[test]
    fn refresh_message_reports_count_or_error() {
        assert_eq!(
            refresh_message("Work OpenAI", &Ok(12)),
            (true, "12 models discovered".to_string())
        );
        assert_eq!(
            refresh_message(
                "Work OpenAI",
                &Err("model list request for openai failed with status 401".to_string())
            ),
            (
                false,
                "Work OpenAI: model list request for openai failed with status 401".to_string()
            )
        );
    }

    fn connection_row(id: &str, provider: &str, label: &str, enabled: bool) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: "api_key".into(),
            label: label.into(),
            priority: 0,
            enabled,
            data: ConnectionData::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// `refresh_provider_models` must refresh only enabled, in-family
    /// connections: a disabled in-family row, an enabled row from a different
    /// family, and an enabled row with an unrecognized provider id must all be
    /// skipped.
    #[test]
    fn refresh_targets_filter_by_family_and_enabled() {
        let in_family_enabled = connection_row("c1", "anthropic-oauth", "Claude", true);
        let in_family_disabled = connection_row("c2", "anthropic-oauth", "Claude (off)", false);
        let other_family_enabled = connection_row("c3", "openai", "Work OpenAI", true);
        let unrecognized_provider_enabled =
            connection_row("c4", "not-a-real-provider", "Mystery", true);

        assert!(is_refresh_target(&in_family_enabled, "anthropic"));
        assert!(!is_refresh_target(&in_family_disabled, "anthropic"));
        assert!(!is_refresh_target(&other_family_enabled, "anthropic"));
        assert!(!is_refresh_target(
            &unrecognized_provider_enabled,
            "anthropic"
        ));
    }

    #[test]
    fn refresh_label_falls_back_to_provider_when_label_is_empty() {
        let row = connection_row("c1", "openai", "", true);
        assert_eq!(refresh_label(&row), "openai");
    }

    #[test]
    fn refresh_label_uses_the_connection_label_when_present() {
        let row = connection_row("c1", "openai", "Work OpenAI", true);
        assert_eq!(refresh_label(&row), "Work OpenAI");
    }

    #[test]
    fn dead_connection_ids_selects_by_provider() {
        let mk = |id: &str, provider: &str, relogin: Option<bool>| ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: "oauth".into(),
            label: String::new(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                needs_relogin: relogin,
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        };
        let rows = vec![
            mk("keep", "qwen", Some(true)),            // keep_id — excluded
            mk("dead", "qwen", Some(true)),            // selected
            mk("healthy", "qwen", None),               // healthy — excluded
            mk("other", "github-copilot", Some(true)), // other provider — excluded
        ];
        assert_eq!(
            dead_connection_ids(&rows, "qwen", "keep"),
            vec!["dead".to_string()]
        );
        assert_eq!(
            dead_connection_ids(&rows, "github-copilot", "x"),
            vec!["other".to_string()]
        );
    }

    #[tokio::test]
    async fn build_device_grant_data_qwen_captures_resource_url() {
        let cfg = registry::device_grant_config("qwen").unwrap();
        let http = reqwest::Client::new();
        let tokens = oauth::device_grant::GrantTokens {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: 123,
            raw: serde_json::json!({ "resource_url": "dashscope.aliyuncs.com" }),
        };
        let data = build_device_grant_data(cfg, tokens, &http).await.unwrap();
        assert_eq!(data.access_token.as_deref(), Some("at"));
        assert_eq!(data.refresh_token.as_deref(), Some("rt"));
        assert_eq!(data.expires_at, Some(123));
        assert_eq!(
            data.provider_specific.unwrap()["resource_url"],
            "dashscope.aliyuncs.com"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn add_and_list_connection_via_rpc() {
        let s = state().await;
        let list = dispatch(
            &s,
            "add_connection",
            json!({
                "provider": "openrouter", "label": "or-1",
                "api_key": "sk-test", "base_url": null
            }),
        )
        .await
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);
        let added = list
            .as_array()
            .unwrap()
            .iter()
            .find(|connection| connection["provider"] == "openrouter")
            .unwrap();
        let id = added["id"].as_str().unwrap();

        let renamed = dispatch(
            &s,
            "rename_connection",
            json!({"id": id, "label": "Primary"}),
        )
        .await
        .unwrap();
        assert_eq!(
            renamed
                .as_array()
                .unwrap()
                .iter()
                .find(|connection| connection["id"] == id)
                .unwrap()["label"],
            "Primary"
        );

        let disabled = dispatch(
            &s,
            "set_connection_enabled",
            json!({"id": id, "enabled": false}),
        )
        .await
        .unwrap();
        let disabled = disabled
            .as_array()
            .unwrap()
            .iter()
            .find(|connection| connection["id"] == id)
            .unwrap();
        assert_eq!(disabled["enabled"], false);
        assert!(disabled.get("keyMasked").is_none());
        assert!(disabled.get("claudeCloaking").is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn add_connection_accepts_the_subscription_members() {
        // The MiMo Token Plan and OpenCode Go descriptors are `ApiKey`, so they
        // skip add_connection's non-ApiKey "coming in a later phase" refusal.
        let s = state().await;

        let list = dispatch(
            &s,
            "add_connection",
            json!({
                "provider": "mimo", "label": "MiMo (Token Plan)",
                "api_key": "tp-test",
                "base_url": "https://token-plan-sgp.xiaomimimo.com/v1"
            }),
        )
        .await
        .unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|connection| connection["provider"] == "mimo"),
            "mimo subscription connection should be added"
        );

        let list = dispatch(
            &s,
            "add_connection",
            json!({
                "provider": "opencode", "label": "OpenCode (Go)",
                "api_key": "sk-oc-test", "base_url": null
            }),
        )
        .await
        .unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|connection| connection["provider"] == "opencode"),
            "opencode subscription connection should be added"
        );
    }
}
