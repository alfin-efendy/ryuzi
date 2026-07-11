//! Scheduler screen commands: thin proxies to the engine daemon's scheduler
//! RPC family. Jobs persist in SQLite; the core runner loop fires them for
//! real (starting agent sessions); run history closes off the session's
//! Result/Error events. `parse_natural_schedule` stays local — a pure wrapper
//! around `scheduler::natural_to_cron` that needs no engine round-trip.

use crate::engine::EngineClient;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `RunInfo` is only reachable transitively (as a field of `JobInfo::history`)
// but is re-exported by name anyway to keep the moved-DTO surface complete
// and self-documenting; specta still emits it via the type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{JobInfo, JobInput, RunInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_jobs(engine: Engine<'_>) -> R<Vec<JobInfo>> {
    engine.rpc("list_jobs", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn create_job(engine: Engine<'_>, input: JobInput) -> R<Vec<JobInfo>> {
    engine
        .rpc("create_job", serde_json::json!({ "input": input }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_job(engine: Engine<'_>, id: String, input: JobInput) -> R<Vec<JobInfo>> {
    engine
        .rpc(
            "update_job",
            serde_json::json!({ "id": id, "input": input }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_job(engine: Engine<'_>, id: String, enabled: bool) -> R<Vec<JobInfo>> {
    engine
        .rpc(
            "toggle_job",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_job(engine: Engine<'_>, id: String) -> R<Vec<JobInfo>> {
    engine
        .rpc("delete_job", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn run_job_now(engine: Engine<'_>, id: String) -> R<Vec<JobInfo>> {
    engine
        .rpc("run_job_now", serde_json::json!({ "id": id }))
        .await
}

/// Preview helper for the natural-language schedule editor.
#[tauri::command]
#[specta::specta]
pub fn parse_natural_schedule(text: String) -> Option<String> {
    ryuzi_core::scheduler::natural_to_cron(&text)
}
