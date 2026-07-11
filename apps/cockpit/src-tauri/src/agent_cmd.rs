//! Settings → Agent commands: thin proxies to the engine daemon's agent RPC
//! family — the native agent's default model and permission mode (settings KV
//! via `ryuzi_core::agent_settings`) plus the selectable-model list the
//! composer and Settings share.

use crate::engine::EngineClient;
use crate::error::CmdError;
use ryuzi_core::llm_router::model_effort::SelectableModelInfo;
use std::sync::Arc;
use tauri::State;

// Re-exported by name for a complete, documented DTO surface; specta still
// emits it via the command type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::AgentSettingsInfo;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn get_agent_settings(engine: Engine<'_>) -> R<AgentSettingsInfo> {
    engine
        .rpc("get_agent_settings", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_agent_settings(
    engine: Engine<'_>,
    model: Option<String>,
    perm_mode: Option<String>,
) -> R<()> {
    engine
        .rpc(
            "set_agent_settings",
            serde_json::json!({ "model": model, "perm_mode": perm_mode }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_selectable_models(engine: Engine<'_>) -> R<Vec<SelectableModelInfo>> {
    engine
        .rpc("list_selectable_models", serde_json::json!({}))
        .await
}
