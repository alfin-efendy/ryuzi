//! Providers tab commands: catalog + credentialed connections CRUD + test.
use crate::error::CmdError;
use ryuzi_core::llm_router::connections::{self, ConnectionData, ConnectionRow};
use ryuzi_core::llm_router::oauth;
use ryuzi_core::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderCategory};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
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
    Ok(match resp {
        Ok(r) if r.status().is_success() => TestResult {
            ok: true,
            message: "Connection OK".into(),
        },
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => TestResult {
            ok: false,
            message: "Rejected: the API key looks invalid for this provider.".into(),
        },
        Ok(r) => TestResult {
            ok: false,
            message: format!("Upstream returned HTTP {}", r.status()),
        },
        Err(e) => TestResult {
            ok: false,
            message: format!("Network error: {e}"),
        },
    })
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
