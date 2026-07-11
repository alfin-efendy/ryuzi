//! Session export / import: thin proxies to the engine daemon's session_io
//! RPC family — serializes a session's transcript and provider-turn ledger
//! to a portable JSON document (or a shareable HTML page), and re-imports it
//! as a new (archived) session for viewing.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::Session;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

/// Export a session as a pretty JSON string.
#[tauri::command]
#[specta::specta]
pub async fn export_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<String> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "export_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Render a session as a self-contained, shareable HTML document.
#[tauri::command]
#[specta::specta]
pub async fn share_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<String> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "share_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Import a previously exported session JSON as a new archived session.
#[tauri::command]
#[specta::specta]
pub async fn import_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    data: String,
) -> R<Session> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "import_session",
            serde_json::json!({ "project_id": project_id, "data": data }),
        )
        .await
}
