//! Agents screen commands: catalog + persisted config + real detection
//! snapshots, enriched with the latest released version from npm.
//!
//! `list_agents` is fast (reads the persisted snapshot); `refresh_agents`
//! re-probes binaries and asks npm, then persists the snapshot in settings.

use crate::error::CmdError;
use ryuzi_core::agents::{self, AgentConfig};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

type R<T> = Result<T, CmdError>;

const SNAPSHOT_KEY: &str = "agents_snapshot";

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TierInfo {
    pub id: String,
    pub label: String,
    pub value: Option<String>,
    pub combo: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub color: String,
    pub initial: String,
    pub connection: String,
    pub binary_path: Option<String>,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub npm_package: Option<String>,
    pub models: Vec<String>,
    pub enabled: bool,
    pub model: String,
    pub perm_mode: String,
    pub flags: String,
    pub tiers: Vec<TierInfo>,
    pub is_default: bool,
    /// Whether Cockpit has a session harness for this agent today.
    pub runnable: bool,
}

/// Persisted probe results per agent (settings KV, JSON).
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    binary_path: Option<String>,
    installed_version: Option<String>,
    latest_version: Option<String>,
    /// Locally installed models (ollama only).
    local_models: Vec<String>,
    checked_at: i64,
}

async fn read_snapshots(cp: &ControlPlane) -> std::collections::HashMap<String, Snapshot> {
    let raw = cp.store().get_setting(SNAPSHOT_KEY).await.ok().flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<AgentInfo>> {
    let snapshots = read_snapshots(cp).await;
    let configs = agents::list_configs(cp.store()).await?;
    let default_agent = cp
        .store()
        .get_setting("default_agent")
        .await?
        .unwrap_or_else(|| "claude".to_string());

    let mut out = Vec::new();
    for desc in agents::CATALOG {
        let snap = snapshots.get(desc.id).cloned().unwrap_or_default();
        let cfg = configs.iter().find(|c| c.id == desc.id);
        let detected = snap.binary_path.is_some();
        let mut models: Vec<String> = desc.models.iter().map(|m| m.to_string()).collect();
        if models.is_empty() {
            models = snap.local_models.clone();
        }
        let model = cfg
            .and_then(|c| c.model.clone())
            .or_else(|| models.first().cloned())
            .unwrap_or_default();
        let tiers = agents::list_tiers(cp.store(), desc.id)
            .await?
            .into_iter()
            .map(|t| TierInfo {
                id: t.tier_id,
                label: t.label,
                value: t.value,
                combo: t.combo,
            })
            .collect();
        out.push(AgentInfo {
            id: desc.id.to_string(),
            name: desc.name.to_string(),
            color: desc.color.to_string(),
            initial: desc.initial.to_string(),
            connection: desc.connection.to_string(),
            binary_path: snap.binary_path,
            installed_version: snap.installed_version,
            latest_version: snap.latest_version,
            npm_package: desc.npm_package.map(|s| s.to_string()),
            models,
            // Zero-config default: detected agents start enabled.
            enabled: cfg.map(|c| c.enabled).unwrap_or(detected),
            model,
            perm_mode: cfg
                .map(|c| c.perm_mode.clone())
                .unwrap_or_else(|| "ask".into()),
            flags: cfg.map(|c| c.flags.clone()).unwrap_or_default(),
            tiers,
            is_default: default_agent == desc.id,
            runnable: desc.id == "claude",
        });
    }
    Ok(out)
}

async fn npm_latest(pkg: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .ok()?;
    let url = format!("https://registry.npmjs.org/{pkg}/latest");
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    Some(v.get("version")?.as_str()?.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn list_agents(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<AgentInfo>> {
    Ok(assemble(&cp).await?)
}

/// Re-probe every catalog agent (PATH + --version + npm latest + local model
/// list for ollama), persist the snapshot, and return the fresh assembly.
#[tauri::command]
#[specta::specta]
pub async fn refresh_agents(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<AgentInfo>> {
    let mut snapshots = std::collections::HashMap::new();
    for desc in agents::CATALOG {
        let det = agents::detect(desc.binary).await;
        let latest = match desc.npm_package {
            Some(pkg) => npm_latest(pkg).await,
            None => None,
        };
        let local_models = match (&det.binary_path, desc.id) {
            (Some(path), "ollama") => agents::ollama_models(path).await,
            _ => vec![],
        };
        snapshots.insert(
            desc.id.to_string(),
            Snapshot {
                binary_path: det.binary_path,
                installed_version: det.installed_version,
                latest_version: latest,
                local_models,
                checked_at: ryuzi_core::paths::now_ms(),
            },
        );
    }
    let json = serde_json::to_string(&snapshots).map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    cp.store().set_setting(SNAPSHOT_KEY, &json).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_agent(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
    model: Option<String>,
    perm_mode: String,
    flags: String,
) -> R<Vec<AgentInfo>> {
    agents::upsert_config(
        cp.store(),
        AgentConfig {
            id,
            enabled,
            model,
            perm_mode,
            flags,
        },
    )
    .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_agent_tier(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    tier_id: String,
    value: Option<String>,
    combo: bool,
) -> R<Vec<AgentInfo>> {
    agents::set_tier(cp.store(), &id, &tier_id, value, combo).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_default_agent(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<AgentInfo>> {
    cp.store().set_setting("default_agent", &id).await?;
    Ok(assemble(&cp).await?)
}
