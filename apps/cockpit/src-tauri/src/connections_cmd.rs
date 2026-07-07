//! Providers tab commands: catalog + credentialed connections CRUD + test.
use crate::error::CmdError;
use crate::events::OauthAuthorizeUrlMsg;
use ryuzi_core::llm_router::claude_cloak;
use ryuzi_core::llm_router::connections::{self, ConnectionData, ConnectionRow};
use ryuzi_core::llm_router::models;
use ryuzi_core::llm_router::oauth;
use ryuzi_core::llm_router::quota::{self, CodexResetCreditResult, ProviderQuotaInfo};
use ryuzi_core::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderCategory};
use ryuzi_core::llm_router::routes::{
    self, ModelRouteInfo, ModelRouteStrategy, ProviderAccountRouteInfo,
};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event as _;

type R<T> = Result<T, CmdError>;

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
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub id: String,
    pub provider: String,
    pub provider_name: String,
    pub color: String,
    pub initial: String,
    pub auth_type: String,
    pub label: String,
    pub priority: i32,
    pub enabled: bool,
    pub base_url: Option<String>,
    pub models: Vec<String>,
    /// e.g. "sk-…3fk9" — full key never leaves the backend after creation.
    pub key_masked: Option<String>,
    /// OAuth connections only: true once refresh has failed terminally and
    /// the user needs to reconnect via the browser/paste flow again.
    pub needs_relogin: bool,
    /// Anthropic OAuth only: enable full Claude Code-style request cloaking.
    pub claude_cloaking: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RefreshModelsResult {
    pub connection_id: String,
    pub label: String,
    pub ok: bool,
    pub message: String,
}

/// One outcome line per refreshed connection — pure so it's testable
/// without a Store or network.
fn refresh_message(label: &str, outcome: &Result<usize, String>) -> (bool, String) {
    match outcome {
        Ok(n) => (true, format!("{n} models discovered")),
        Err(e) => (false, format!("{label}: {e}")),
    }
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ManualStartInfo {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
}

/// Device-code flow info shown to the user while they complete the browser
/// step (Kiro): the short code to enter, the URL to visit, and the poll
/// cadence the frontend's `await_kiro_device_flow` call will honor.
#[derive(Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFlowInfo {
    pub flow_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

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

/// Mask a secret for display: first 3 + last 4 chars, elided in between.
/// Defensive for short keys: the brief's naive head(3)/tail(4) slicing
/// overlaps (and can echo the *entire* key back) once `key.len() < 7`, so
/// short/empty keys get a fixed placeholder instead of being echoed.
fn mask(key: &str) -> String {
    if key.chars().count() < 7 {
        return "••••".to_string();
    }
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}…{tail}", key.chars().take(3).collect::<String>())
}

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
        base_url: desc.and_then(|d| connections::effective_base_url(d, row)),
        models: desc
            .map(|d| connections::effective_models(d, row))
            .unwrap_or_default(),
        key_masked: row.data.api_key.as_deref().map(mask),
        needs_relogin: row.data.needs_relogin.unwrap_or(false),
        claude_cloaking: row.provider == "anthropic-oauth" && claude_cloak::enabled(&row.data),
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

fn quota_http_client() -> R<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })
}

fn provider_http_client() -> R<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })
}

/// Pick out the ids of OTHER kiro connections that are dead
/// (`needs_relogin == Some(true)`) and should be cleared once a fresh kiro
/// connection (`keep_id`) has been persisted. Split out from
/// [`remove_dead_kiro_connections`] so the actual selection logic — the fix
/// for the reconnect-shadow bug below — is unit-testable without a live
/// `Store`. Never selects `keep_id` itself, never selects a healthy kiro row
/// (`needs_relogin` `None`/`false`), and never selects a non-kiro row.
fn dead_kiro_connection_ids(rows: &[ConnectionRow], keep_id: &str) -> Vec<String> {
    rows.iter()
        .filter(|r| r.provider == "kiro" && r.id != keep_id && r.data.needs_relogin == Some(true))
        .map(|r| r.id.clone())
        .collect()
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
        })
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn list_connections(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<ConnectionInfo>> {
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn add_connection(
    cp: State<'_, Arc<ControlPlane>>,
    provider: String,
    label: String,
    api_key: String,
    base_url: Option<String>,
) -> R<Vec<ConnectionInfo>> {
    let desc = registry::descriptor(&provider).ok_or_else(|| CmdError {
        message: format!("unknown provider: {provider}"),
    })?;
    if desc.category != ProviderCategory::ApiKey {
        return Err(CmdError {
            message: format!("{} is coming in a later phase.", desc.name),
        });
    }
    if desc.requires_base_url && base_url.as_deref().map(str::is_empty).unwrap_or(true) {
        return Err(CmdError {
            message: format!("{} requires a base URL", desc.name),
        });
    }
    let now = ryuzi_core::paths::now_ms();
    let mut row = ConnectionRow {
        id: ryuzi_core::paths::new_id(),
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
    refresh_models_best_effort(&cp, &mut row).await;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_connection(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    label: String,
    enabled: bool,
    api_key: Option<String>,
    base_url: Option<String>,
    models: Vec<String>,
    claude_cloaking: Option<bool>,
) -> R<Vec<ConnectionInfo>> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    row.label = label;
    row.enabled = enabled;
    if let Some(k) = api_key {
        // Empty string = keep existing key; UI sends null to keep too.
        if !k.is_empty() {
            row.data.api_key = Some(k);
        }
    }
    row.data.base_url_override = base_url.filter(|s| !s.is_empty());
    row.data.models_override = if models.is_empty() {
        None
    } else {
        Some(models)
    };
    if row.provider == "anthropic-oauth" {
        if let Some(value) = claude_cloaking {
            claude_cloak::set_enabled(&mut row.data, value);
        }
    }
    row.updated_at = ryuzi_core::paths::now_ms();
    connections::update_connection(cp.store(), row.clone()).await?;
    if row.data.models_override.is_none() {
        refresh_models_best_effort(&cp, &mut row).await;
    }
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_connection(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<ConnectionInfo>> {
    connections::remove_connection(cp.store(), &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn move_connection(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    dir: i32,
) -> R<Vec<ConnectionInfo>> {
    connections::move_connection(cp.store(), &id, dir).await?;
    Ok(assemble(&cp).await?)
}

/// Map the probe response to the user-facing verdict: 2xx passes, 401/403
/// blames the API key, any other status is surfaced as-is, and a transport
/// failure (`Err` carries its display text) reads as network trouble.
fn probe_outcome(resp: Result<reqwest::StatusCode, String>) -> TestResult {
    match resp {
        Ok(s) if s.is_success() => TestResult {
            ok: true,
            message: "Connection OK".into(),
        },
        Ok(s) if s.as_u16() == 401 || s.as_u16() == 403 => TestResult {
            ok: false,
            message: "Rejected: the API key looks invalid for this provider.".into(),
        },
        Ok(s) => TestResult {
            ok: false,
            message: format!("Upstream returned HTTP {s}"),
        },
        Err(e) => TestResult {
            ok: false,
            message: format!("Network error: {e}"),
        },
    }
}

fn model_probe_body(format: ApiFormat, model: &str) -> Value {
    match format {
        ApiFormat::OpenAi => json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        }),
        ApiFormat::Anthropic => json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        }),
    }
}

fn model_probe_outcome(model: &str, resp: Result<reqwest::StatusCode, String>) -> TestResult {
    match resp {
        Ok(s) if s.is_success() => TestResult {
            ok: true,
            message: format!("Model {model} OK"),
        },
        Ok(s) if s.as_u16() == 401 || s.as_u16() == 403 => TestResult {
            ok: false,
            message: format!("Model {model} was rejected by provider credentials."),
        },
        Ok(s) => TestResult {
            ok: false,
            message: format!("Model {model} returned HTTP {s}"),
        },
        Err(e) => TestResult {
            ok: false,
            message: format!("Model {model} network error: {e}"),
        },
    }
}

fn chatgpt_account_id(row: &ConnectionRow) -> Option<&str> {
    let data = row.data.provider_specific.as_ref()?;
    data.get("chatgpt_account_id")
        .or_else(|| data.get("chatgptAccountId"))
        .or_else(|| data.get("accountId"))
        .or_else(|| data.get("workspaceId"))
        .and_then(Value::as_str)
}

fn codex_probe_model(model: &str) -> String {
    let mut upstream = model.strip_suffix("-review").unwrap_or(model).to_string();
    for effort in ["xhigh", "high", "medium", "low", "none"] {
        let suffix = format!("-{effort}");
        if upstream.ends_with(&suffix) {
            upstream.truncate(upstream.len() - suffix.len());
            break;
        }
    }
    upstream
}

fn build_model_probe_request(
    http: &reqwest::Client,
    desc: &registry::ProviderDescriptor,
    row: &ConnectionRow,
    model: &str,
) -> anyhow::Result<reqwest::RequestBuilder> {
    if row.provider == "openai-oauth" {
        let token = row.data.access_token.clone().unwrap_or_default();
        let mut req = http
            .post("https://chatgpt.com/backend-api/codex/responses")
            .json(&json!({
                "model": codex_probe_model(model),
                "input": "ping",
                "stream": false,
                "store": false
            }))
            .header("authorization", format!("Bearer {token}"))
            .header("originator", "codex_cli_rs")
            .header("session_id", ryuzi_core::paths::new_id());
        if let Some(account_id) = chatgpt_account_id(row) {
            req = req.header("chatgpt-account-id", account_id);
        }
        return Ok(req);
    }

    let base = connections::effective_base_url(desc, row)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", row.id))?;
    let mut body = model_probe_body(desc.format, model);

    if row.provider == "anthropic-oauth" {
        models::inject_claude_code_system_prompt(&mut body);
        let token = row.data.access_token.clone().unwrap_or_default();
        let session_id = ryuzi_core::paths::new_id();
        let cloaked = claude_cloak::enabled(&row.data);
        if cloaked {
            claude_cloak::apply_request_cloak(&mut body, &token, &session_id);
        }
        let req = http
            .post(format!("{base}/messages?beta=true"))
            .json(&body)
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", models::ANTHROPIC_OAUTH_BETA)
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
            .header("x-app", "cli");
        return Ok(if cloaked {
            claude_cloak::spoof_headers(req, &session_id)
        } else {
            req
        });
    }

    let path = match desc.format {
        ApiFormat::OpenAi => "/chat/completions",
        ApiFormat::Anthropic => "/messages",
    };
    let mut req = http.post(format!("{base}{path}")).json(&body);
    if desc.no_auth {
        if row.provider == "opencode-free" {
            req = req
                .header("authorization", "Bearer public")
                .header("x-opencode-client", "desktop");
        }
        return Ok(req);
    }

    let key = row.data.api_key.clone().unwrap_or_default();
    match desc.auth {
        AuthScheme::XApiKey => Ok(req
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")),
        AuthScheme::Bearer => Ok(req.header("authorization", format!("Bearer {key}"))),
        AuthScheme::None => Ok(req),
    }
}

async fn test_model_once(
    http: &reqwest::Client,
    desc: &registry::ProviderDescriptor,
    row: &ConnectionRow,
    model: &str,
) -> anyhow::Result<reqwest::StatusCode> {
    Ok(build_model_probe_request(http, desc, row, model)?
        .send()
        .await?
        .status())
}

async fn test_model_status(
    store: &Arc<ryuzi_core::Store>,
    http: &reqwest::Client,
    desc: &registry::ProviderDescriptor,
    row: &mut ConnectionRow,
    model: &str,
) -> anyhow::Result<reqwest::StatusCode> {
    if connections::is_oauth(row) {
        if let Err(err) = oauth::refresh::ensure_fresh(store, http, row).await {
            if row.data.needs_relogin == Some(true) {
                return Err(err);
            }
        }
    }

    let status = test_model_once(http, desc, row, model).await?;
    if connections::is_oauth(row)
        && matches!(status.as_u16(), 401 | 403)
        && row.data.refresh_token.is_some()
    {
        oauth::refresh::force_refresh(store, http, row).await?;
        return test_model_once(http, desc, row, model).await;
    }
    Ok(status)
}

/// Hit the upstream's model-list endpoint to distinguish bad credentials
/// (401/403) from network trouble, and persist the discovered model ids.
#[tauri::command]
#[specta::specta]
pub async fn test_connection(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<TestResult> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    let desc = registry::descriptor(&row.provider).ok_or_else(|| CmdError {
        message: format!("unknown provider: {}", row.provider),
    })?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    let result = match models::fetch_connection_models(cp.store(), &client, desc, &mut row).await {
        Ok((status, discovered)) => {
            if status.is_success() {
                if !discovered.is_empty() {
                    row.data.models_override = Some(discovered);
                    row.updated_at = ryuzi_core::paths::now_ms();
                    let _ = connections::update_connection(cp.store(), row).await;
                }
            }
            probe_outcome(Ok(status))
        }
        Err(e) => probe_outcome(Err(e.to_string())),
    };
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn test_connection_model(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    model: String,
) -> R<TestResult> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Ok(TestResult {
            ok: false,
            message: "Model id is empty".into(),
        });
    }
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    let desc = registry::descriptor(&row.provider).ok_or_else(|| CmdError {
        message: format!("unknown provider: {}", row.provider),
    })?;
    let client = provider_http_client()?;
    let result = test_model_status(cp.store(), &client, desc, &mut row, &model)
        .await
        .map_or_else(
            |e| model_probe_outcome(&model, Err(e.to_string())),
            |status| model_probe_outcome(&model, Ok(status)),
        );
    Ok(result)
}

/// Re-fetch the live model list for every enabled connection in a vendor
/// family, persisting discoveries. Unlike the add/update-time best-effort
/// refresh, failures are returned to the UI instead of being swallowed.
#[tauri::command]
#[specta::specta]
pub async fn refresh_provider_models(
    cp: State<'_, Arc<ControlPlane>>,
    family: String,
) -> R<Vec<RefreshModelsResult>> {
    let http = quota_http_client()?;
    let rows = connections::list_connections(cp.store()).await?;
    let mut out = Vec::new();
    for mut row in rows {
        let in_family = registry::family_of(&row.provider)
            .map(|f| f == family)
            .unwrap_or(row.provider == family);
        if !in_family || !row.enabled {
            continue;
        }
        let label = if row.label.is_empty() {
            row.provider.clone()
        } else {
            row.label.clone()
        };
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

#[tauri::command]
#[specta::specta]
pub async fn connection_provider_quota(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<ProviderQuotaInfo> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    let http = quota_http_client()?;
    Ok(quota::fetch_provider_quota(cp.store(), &http, &mut row).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn reset_codex_credit(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<CodexResetCreditResult> {
    let mut row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    let http = quota_http_client()?;
    Ok(quota::consume_codex_reset_credit(cp.store(), &http, &mut row).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn list_model_routes(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<ModelRouteInfo>> {
    Ok(routes::list_model_routes(cp.store()).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn save_model_route(
    cp: State<'_, Arc<ControlPlane>>,
    route: ModelRouteInfo,
) -> R<Vec<ModelRouteInfo>> {
    routes::save_model_route(cp.store(), route).await?;
    Ok(routes::list_model_routes(cp.store()).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_model_route(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<ModelRouteInfo>> {
    routes::delete_model_route(cp.store(), &id).await?;
    Ok(routes::list_model_routes(cp.store()).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn provider_account_route(
    cp: State<'_, Arc<ControlPlane>>,
    provider: String,
) -> R<ProviderAccountRouteInfo> {
    Ok(routes::provider_account_route(cp.store(), &provider).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_provider_account_route(
    cp: State<'_, Arc<ControlPlane>>,
    provider: String,
    strategy: ModelRouteStrategy,
) -> R<ProviderAccountRouteInfo> {
    Ok(routes::save_provider_account_route(cp.store(), &provider, strategy).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16) -> reqwest::StatusCode {
        reqwest::StatusCode::from_u16(code).unwrap()
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
    fn model_probe_body_matches_provider_format() {
        let openai = model_probe_body(ApiFormat::OpenAi, "gpt-test");
        assert_eq!(openai["model"], "gpt-test");
        assert_eq!(openai["messages"][0]["content"], "ping");
        assert_eq!(openai["max_tokens"], 1);

        let anthropic = model_probe_body(ApiFormat::Anthropic, "claude-test");
        assert_eq!(anthropic["model"], "claude-test");
        assert_eq!(anthropic["messages"][0]["content"], "ping");
        assert_eq!(anthropic["max_tokens"], 1);
    }

    #[test]
    fn anthropic_oauth_model_probe_injects_claude_code_system_prompt() {
        let http = reqwest::Client::new();
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let row = ConnectionRow {
            id: "c1".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "Claude Code".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-claude".into()),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        };
        let req = build_model_probe_request(&http, desc, &row, "claude-opus-4-8")
            .unwrap()
            .build()
            .unwrap();
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();

        assert_eq!(
            sent["system"][0]["text"],
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
    }

    #[test]
    fn connection_info_exposes_claude_cloaking_for_anthropic_oauth_only() {
        let mut row = ConnectionRow {
            id: "c1".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "Claude Code".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                provider_specific: Some(json!({"claudeCloaking": true})),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        };
        assert!(to_info(&row).claude_cloaking);

        row.provider = "openai-oauth".into();
        assert!(!to_info(&row).claude_cloaking);
    }

    #[test]
    fn model_probe_outcome_mentions_model_id() {
        let r = model_probe_outcome("gpt-test", Ok(status(200)));
        assert!(r.ok);
        assert_eq!(r.message, "Model gpt-test OK");

        let r = model_probe_outcome("gpt-test", Ok(status(404)));
        assert!(!r.ok);
        assert_eq!(r.message, "Model gpt-test returned HTTP 404 Not Found");
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
}

/// Drive the full interactive OAuth flow: binds a loopback listener, opens
/// the provider's authorize URL in the system browser via
/// `tauri-plugin-opener`, and awaits the callback (up to 5 minutes) before
/// persisting the resulting connection.
#[tauri::command]
#[specta::specta]
pub async fn connect_oauth(
    cp: State<'_, Arc<ControlPlane>>,
    app: tauri::AppHandle,
    provider: String,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let http = reqwest::Client::new();
    let app2 = app.clone();
    let provider_for_event = provider.clone();
    let mut row = oauth::callback::run_flow(
        cp.store(),
        &http,
        &provider,
        &label,
        None,
        move |url| {
            let _ = OauthAuthorizeUrlMsg {
                provider: provider_for_event,
                authorize_url: url.to_string(),
            }
            .emit(&app2);
            let _ = app2.opener().open_url(url.to_string(), None::<&str>);
        },
        std::time::Duration::from_secs(300),
    )
    .await
    .map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    refresh_models_best_effort(&cp, &mut row).await;
    Ok(assemble(&cp).await?)
}

/// Reconnect an existing `needs_relogin` OAuth connection: drives the same
/// browser flow as [`connect_oauth`], but updates the connection in place
/// (same id/priority/label) instead of inserting a new row — otherwise the
/// stale, dead connection would keep shadowing the fresh one in
/// `route_model`'s `priority ASC` ordering.
#[tauri::command]
#[specta::specta]
pub async fn reconnect_oauth(
    cp: State<'_, Arc<ControlPlane>>,
    app: tauri::AppHandle,
    connection_id: String,
) -> R<Vec<ConnectionInfo>> {
    let existing = connections::get_connection(cp.store(), &connection_id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {connection_id}"),
        })?;
    let http = reqwest::Client::new();
    let app2 = app.clone();
    let provider_for_event = existing.provider.clone();
    let mut row = oauth::callback::run_flow(
        cp.store(),
        &http,
        &existing.provider,
        &existing.label,
        Some(&connection_id),
        move |url| {
            let _ = OauthAuthorizeUrlMsg {
                provider: provider_for_event,
                authorize_url: url.to_string(),
            }
            .emit(&app2);
            let _ = app2.opener().open_url(url.to_string(), None::<&str>);
        },
        std::time::Duration::from_secs(300),
    )
    .await
    .map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    refresh_models_best_effort(&cp, &mut row).await;
    Ok(assemble(&cp).await?)
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
    cp: State<'_, Arc<ControlPlane>>,
    provider: String,
    label: String,
    verifier: String,
    state: String,
    pasted: String,
    redirect_uri: String,
) -> R<Vec<ConnectionInfo>> {
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
    .map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    refresh_models_best_effort(&cp, &mut row).await;
    Ok(assemble(&cp).await?)
}

/// Add a `no_auth` connection for a Free-category provider — refuses any
/// other category (those need the api-key or OAuth flows above).
#[tauri::command]
#[specta::specta]
pub async fn add_free_connection(
    cp: State<'_, Arc<ControlPlane>>,
    provider: String,
    label: String,
) -> R<Vec<ConnectionInfo>> {
    let desc = registry::descriptor(&provider).ok_or_else(|| CmdError {
        message: format!("unknown provider: {provider}"),
    })?;
    if desc.category != ProviderCategory::Free {
        return Err(CmdError {
            message: format!("{} is not a free provider", desc.name),
        });
    }
    if desc.device_flow.is_some() {
        return Err(CmdError {
            message: format!(
                "{} uses device login — connect it from the provider list.",
                desc.name
            ),
        });
    }
    let now = ryuzi_core::paths::now_ms();
    let mut row = ConnectionRow {
        id: ryuzi_core::paths::new_id(),
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
    refresh_models_best_effort(&cp, &mut row).await;
    Ok(assemble(&cp).await?)
}

/// Start Kiro's AWS SSO-OIDC device-code flow: registers a public client,
/// starts a device authorization, opens the browser to the verification URL,
/// and stashes the in-flight state under a fresh `flow_id` for
/// [`await_kiro_device_flow`] to poll. Does not touch the store — nothing is
/// persisted until the user completes the browser step.
#[tauri::command]
#[specta::specta]
pub async fn start_kiro_device_flow(
    _cp: State<'_, Arc<ControlPlane>>,
    app: tauri::AppHandle,
) -> R<DeviceFlowInfo> {
    let http = reqwest::Client::new();
    let cfg = registry::device_flow_config("kiro").ok_or_else(|| CmdError {
        message: "kiro device flow is not configured".into(),
    })?;
    let client = oauth::device::register_client(&http, cfg)
        .await
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    let auth = oauth::device::start_device_authorization(&http, cfg, &client)
        .await
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;

    let flow_id = ryuzi_core::paths::new_id();
    let deadline_ms = ryuzi_core::paths::now_ms() + auth.expires_in * 1000;
    flows().lock().unwrap_or_else(|e| e.into_inner()).insert(
        flow_id.clone(),
        KiroFlowState {
            client,
            device_code: auth.device_code.clone(),
            interval: auth.interval,
            deadline_ms,
        },
    );

    let _ = app
        .opener()
        .open_url(auth.verification_uri_complete.clone(), None::<&str>);

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
#[tauri::command]
#[specta::specta]
pub async fn await_kiro_device_flow(
    cp: State<'_, Arc<ControlPlane>>,
    label: String,
    flow_id: String,
) -> R<Vec<ConnectionInfo>> {
    let KiroFlowState {
        client,
        device_code,
        mut interval,
        deadline_ms,
    } = flows()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&flow_id)
        .ok_or_else(|| CmdError {
            message: "device sign-in flow not found — start again".into(),
        })?;

    let http = reqwest::Client::new();
    let cfg = registry::device_flow_config("kiro").ok_or_else(|| CmdError {
        message: "kiro device flow is not configured".into(),
    })?;

    let tokens = loop {
        if ryuzi_core::paths::now_ms() > deadline_ms {
            return Err(CmdError {
                message: "device code expired — start again".into(),
            });
        }
        tokio::time::sleep(Duration::from_secs(interval.max(1) as u64)).await;
        match oauth::device::poll_token_once(&http, cfg.token_url, &client, &device_code)
            .await
            .map_err(|e| CmdError {
                message: e.to_string(),
            })? {
            oauth::device::PollOutcome::Pending => continue,
            oauth::device::PollOutcome::SlowDown => {
                interval = (interval + 5).min(30);
                continue;
            }
            oauth::device::PollOutcome::Denied => {
                return Err(CmdError {
                    message: "sign-in was denied".into(),
                });
            }
            oauth::device::PollOutcome::Expired => {
                return Err(CmdError {
                    message: "device code expired — start again".into(),
                });
            }
            oauth::device::PollOutcome::Ready(tokens) => break tokens,
        }
    };

    let profile_arn = oauth::device::resolve_profile_arn(&http, &tokens.access_token)
        .await
        .unwrap_or_else(|| connections::default_profile_arn("builder-id").to_string());

    let now = ryuzi_core::paths::now_ms();
    let new_id = ryuzi_core::paths::new_id();
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
    remove_dead_kiro_connections(&cp, &new_id).await?;
    Ok(assemble(&cp).await?)
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
#[tauri::command]
#[specta::specta]
pub async fn import_kiro_token(
    cp: State<'_, Arc<ControlPlane>>,
    label: String,
) -> R<Vec<ConnectionInfo>> {
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

    let now = ryuzi_core::paths::now_ms();
    let new_id = ryuzi_core::paths::new_id();
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
        .map_err(|e| CmdError {
            message: format!("imported Kiro token looks dead: {e}"),
        })?;

    connections::add_connection(cp.store(), row).await?;
    // Import succeeded — clear any OTHER dead kiro row so it doesn't keep
    // shadowing this fresh connection in `route_model`. See
    // `remove_dead_kiro_connections`.
    remove_dead_kiro_connections(&cp, &new_id).await?;
    Ok(assemble(&cp).await?)
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

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
