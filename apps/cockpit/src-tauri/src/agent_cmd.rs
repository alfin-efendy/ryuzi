//! Settings → Agent commands: thin proxies to the engine daemon's agent RPC
//! family — the native agent's default model and permission mode (settings KV
//! via `ryuzi_core::agent_settings`) plus the selectable-model list the
//! composer and Settings share.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::llm_router::model_effort::SelectableModelInfo;
use std::sync::Arc;
use tauri::State;

// Re-exported by name for a complete, documented DTO surface; specta still
// emits it via the command type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::AgentSettingsInfo;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn get_agent_settings(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<AgentSettingsInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("get_agent_settings", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_agent_settings(
    engine: Engine<'_>,
    runner_id: Option<String>,
    model: Option<String>,
    perm_mode: Option<String>,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_agent_settings",
            serde_json::json!({ "model": model, "perm_mode": perm_mode }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_selectable_models(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<SelectableModelInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_selectable_models", serde_json::json!({}))
        .await
}
