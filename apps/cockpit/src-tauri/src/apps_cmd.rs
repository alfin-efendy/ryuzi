//! Apps screen commands: thin proxies to the engine daemon's apps RPC
//! family. MCP server definitions persist in SQLite; `probe_app` does a real
//! stdio handshake (initialize → tools/list) or an HTTP reachability check;
//! enabled servers attach to agent sessions for real via
//! `SessionCtx.mcp_servers`.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `AgentAccessInfo`/`ToolInfo` are only reachable transitively (as fields of
// `AppInfo`) but are re-exported by name anyway for a complete, documented
// DTO surface; specta still emits them via the type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{AddAppInput, AgentAccessInfo, AppInfo, ToolInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_apps(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_apps", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn add_app(
    engine: Engine<'_>,
    runner_id: Option<String>,
    input: AddAppInput,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("add_app", serde_json::json!({ "input": input }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_app(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("remove_app", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn probe_app(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("probe_app", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_app_scope(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    scope: String,
    scope_gateways: Vec<String>,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_app_scope",
            serde_json::json!({ "id": id, "scope": scope, "scope_gateways": scope_gateways }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_app_tool_perm(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    tool: String,
    perm: String,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_app_tool_perm",
            serde_json::json!({ "id": id, "tool": tool, "perm": perm }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_app_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    agent_id: String,
    allowed: bool,
) -> R<Vec<AppInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "toggle_app_agent",
            serde_json::json!({ "id": id, "agent_id": agent_id, "allowed": allowed }),
        )
        .await
}
