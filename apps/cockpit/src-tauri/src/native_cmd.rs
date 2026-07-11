//! Tauri commands exposing the native runtime's agents, slash commands, and
//! per-session todos to Cockpit — thin proxies to the engine daemon's native
//! RPC family.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

pub use ryuzi_core::api::types::{AgentInfo, CommandInfo, TodoItem};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

/// The agents available for a project (built-ins plus discovered custom agents).
#[tauri::command]
#[specta::specta]
pub async fn native_agents(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
) -> R<Vec<AgentInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "native_agents",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

/// The slash commands available for a project.
#[tauri::command]
#[specta::specta]
pub async fn native_commands(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
) -> R<Vec<CommandInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "native_commands",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

/// A session's current native todo list.
#[tauri::command]
#[specta::specta]
pub async fn session_todos(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<Vec<TodoItem>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "session_todos",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}
