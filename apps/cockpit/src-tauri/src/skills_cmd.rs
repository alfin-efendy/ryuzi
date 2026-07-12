//! Skills screen commands: thin proxies to the engine daemon's
//! `crates/core/src/api/skills_api.rs` RPC family (list/install/remove/refresh
//! git-backed native skills and plugin-bundled skill packs). DTOs stay
//! imported from `ryuzi_core::skills_install` — that's their real home, it
//! was never moved to `ryuzi_core::api::types`.

use crate::engine_manager::EngineManager;
use ryuzi_core::skills_install::{InstalledSkillInfo, InstalledSkillPack};
use std::sync::Arc;
use tauri::State;

type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_skills(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> Result<Vec<InstalledSkillInfo>, String> {
    let client = engine
        .client(runner_id.as_deref().unwrap_or("local"))
        .map_err(|e| e.message)?;
    client
        .rpc("list_skills", serde_json::json!({}))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill(
    engine: Engine<'_>,
    runner_id: Option<String>,
    source: String,
) -> Result<InstalledSkillPack, String> {
    let client = engine
        .client(runner_id.as_deref().unwrap_or("local"))
        .map_err(|e| e.message)?;
    client
        .rpc("install_skill", serde_json::json!({ "source": source }))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_skill(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> Result<(), String> {
    let client = engine
        .client(runner_id.as_deref().unwrap_or("local"))
        .map_err(|e| e.message)?;
    client
        .rpc("remove_skill", serde_json::json!({ "id": id }))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_skill(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> Result<InstalledSkillPack, String> {
    let client = engine
        .client(runner_id.as_deref().unwrap_or("local"))
        .map_err(|e| e.message)?;
    client
        .rpc("refresh_skill", serde_json::json!({ "id": id }))
        .await
        .map_err(|e| e.message)
}
