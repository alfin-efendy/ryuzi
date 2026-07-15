//! Automation Hook commands: local-only thin proxies to the engine daemon's
//! automation RPC family. Hooks execute against local runtime state and never
//! target paired remote runners.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::api::types::{AutomationHookDetail, AutomationHookInfo, AutomationHookInput};
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

fn local_client(
    engine: &Engine<'_>,
    runner_id: Option<&str>,
) -> R<Arc<crate::engine::EngineClient>> {
    if let Some(runner_id) = runner_id.filter(|id| *id != "local") {
        return Err(CmdError {
            message: format!(
                "automation hooks are only available on the local runner, not {runner_id}"
            ),
        });
    }
    engine.client("local")
}

#[tauri::command]
#[specta::specta]
pub async fn list_automation_hooks(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<AutomationHookInfo>> {
    local_client(&engine, runner_id.as_deref())?
        .rpc("list_automation_hooks", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn automation_hook_detail(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<AutomationHookDetail> {
    local_client(&engine, runner_id.as_deref())?
        .rpc("automation_hook_detail", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn create_automation_hook(
    engine: Engine<'_>,
    runner_id: Option<String>,
    input: AutomationHookInput,
) -> R<AutomationHookInfo> {
    local_client(&engine, runner_id.as_deref())?
        .rpc(
            "create_automation_hook",
            serde_json::json!({ "input": input }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_automation_hook(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    input: AutomationHookInput,
) -> R<AutomationHookInfo> {
    local_client(&engine, runner_id.as_deref())?
        .rpc(
            "update_automation_hook",
            serde_json::json!({ "id": id, "input": input }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_automation_hook(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    enabled: bool,
) -> R<AutomationHookInfo> {
    local_client(&engine, runner_id.as_deref())?
        .rpc(
            "toggle_automation_hook",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_automation_hook(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<AutomationHookInfo>> {
    local_client(&engine, runner_id.as_deref())?
        .rpc("delete_automation_hook", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn test_automation_hook(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<AutomationHookDetail> {
    local_client(&engine, runner_id.as_deref())?
        .rpc("test_automation_hook", serde_json::json!({ "id": id }))
        .await
}
