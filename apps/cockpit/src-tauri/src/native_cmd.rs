//! Tauri commands exposing the native runtime's agents, slash commands, and
//! per-session todos to Cockpit — thin proxies to the engine daemon's native
//! RPC family.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

pub use ryuzi_core::api::types::{AgentInfo, CommandInfo, QueuedMessageInfo, TodoItem};

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

/// A session's durable queued messages.
#[tauri::command]
#[specta::specta]
pub async fn session_queue(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<Vec<QueuedMessageInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "session_queue",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Queue a durable message for a session.
#[tauri::command]
#[specta::specta]
pub async fn enqueue_session_message(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    prompt: String,
    options: Option<ryuzi_core::api::types::ChatRequestOptions>,
) -> R<QueuedMessageInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "enqueue_session_message",
            serde_json::json!({
                "session_pk": session_pk,
                "prompt": prompt,
                "options": options,
            }),
        )
        .await
}

/// Remove a durable queued message from a session.
#[tauri::command]
#[specta::specta]
pub async fn remove_session_message(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    id: String,
) -> R<bool> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "remove_session_message",
            serde_json::json!({ "session_pk": session_pk, "id": id }),
        )
        .await
}
