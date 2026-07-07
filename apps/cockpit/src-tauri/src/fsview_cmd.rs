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

pub(crate) async fn session_root(cp: &ControlPlane, session_pk: &str) -> anyhow::Result<PathBuf> {
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

/// Classify the worktree at `wt`: a directory that isn't a git repo (e.g. an
/// emptied leftover dir) reports clean; otherwise uncommitted work marks it
/// dirty, and commits reachable only from the session branch are counted (no
/// branch, or a failed count, means none).
async fn worktree_state_at(wt: &str, branch: Option<&str>) -> anyhow::Result<WorktreeState> {
    if !std::path::Path::new(wt).join(".git").exists() {
        return Ok(WorktreeState {
            dirty: false,
            unmerged_commits: 0,
        });
    }
    let dirty = fsview::is_dirty(wt).await?;
    let unmerged_commits = match branch {
        Some(branch) => fsview::unmerged_commit_count(wt, branch).await.unwrap_or(0),
        None => 0,
    };
    Ok(WorktreeState {
        dirty,
        unmerged_commits,
    })
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
    let Some(wt) = session.worktree_path.as_deref() else {
        return Ok(WorktreeState {
            dirty: false,
            unmerged_commits: 0,
        });
    };
    Ok(worktree_state_at(wt, session.branch.as_deref()).await?)
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

/// Revert one file in the session workdir to HEAD (Undo on a file-edit card).
#[tauri::command]
#[specta::specta]
pub async fn revert_file(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
    path: String,
) -> R<()> {
    let root = session_root(&cp, &session_pk).await?;
    Ok(fsview::revert_file(&root.to_string_lossy(), &path).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Empty, unique scratch directory (recreated on reruns of the same pid).
    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ryuzi-fsview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Run git isolated from the developer's global/system config so commits
    /// need no signing keys or hooks.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=test", "-c", "user.email=test@test"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn non_repo_directory_reports_clean() {
        let dir = fresh_dir("nonrepo");
        let st = worktree_state_at(dir.to_str().unwrap(), Some("sess"))
            .await
            .unwrap();
        assert!(!st.dirty);
        assert_eq!(st.unmerged_commits, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn uncommitted_work_marks_dirty() {
        let dir = fresh_dir("dirty");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("scratch.txt"), "wip").unwrap();
        let st = worktree_state_at(dir.to_str().unwrap(), None)
            .await
            .unwrap();
        assert!(st.dirty);
        assert_eq!(st.unmerged_commits, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commit_only_on_session_branch_counts_as_unmerged() {
        let dir = fresh_dir("unmerged");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "base").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "base"]);
        git(&dir, &["checkout", "-q", "-b", "sess"]);
        std::fs::write(dir.join("b.txt"), "session work").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "session work"]);
        let st = worktree_state_at(dir.to_str().unwrap(), Some("sess"))
            .await
            .unwrap();
        assert!(!st.dirty);
        assert_eq!(st.unmerged_commits, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
