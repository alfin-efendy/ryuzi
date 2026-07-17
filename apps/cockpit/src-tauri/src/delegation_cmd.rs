//! Cockpit child-run commands: runner-aware thin proxies to the engine RPC API.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::{
    domain::{AgentRun, AgentRunRosterInfo},
    Message,
};
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn get_child_runs(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<AgentRunRosterInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc(
            "get_child_runs",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_child_transcript(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    run_id: String,
) -> R<Vec<Message>> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc(
            "get_child_transcript",
            serde_json::json!({ "session_pk": session_pk, "run_id": run_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_child_run(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    run_id: String,
) -> R<()> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc(
            "cancel_child_run",
            serde_json::json!({ "session_pk": session_pk, "run_id": run_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn retry_child_run(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    run_id: String,
) -> R<AgentRun> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc(
            "retry_child_run",
            serde_json::json!({ "session_pk": session_pk, "run_id": run_id }),
        )
        .await
}
