//! Providers tab commands: catalog + credentialed connections CRUD + test.
use crate::error::CmdError;
use ryuzi_core::llm_router::connections::{self, ConnectionData, ConnectionRow};
use ryuzi_core::llm_router::oauth;
use ryuzi_core::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderCategory};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

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
    }
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<ConnectionInfo>> {
    Ok(connections::list_connections(cp.store())
        .await?
        .iter()
        .map(to_info)
        .collect())
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
    connections::add_connection(
        cp.store(),
        ConnectionRow {
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
        },
    )
    .await?;
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
    connections::update_connection(cp.store(), row).await?;
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

/// Hit the upstream's model-list (openai) / a 1-token message (anthropic)
/// to distinguish bad key (401/403) from network trouble.
#[tauri::command]
#[specta::specta]
pub async fn test_connection(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<TestResult> {
    let row = connections::get_connection(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown connection: {id}"),
        })?;
    let desc = registry::descriptor(&row.provider).ok_or_else(|| CmdError {
        message: format!("unknown provider: {}", row.provider),
    })?;
    let Some(base) = connections::effective_base_url(desc, &row) else {
        return Ok(TestResult {
            ok: false,
            message: "no base URL configured".into(),
        });
    };
    let key = row.data.api_key.clone().unwrap_or_default();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    let resp = match desc.format {
        ApiFormat::OpenAi => {
            let mut r = client.get(format!("{base}/models"));
            if desc.auth == AuthScheme::Bearer {
                r = r.header("authorization", format!("Bearer {key}"));
            }
            r.send().await
        }
        ApiFormat::Anthropic => client
            .post(format!("{base}/messages"))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({"model": desc.models.first().copied().unwrap_or("claude-haiku-4-5"),
                                       "max_tokens": 1,
                                       "messages": [{"role": "user", "content": "ping"}]}))
            .send()
            .await,
    };
    Ok(probe_outcome(
        resp.map(|r| r.status()).map_err(|e| e.to_string()),
    ))
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
    oauth::callback::run_flow(
        cp.store(),
        &http,
        &provider,
        &label,
        None,
        move |url| {
            let _ = app2.opener().open_url(url.to_string(), None::<&str>);
        },
        std::time::Duration::from_secs(300),
    )
    .await
    .map_err(|e| CmdError {
        message: e.to_string(),
    })?;
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
    oauth::callback::run_flow(
        cp.store(),
        &http,
        &existing.provider,
        &existing.label,
        Some(&connection_id),
        move |url| {
            let _ = app2.opener().open_url(url.to_string(), None::<&str>);
        },
        std::time::Duration::from_secs(300),
    )
    .await
    .map_err(|e| CmdError {
        message: e.to_string(),
    })?;
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
    oauth::callback::complete_manual(
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
    connections::add_connection(
        cp.store(),
        ConnectionRow {
            id: ryuzi_core::paths::new_id(),
            provider,
            auth_type: "free".into(),
            label,
            priority: 0,
            enabled: true,
            data: ConnectionData::default(),
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
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
    connections::add_connection(
        cp.store(),
        ConnectionRow {
            id: ryuzi_core::paths::new_id(),
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
    let mut row = ConnectionRow {
        id: ryuzi_core::paths::new_id(),
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
    oauth::refresh::force_refresh(cp.store(), &http, &mut row)
        .await
        .map_err(|e| CmdError {
            message: format!("imported Kiro token looks dead: {e}"),
        })?;

    connections::add_connection(cp.store(), row).await?;
    Ok(assemble(&cp).await?)
}
