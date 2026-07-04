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
        .map_err(|e| CmdError {
            message: e.to_string(),
        })??;
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
    Ok(session_root(&cp, &session_pk)
        .await?
        .to_string_lossy()
        .into_owned())
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeState {
    /// Uncommitted work (staged, unstaged, or untracked).
    pub dirty: bool,
    /// Commits reachable only from the session branch — deleting the branch
    /// would strand them.
    pub unmerged_commits: u32,
}

/// What the session's OWN worktree would lose on teardown — the archive flow
/// asks before discarding either kind of work. Sessions whose worktree is
/// gone (or isn't a repo, e.g. an emptied leftover dir) report clean —
/// deliberately NOT the project-workdir fallback: the main checkout's state
/// is the user's business, not the session's.
#[tauri::command]
#[specta::specta]
pub async fn worktree_dirty(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
) -> R<WorktreeState> {
    let session = cp
        .store()
        .get_session(&session_pk)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown session: {session_pk}"),
        })?;
    let clean = WorktreeState {
        dirty: false,
        unmerged_commits: 0,
    };
    let Some(wt) = session.worktree_path.as_deref() else {
        return Ok(clean);
    };
    if !std::path::Path::new(wt).join(".git").exists() {
        return Ok(clean);
    }
    let dirty = fsview::is_dirty(wt).await?;
    let unmerged_commits = match session.branch.as_deref() {
        Some(branch) => fsview::unmerged_commit_count(wt, branch).await.unwrap_or(0),
        None => 0,
    };
    Ok(WorktreeState {
        dirty,
        unmerged_commits,
    })
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
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    Ok(hits)
}
