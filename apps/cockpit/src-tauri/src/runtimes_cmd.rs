//! Runtime screen commands: thin proxies to the engine daemon's runtimes RPC
//! family (catalog + persisted config + real detection snapshots, enriched
//! with the latest released version from npm). `runtime_config_status` and
//! `reset_runtime_config` stay local — pure home-config reads/writes that
//! never touch the engine's `ControlPlane`.

use crate::engine::EngineClient;
use crate::error::CmdError;
use ryuzi_core::runtime_config::{self, ConfigStatus};
use std::sync::Arc;
use tauri::State;

// `TierInfo` is only reachable transitively (as a field of
// `RuntimeInfo::tiers`) but is re-exported by name anyway for a complete,
// documented DTO surface; specta still emits it via the type graph either
// way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{
    RuntimeConfigStatusInfo, RuntimeInfo, RuntimeMappingArg, TierInfo,
};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_runtimes(engine: Engine<'_>) -> R<Vec<RuntimeInfo>> {
    engine.rpc("list_runtimes", serde_json::json!({})).await
}

/// Re-probe every catalog agent (PATH + --version + npm latest + local model
/// list for ollama), persist the snapshot, and return the fresh assembly.
#[tauri::command]
#[specta::specta]
pub async fn refresh_runtimes(engine: Engine<'_>) -> R<Vec<RuntimeInfo>> {
    engine.rpc("refresh_runtimes", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn update_runtime_config(
    engine: Engine<'_>,
    id: String,
    enabled: bool,
    model: Option<String>,
    perm_mode: String,
    flags: String,
) -> R<Vec<RuntimeInfo>> {
    engine
        .rpc(
            "update_runtime_config",
            serde_json::json!({
                "id": id, "enabled": enabled, "model": model,
                "perm_mode": perm_mode, "flags": flags,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_runtime_tier(
    engine: Engine<'_>,
    id: String,
    tier_id: String,
    value: Option<String>,
    combo: bool,
) -> R<Vec<RuntimeInfo>> {
    engine
        .rpc(
            "set_runtime_tier",
            serde_json::json!({ "id": id, "tier_id": tier_id, "value": value, "combo": combo }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_default_runtime(engine: Engine<'_>, id: String) -> R<Vec<RuntimeInfo>> {
    engine
        .rpc("set_default_runtime", serde_json::json!({ "id": id }))
        .await
}

/// Fire-and-forget npm update; progress streams via CoreEvent
/// RuntimeUpdateLog / RuntimeUpdateDone, then a refreshed snapshot matters —
/// the UI calls refreshRuntimes() on Done.
#[tauri::command]
#[specta::specta]
pub async fn update_runtime(engine: Engine<'_>, id: String) -> R<()> {
    engine
        .rpc("update_runtime", serde_json::json!({ "id": id }))
        .await
}

/// Guard (spec §5): refuse to write configs that point at a dead endpoint —
/// the server must be running and at least one endpoint key must exist.
#[tauri::command]
#[specta::specta]
pub async fn apply_runtime_config(
    engine: Engine<'_>,
    id: String,
    mapping: RuntimeMappingArg,
) -> R<RuntimeConfigStatusInfo> {
    engine
        .rpc(
            "apply_runtime_config",
            serde_json::json!({ "id": id, "mapping": mapping }),
        )
        .await
}

// ---------------------------------------------------------------------------
// Local home-config reads/writes (spec §5) — no ControlPlane access, so these
// never proxy through the engine.
// ---------------------------------------------------------------------------

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
