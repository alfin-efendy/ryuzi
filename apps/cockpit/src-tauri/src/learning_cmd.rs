//! Learning screen commands: thin proxies to the engine daemon's
//! `crates/core/src/api/learning_api.rs` RPC family — memory read/write,
//! cross-session recall, the Learning panel's journey graph, curator
//! status/rollback, and skill lifecycle/pin controls (Task 11).

use crate::engine::EngineClient;
use crate::error::CmdError;
use ryuzi_core::api::types::{CuratorStatus, LearningGraph};
use ryuzi_core::domain::SkillUsage;
use ryuzi_core::store::FtsHit;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn read_memory(engine: Engine<'_>, scope: String) -> R<Vec<String>> {
    engine
        .rpc("read_memory", serde_json::json!({ "scope": scope }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn write_memory(
    engine: Engine<'_>,
    scope: String,
    action: String,
    text: Option<String>,
    r#match: Option<String>,
) -> R<()> {
    engine
        .rpc(
            "write_memory",
            serde_json::json!({ "scope": scope, "action": action, "text": text, "match": r#match }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn search_sessions(engine: Engine<'_>, query: String) -> R<Vec<FtsHit>> {
    engine
        .rpc("search_sessions", serde_json::json!({ "query": query }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn learning_graph(engine: Engine<'_>) -> R<LearningGraph> {
    engine.rpc("learning_graph", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn curator_status(engine: Engine<'_>) -> R<CuratorStatus> {
    engine.rpc("curator_status", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn curator_rollback(engine: Engine<'_>, run_id: String) -> R<()> {
    engine
        .rpc("curator_rollback", serde_json::json!({ "run_id": run_id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_skill_usage(engine: Engine<'_>) -> R<Vec<SkillUsage>> {
    engine.rpc("list_skill_usage", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_skill_pinned(engine: Engine<'_>, name: String, pinned: bool) -> R<()> {
    engine
        .rpc(
            "set_skill_pinned",
            serde_json::json!({ "name": name, "pinned": pinned }),
        )
        .await
}
