//! Session right-dock data: thin proxies to the engine daemon's fsview RPC
//! family — real file tree, real git diff, and project-wide filename search
//! for the ⌘K palette.

use crate::engine::EngineClient;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

pub use ryuzi_core::api::fsview_api::{DirEntryInfo, WorktreeState};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_dir(engine: Engine<'_>, session_pk: String, rel: String) -> R<Vec<DirEntryInfo>> {
    engine
        .rpc(
            "list_dir",
            serde_json::json!({ "session_pk": session_pk, "rel": rel }),
        )
        .await
}

/// Absolute root path of the session's working tree (for opening files).
#[tauri::command]
#[specta::specta]
pub async fn session_workdir(engine: Engine<'_>, session_pk: String) -> R<String> {
    engine
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
pub async fn file_exists(engine: Engine<'_>, session_pk: String, rel: String) -> R<bool> {
    engine
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
pub async fn worktree_dirty(engine: Engine<'_>, session_pk: String) -> R<WorktreeState> {
    engine
        .rpc(
            "worktree_dirty",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn git_diff(engine: Engine<'_>, session_pk: String) -> R<String> {
    engine
        .rpc("git_diff", serde_json::json!({ "session_pk": session_pk }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn search_files(engine: Engine<'_>, project_id: String, query: String) -> R<Vec<String>> {
    engine
        .rpc(
            "search_files",
            serde_json::json!({ "project_id": project_id, "query": query }),
        )
        .await
}

/// Revert one file in the session workdir to HEAD (Undo on a file-edit card).
#[tauri::command]
#[specta::specta]
pub async fn revert_file(engine: Engine<'_>, session_pk: String, path: String) -> R<()> {
    engine
        .rpc(
            "revert_file",
            serde_json::json!({ "session_pk": session_pk, "path": path }),
        )
        .await
}
