//! Session export / import: thin proxies to the engine daemon's session_io
//! RPC family — serializes a session's transcript and provider-turn ledger
//! to a portable JSON document (or a shareable HTML page), and re-imports it
//! as a new (archived) session for viewing.

use crate::engine::EngineClient;
use crate::error::CmdError;
use ryuzi_core::Session;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

/// Export a session as a pretty JSON string.
#[tauri::command]
#[specta::specta]
pub async fn export_session(engine: Engine<'_>, session_pk: String) -> R<String> {
    engine
        .rpc(
            "export_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Render a session as a self-contained, shareable HTML document.
#[tauri::command]
#[specta::specta]
pub async fn share_session(engine: Engine<'_>, session_pk: String) -> R<String> {
    engine
        .rpc(
            "share_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Import a previously exported session JSON as a new archived session.
#[tauri::command]
#[specta::specta]
pub async fn import_session(engine: Engine<'_>, project_id: String, data: String) -> R<Session> {
    engine
        .rpc(
            "import_session",
            serde_json::json!({ "project_id": project_id, "data": data }),
        )
        .await
}
