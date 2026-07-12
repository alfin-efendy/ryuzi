//! App-control audit feed command: a thin proxy to the engine daemon's
//! `crates/core/src/api/audit.rs` RPC family — the read-only Settings →
//! Audit feed surfacing Task 7's per-mutation audit rows.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::domain::AuditRow;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_audit(engine: Engine<'_>, limit: u32) -> R<Vec<AuditRow>> {
    let client = engine.client("local")?;
    client
        .rpc("list_audit", serde_json::json!({ "limit": limit }))
        .await
}
