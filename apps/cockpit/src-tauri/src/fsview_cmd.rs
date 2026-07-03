//! Session right-dock data: real file tree, real git diff, and project-wide
//! filename search for the ⌘K palette.

use crate::error::CmdError;
use ryuzi_core::{fsview, ControlPlane};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryInfo {
    pub name: String,
    pub dir: bool,
}

async fn session_root(cp: &ControlPlane, session_pk: &str) -> anyhow::Result<PathBuf> {
    let session = cp
        .store()
        .get_session(session_pk)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown session: {session_pk}"))?;
    if let Some(wt) = &session.worktree_path {
        if std::path::Path::new(wt).exists() {
            return Ok(PathBuf::from(wt));
        }
    }
    let project = cp
        .store()
        .get_project(&session.project_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
    Ok(PathBuf::from(project.workdir))
}

#[tauri::command]
#[specta::specta]
pub async fn list_dir(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
    rel: String,
) -> R<Vec<DirEntryInfo>> {
    let root = session_root(&cp, &session_pk).await?;
    let entries = tokio::task::spawn_blocking(move || fsview::list_dir(&root, &rel))
        .await
        .map_err(|e| CmdError { message: e.to_string() })??;
    Ok(entries
        .into_iter()
        .map(|e| DirEntryInfo {
            name: e.name,
            dir: e.dir,
        })
        .collect())
}

/// Absolute root path of the session's working tree (for opening files).
#[tauri::command]
#[specta::specta]
pub async fn session_workdir(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<String> {
    Ok(session_root(&cp, &session_pk).await?.to_string_lossy().into_owned())
}

#[tauri::command]
#[specta::specta]
pub async fn git_diff(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<String> {
    let root = session_root(&cp, &session_pk).await?;
    Ok(fsview::git_diff(&root.to_string_lossy()).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn search_files(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
    query: String,
) -> R<Vec<String>> {
    let project = cp
        .store()
        .get_project(&project_id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown project: {project_id}"),
        })?;
    let root = PathBuf::from(project.workdir);
    let hits = tokio::task::spawn_blocking(move || fsview::search_files(&root, &query, 50))
        .await
        .map_err(|e| CmdError { message: e.to_string() })?;
    Ok(hits)
}
