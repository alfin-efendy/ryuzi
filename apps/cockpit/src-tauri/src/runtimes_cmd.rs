//! Runtime screen commands: catalog + persisted config + real detection
//! snapshots, enriched with the latest released version from npm.
//!
//! `list_runtimes` is fast (reads the persisted snapshot); `refresh_runtimes`
//! re-probes binaries and asks npm, then persists the snapshot in settings.

use crate::error::CmdError;
use ryuzi_core::llm_router::{keys, server::RouterServer};
use ryuzi_core::runtime_config::{self, ConfigStatus, EndpointInfo, RuntimeMapping};
use ryuzi_core::runtimes::{self, RuntimeConfig};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

type R<T> = Result<T, CmdError>;

// legacy storage keys — see Task 1 note in the plan
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
pub struct RuntimeInfo {
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

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<RuntimeInfo>> {
    let snapshots = read_snapshots(cp).await;
    let configs = runtimes::list_configs(cp.store()).await?;
    let default_agent = cp
        .store()
        .get_setting("default_agent")
        .await?
        .unwrap_or_else(|| "claude".to_string());

    let mut out = Vec::new();
    for desc in runtimes::CATALOG {
        let snap = snapshots.get(desc.id).cloned().unwrap_or_default();
        let cfg = configs.iter().find(|c| c.id == desc.id);
        // The native runtime runs in-process, so it's always "available"
        // regardless of any binary on PATH.
        let is_native = desc.id == "native";
        let binary_path = if is_native {
            Some("in-process".to_string())
        } else {
            snap.binary_path.clone()
        };
        let installed_version = if is_native {
            Some(env!("CARGO_PKG_VERSION").to_string())
        } else {
            snap.installed_version.clone()
        };
        let detected = binary_path.is_some();
        let mut models: Vec<String> = desc.models.iter().map(|m| m.to_string()).collect();
        if is_native {
            // The native runtime has no fixed catalog: its selectable models
            // are the user's enabled routes + provider connections, so the
            // picker reflects what the router can actually reach today.
            models = ryuzi_core::llm_router::client::selectable_native_models(cp.store()).await;
        } else if models.is_empty() {
            models = snap.local_models.clone();
        }
        let model = cfg
            .and_then(|c| c.model.clone())
            .or_else(|| models.first().cloned())
            .unwrap_or_default();
        let tiers = runtimes::list_tiers(cp.store(), desc.id)
            .await?
            .into_iter()
            .map(|t| TierInfo {
                id: t.tier_id,
                label: t.label,
                value: t.value,
                combo: t.combo,
            })
            .collect();
        out.push(RuntimeInfo {
            id: desc.id.to_string(),
            name: desc.name.to_string(),
            color: desc.color.to_string(),
            initial: desc.initial.to_string(),
            connection: desc.connection.to_string(),
            binary_path,
            installed_version,
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
            // The native runtime and claude-code have working session harnesses.
            runnable: desc.id == "claude" || is_native,
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
pub async fn list_runtimes(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<RuntimeInfo>> {
    Ok(assemble(&cp).await?)
}

/// Re-probe every catalog agent (PATH + --version + npm latest + local model
/// list for ollama), persist the snapshot, and return the fresh assembly.
#[tauri::command]
#[specta::specta]
pub async fn refresh_runtimes(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<RuntimeInfo>> {
    let mut snapshots = std::collections::HashMap::new();
    for desc in runtimes::CATALOG {
        // Native runs in-process — nothing to probe on PATH; assemble marks it
        // available directly.
        if desc.id == "native" {
            continue;
        }
        let det = runtimes::detect(desc.binary).await;
        let latest = match desc.npm_package {
            Some(pkg) => npm_latest(pkg).await,
            None => None,
        };
        let local_models = match (&det.binary_path, desc.id) {
            (Some(path), "ollama") => runtimes::ollama_models(path).await,
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
pub async fn update_runtime_config(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
    model: Option<String>,
    perm_mode: String,
    flags: String,
) -> R<Vec<RuntimeInfo>> {
    runtimes::upsert_config(
        cp.store(),
        RuntimeConfig {
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
pub async fn set_runtime_tier(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    tier_id: String,
    value: Option<String>,
    combo: bool,
) -> R<Vec<RuntimeInfo>> {
    runtimes::set_tier(cp.store(), &id, &tier_id, value, combo).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_default_runtime(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<RuntimeInfo>> {
    cp.store().set_setting("default_agent", &id).await?;
    Ok(assemble(&cp).await?)
}

/// Fire-and-forget npm update; progress streams via CoreEvent
/// RuntimeUpdateLog / RuntimeUpdateDone, then a refreshed snapshot matters —
/// the UI calls refreshRuntimes() on Done.
#[tauri::command]
#[specta::specta]
pub async fn update_runtime(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<()> {
    let desc = runtimes::descriptor(&id).ok_or_else(|| CmdError {
        message: format!("unknown runtime: {id}"),
    })?;
    let Some(pkg) = desc.npm_package else {
        return Err(CmdError {
            message: format!("{} is not npm-managed", desc.name),
        });
    };
    let events = cp.events_sender();
    let pkg = pkg.to_string();
    tauri::async_runtime::spawn(async move {
        let res = ryuzi_core::runtimes::run_npm_update(events.clone(), &id, &pkg).await;
        let (ok, message) = match res {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("npm exited with a non-zero status".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };
        let _ = events.send(ryuzi_core::CoreEvent::RuntimeUpdateDone {
            runtime_id: id,
            ok,
            message,
        });
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Endpoint config apply/reset (spec §5) — write/remove the local router's
// base URL + key into native CLI-tool configs (claude/codex/opencode only).
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigStatusInfo {
    pub config_path: String,
    pub exists: bool,
    pub configured: bool,
    /// False for runtimes without an F1 handler (gemini, ollama).
    pub supported: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeMappingArg {
    pub model: String,
    pub opus: Option<String>,
    pub sonnet: Option<String>,
    pub haiku: Option<String>,
    pub models: Vec<String>,
}

fn home() -> Result<std::path::PathBuf, CmdError> {
    dirs::home_dir().ok_or_else(|| CmdError {
        message: "cannot resolve home directory".into(),
    })
}

fn status_of(id: &str, home: &std::path::Path) -> Result<RuntimeConfigStatusInfo, CmdError> {
    let st: Option<ConfigStatus> = match id {
        "claude" => Some(runtime_config::claude_status(home).map_err(err)?),
        "codex" => Some(runtime_config::codex_status(home).map_err(err)?),
        "opencode" => Some(runtime_config::opencode_status(home).map_err(err)?),
        _ => None,
    };
    Ok(match st {
        Some(s) => RuntimeConfigStatusInfo {
            config_path: s.config_path,
            exists: s.exists,
            configured: s.configured,
            supported: true,
        },
        None => RuntimeConfigStatusInfo {
            config_path: String::new(),
            exists: false,
            configured: false,
            supported: false,
        },
    })
}

fn err(e: anyhow::Error) -> CmdError {
    CmdError {
        message: e.to_string(),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn runtime_config_status(id: String) -> Result<RuntimeConfigStatusInfo, CmdError> {
    status_of(&id, &home()?)
}

/// Guard (spec §5): refuse to write configs that point at a dead endpoint —
/// the server must be running and at least one endpoint key must exist.
#[tauri::command]
#[specta::specta]
pub async fn apply_runtime_config(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
    id: String,
    mapping: RuntimeMappingArg,
) -> Result<RuntimeConfigStatusInfo, CmdError> {
    let st = srv.status();
    if !st.running {
        return Err(CmdError {
            message: "The endpoint server is not running. Start it in Models → Endpoint first."
                .into(),
        });
    }
    let key = keys::first_key(cp.store())
        .await
        .map_err(err)?
        .ok_or_else(|| CmdError {
            message: "No endpoint API key exists. Create one in Models → Endpoint first.".into(),
        })?;
    if mapping.model.trim().is_empty() {
        return Err(CmdError {
            message:
                "No model selected. Add an enabled provider connection in Models → Providers first."
                    .into(),
        });
    }
    let ep = EndpointInfo {
        base_url: format!("http://127.0.0.1:{}", st.port),
        api_key: key.key,
    };
    let m = RuntimeMapping {
        model: mapping.model,
        opus: mapping.opus,
        sonnet: mapping.sonnet,
        haiku: mapping.haiku,
        models: mapping.models,
    };
    let home = home()?;
    match id.as_str() {
        "claude" => runtime_config::claude_apply(&home, &ep, &m).map_err(err)?,
        "codex" => runtime_config::codex_apply(&home, &ep, &m).map_err(err)?,
        "opencode" => runtime_config::opencode_apply(&home, &ep, &m).map_err(err)?,
        other => {
            return Err(CmdError {
                message: format!("config apply is not supported for '{other}' yet"),
            })
        }
    }
    status_of(&id, &home)
}

#[tauri::command]
#[specta::specta]
pub async fn reset_runtime_config(id: String) -> Result<RuntimeConfigStatusInfo, CmdError> {
    let home = home()?;
    match id.as_str() {
        "claude" => runtime_config::claude_reset(&home).map_err(err)?,
        "codex" => runtime_config::codex_reset(&home).map_err(err)?,
        "opencode" => runtime_config::opencode_reset(&home).map_err(err)?,
        other => {
            return Err(CmdError {
                message: format!("config apply is not supported for '{other}' yet"),
            })
        }
    }
    status_of(&id, &home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        ControlPlane::new(store, ryuzi_core::Registries::new()).await
    }

    #[tokio::test]
    async fn native_runtime_is_listed_and_available() {
        let cp = test_cp().await;
        let list = assemble(&cp).await.unwrap();
        let native = list
            .iter()
            .find(|r| r.id == "native")
            .expect("native runtime must appear in the Runtime list");
        assert_eq!(native.name, "Native (ryuzi)");
        // Available (in-process) without any binary on PATH, and runnable.
        assert!(native.binary_path.is_some(), "native must be available");
        assert!(native.installed_version.is_some());
        assert!(native.runnable, "native must be runnable");
    }
}
