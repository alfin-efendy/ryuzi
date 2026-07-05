//! Providers tab commands: catalog + credentialed connections CRUD + test.
use crate::error::CmdError;
use crate::events::OauthAuthorizeUrlMsg;
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
use std::sync::Arc;
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event as _;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub color: String,
    pub initial: String,
    pub category: String,
    pub format: String,
    pub requires_base_url: bool,
    pub models: Vec<String>,
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
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ManualStartInfo {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
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

#[tauri::command]
#[specta::specta]
pub async fn list_provider_catalog() -> R<Vec<CatalogEntry>> {
    Ok(registry::CATALOG
        .iter()
        .map(|d| CatalogEntry {
            id: d.id.into(),
            name: d.name.into(),
            color: d.color.into(),
            initial: d.initial.into(),
            category: match d.category {
                ProviderCategory::ApiKey => "api_key".into(),
                ProviderCategory::OAuth => "oauth".into(),
                ProviderCategory::Free => "free".into(),
            },
            format: match d.format {
                ApiFormat::Anthropic => "anthropic".into(),
                ApiFormat::OpenAi => "openai".into(),
            },
            requires_base_url: d.requires_base_url,
            models: d.models.iter().map(|s| s.to_string()).collect(),
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
    let body = model_probe_body(desc.format, model);

    if row.provider == "anthropic-oauth" {
        let token = row.data.access_token.clone().unwrap_or_default();
        return Ok(http
            .post(format!("{base}/messages?beta=true"))
            .json(&body)
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", models::ANTHROPIC_OAUTH_BETA)
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
            .header("x-app", "cli"));
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
    fn model_probe_outcome_mentions_model_id() {
        let r = model_probe_outcome("gpt-test", Ok(status(200)));
        assert!(r.ok);
        assert_eq!(r.message, "Model gpt-test OK");

        let r = model_probe_outcome("gpt-test", Ok(status(404)));
        assert!(!r.ok);
        assert_eq!(r.message, "Model gpt-test returned HTTP 404 Not Found");
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
