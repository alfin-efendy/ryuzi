//! Session right-dock data: real file tree, real git diff, and project-wide
//! filename search for the ⌘K palette. Moved verbatim (per the Move Recipe)
//! from `apps/cockpit/src-tauri/src/fsview_cmd.rs`; that file keeps its own
//! copy until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::control::ControlPlane;
use crate::fsview;
use crate::serve::ApiState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::path::PathBuf;

pub(crate) const HANDLES: &[&str] = &[
    "list_dir",
    "file_exists",
    "session_workdir",
    "worktree_dirty",
    "git_diff",
    "search_files",
    "revert_file",
];

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
    if session.project_id.is_none() {
        let dir = crate::paths::chat_scratch_dir(session_pk);
        std::fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    let project_id = session
        .project_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("session {session_pk} has no bound project"))?;
    let project = cp
        .store()
        .get_project(project_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
    Ok(PathBuf::from(project.workdir))
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

#[derive(Deserialize)]
struct ListDirP {
    session_pk: String,
    rel: String,
}
#[derive(Deserialize)]
struct SessionPkP {
    session_pk: String,
}
#[derive(Deserialize)]
struct SearchFilesP {
    project_id: String,
    query: String,
}
#[derive(Deserialize)]
struct RevertFileP {
    session_pk: String,
    path: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_dir" => {
            let a: ListDirP = params(p)?;
            ok(list_dir(cp, &a.session_pk, a.rel).await?)
        }
        "file_exists" => {
            let a: ListDirP = params(p)?;
            ok(file_exists(cp, &a.session_pk, &a.rel).await?)
        }
        "session_workdir" => {
            let a: SessionPkP = params(p)?;
            ok(session_root(cp, &a.session_pk)
                .await?
                .to_string_lossy()
                .into_owned())
        }
        "worktree_dirty" => {
            let a: SessionPkP = params(p)?;
            ok(worktree_dirty(cp, &a.session_pk).await?)
        }
        "git_diff" => {
            let a: SessionPkP = params(p)?;
            let root = session_root(cp, &a.session_pk).await?;
            ok(fsview::git_diff(&root.to_string_lossy()).await?)
        }
        "search_files" => {
            let a: SearchFilesP = params(p)?;
            ok(search_files(cp, &a.project_id, &a.query).await?)
        }
        "revert_file" => {
            let a: RevertFileP = params(p)?;
            let root = session_root(cp, &a.session_pk).await?;
            ok(fsview::revert_file(&root.to_string_lossy(), &a.path).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn list_dir(
    cp: &ControlPlane,
    session_pk: &str,
    rel: String,
) -> Result<Vec<DirEntryInfo>, ApiError> {
    let root = session_root(cp, session_pk).await?;
    let entries = tokio::task::spawn_blocking(move || fsview::list_dir(&root, &rel))
        .await
        .map_err(|e| ApiError {
            status: 500,
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

/// Does `rel` resolve to an existing regular file inside the session's jailed
/// worktree root? Escapes, absolute paths, and non-files fail silently to
/// `false` (used by the chat file-link preview, which must never error).
async fn file_exists(cp: &ControlPlane, session_pk: &str, rel: &str) -> Result<bool, ApiError> {
    let root = session_root(cp, session_pk).await?;
    let Ok(path) = fsview::jail(&root, rel) else {
        return Ok(false);
    };
    Ok(tokio::fs::metadata(&path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false))
}

/// What the session's OWN worktree would lose on teardown — the archive flow
/// asks before discarding either kind of work. Sessions whose worktree is
/// gone (or isn't a repo, e.g. an emptied leftover dir) report clean —
/// deliberately NOT the project-workdir fallback: the main checkout's state
/// is the user's business, not the session's.
async fn worktree_dirty(cp: &ControlPlane, session_pk: &str) -> Result<WorktreeState, ApiError> {
    let session = cp
        .store()
        .get_session(session_pk)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown session: {session_pk}")))?;
    let Some(wt) = session.worktree_path.as_deref() else {
        return Ok(WorktreeState {
            dirty: false,
            unmerged_commits: 0,
        });
    };
    Ok(worktree_state_at(wt, session.branch.as_deref()).await?)
}

async fn search_files(
    cp: &ControlPlane,
    project_id: &str,
    query: &str,
) -> Result<Vec<String>, ApiError> {
    let project = cp
        .store()
        .get_project(project_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown project: {project_id}")))?;
    let root = PathBuf::from(project.workdir);
    let query = query.to_string();
    let hits = tokio::task::spawn_blocking(move || fsview::search_files(&root, &query, 50))
        .await
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;
    use serial_test::serial;
    use std::path::Path;
    use std::process::Command;

    /// Redirect dirs::data_dir() into a tempdir for the duration of a test so
    /// scratch-dir creation never touches the real ~/.local/share.
    /// Process-global env — every test using it must be #[serial].
    struct StateDirGuard {
        _dir: tempfile::TempDir,
    }

    impl StateDirGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("HOME", dir.path());
            StateDirGuard { _dir: dir }
        }
    }

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
    #[serial]
    async fn session_root_resolves_chat_scratch_dir() {
        let _guard = StateDirGuard::new();
        let s = crate::api::tests_support::state().await;
        let now = crate::paths::now_ms();
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "chat-x".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: crate::domain::SessionStatus::Idle,
                started_by: None,
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: crate::domain::PermMode::Default,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let root = super::session_root(&s.cp, "chat-x").await.unwrap();
        assert_eq!(root, crate::paths::chat_scratch_dir("chat-x"));
        assert!(root.exists(), "scratch dir should be created on resolve");
    }

    #[tokio::test]
    async fn session_workdir_errors_cleanly_on_unknown_session() {
        let s = state().await;
        let err = dispatch(&s, "session_workdir", json!({"session_pk": "nope"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("nope"), "got: {}", err.message);
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
