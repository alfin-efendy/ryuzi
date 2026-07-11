//! Session right-dock data: thin proxies to the engine daemon's fsview RPC
//! family — real file tree, real git diff, and project-wide filename search
//! for the ⌘K palette.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

pub use ryuzi_core::api::fsview_api::{DirEntryInfo, WorktreeState};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_dir(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    rel: String,
) -> R<Vec<DirEntryInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_dir",
            serde_json::json!({ "session_pk": session_pk, "rel": rel }),
        )
        .await
}

/// Absolute root path of the session's working tree (for opening files).
#[tauri::command]
#[specta::specta]
pub async fn session_workdir(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<String> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "session_workdir",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Whether `rel` names an existing regular file inside the session's
/// working tree. Jailed like every fsview path: absolute paths and `..`
/// escapes are simply "not found" (false), never an error — chat file-links
/// must fail silent.
#[tauri::command]
#[specta::specta]
pub async fn file_exists(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    rel: String,
) -> R<bool> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "file_exists",
            serde_json::json!({ "session_pk": session_pk, "rel": rel }),
        )
        .await
}

/// What the session's OWN worktree would lose on teardown — the archive flow
/// asks before discarding either kind of work. Sessions whose worktree is
/// gone (or isn't a repo, e.g. an emptied leftover dir) report clean —
/// deliberately NOT the project-workdir fallback: the main checkout's state
/// is the user's business, not the session's.
#[tauri::command]
#[specta::specta]
pub async fn worktree_dirty(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<WorktreeState> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "worktree_dirty",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn git_diff(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<String> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("git_diff", serde_json::json!({ "session_pk": session_pk }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn search_files(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    query: String,
) -> R<Vec<String>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "search_files",
            serde_json::json!({ "project_id": project_id, "query": query }),
        )
        .await
}

/// Revert one file in the session workdir to HEAD (Undo on a file-edit card).
#[tauri::command]
#[specta::specta]
pub async fn revert_file(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    path: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "revert_file",
            serde_json::json!({ "session_pk": session_pk, "path": path }),
        )
        .await
}
