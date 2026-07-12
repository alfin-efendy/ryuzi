//! Scheduler screen commands: thin proxies to the engine daemon's scheduler
//! RPC family. Jobs persist in SQLite; the core runner loop fires them for
//! real (starting agent sessions); run history closes off the session's
//! Result/Error events. `parse_natural_schedule` stays local — a pure wrapper
//! around `scheduler::natural_to_cron` that needs no engine round-trip.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `RunInfo` is only reachable transitively (as a field of `JobInfo::history`)
// but is re-exported by name anyway to keep the moved-DTO surface complete
// and self-documenting; specta still emits it via the type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{JobInfo, JobInput, RunInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_jobs(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_jobs", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn create_job(
    engine: Engine<'_>,
    runner_id: Option<String>,
    input: JobInput,
) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("create_job", serde_json::json!({ "input": input }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_job(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    input: JobInput,
) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_job",
            serde_json::json!({ "id": id, "input": input }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_job(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    enabled: bool,
) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "toggle_job",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_job(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("delete_job", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn run_job_now(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<JobInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("run_job_now", serde_json::json!({ "id": id }))
        .await
}

/// Preview helper for the natural-language schedule editor.
#[tauri::command]
#[specta::specta]
pub fn parse_natural_schedule(text: String) -> Option<String> {
    ryuzi_core::scheduler::natural_to_cron(&text)
}
