//! Skills screen commands: thin proxies to the engine daemon's
//! `crates/core/src/api/skills_api.rs` RPC family (list/install/remove/refresh
//! git-backed native skills and plugin-bundled skill packs). DTOs stay
//! imported from `ryuzi_core::skills_install` — that's their real home, it
//! was never moved to `ryuzi_core::api::types`.

use crate::engine::EngineClient;
use ryuzi_core::skills_install::{InstalledSkillInfo, InstalledSkillPack};
use std::sync::Arc;
use tauri::State;

type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_skills(engine: Engine<'_>) -> Result<Vec<InstalledSkillInfo>, String> {
    engine
        .rpc("list_skills", serde_json::json!({}))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill(
    engine: Engine<'_>,
    source: String,
) -> Result<InstalledSkillPack, String> {
    engine
        .rpc("install_skill", serde_json::json!({ "source": source }))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_skill(engine: Engine<'_>, id: String) -> Result<(), String> {
    engine
        .rpc("remove_skill", serde_json::json!({ "id": id }))
        .await
        .map_err(|e| e.message)
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_skill(engine: Engine<'_>, id: String) -> Result<InstalledSkillPack, String> {
    engine
        .rpc("refresh_skill", serde_json::json!({ "id": id }))
        .await
        .map_err(|e| e.message)
}
