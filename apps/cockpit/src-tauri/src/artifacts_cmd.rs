//! Session artifact commands: thin proxies to the engine artifact RPC family.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

pub use ryuzi_core::api::types::{ArtifactFileInfo, ArtifactInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_session_artifacts(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<Vec<ArtifactInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_session_artifacts",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn fetch_artifact(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    artifact_id: String,
) -> R<ArtifactFileInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "fetch_artifact",
            serde_json::json!({ "session_pk": session_pk, "artifact_id": artifact_id }),
        )
        .await
}
