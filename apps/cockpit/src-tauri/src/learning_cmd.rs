//! Learning screen commands: thin proxies to the engine daemon's
//! `crates/core/src/api/learning_api.rs` RPC family for cross-session recall
//! and skill lifecycle/pin controls.
//!
//! Per-agent Learning commands (`get_agent_learning`, concept CRUD, raw
//! repair, rollback) live in `agent_cmd.rs`; like everything here they are
//! local-engine-only.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::domain::SkillUsage;
use ryuzi_core::store::FtsHit;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn search_sessions(engine: Engine<'_>, query: String) -> R<Vec<FtsHit>> {
    let client = engine.client("local")?;
    client
        .rpc("search_sessions", serde_json::json!({ "query": query }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_skill_usage(engine: Engine<'_>) -> R<Vec<SkillUsage>> {
    let client = engine.client("local")?;
    client.rpc("list_skill_usage", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_skill_pinned(engine: Engine<'_>, name: String, pinned: bool) -> R<()> {
    let client = engine.client("local")?;
    client
        .rpc(
            "set_skill_pinned",
            serde_json::json!({ "name": name, "pinned": pinned }),
        )
        .await
}
