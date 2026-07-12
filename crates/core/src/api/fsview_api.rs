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
use std::path::{Path, PathBuf};

pub(crate) const HANDLES: &[&str] = &[
    "list_dir",
    "file_exists",
    "session_workdir",
    "worktree_dirty",
    "git_diff",
    "search_files",
    "revert_file",
    "read_file",
    "read_file_base64",
];

/// Largest file the viewer will load into memory as text.
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024; // 2 MB cap

/// Largest media file inlined as a base64 preview — shared by the
/// session-workdir file viewer (this module) and the composer's
/// client-local media previews (`commands::read_local_media`).
pub const MAX_MEDIA_READ_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    pub data_base64: String,
    pub content_type: Option<String>,
}

/// Best-effort content type from a file extension — `None` (and thus no data
/// URL mime) for anything unrecognized, including svg (callers force
/// `image/svg+xml` themselves since it's always trustworthy text).
pub fn content_type_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "rs" | "ts" | "tsx" | "js" | "jsx" | "json" | "toml" | "yaml" | "yml" => {
            Some("text/plain".to_string())
        }
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "zip" => Some("application/zip".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "webm" => Some("video/webm".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "mkv" => Some("video/x-matroska".to_string()),
        "mp3" => Some("audio/mpeg".to_string()),
        "wav" => Some("audio/wav".to_string()),
        "ogg" => Some("audio/ogg".to_string()),
        "m4a" => Some("audio/mp4".to_string()),
        "flac" => Some("audio/flac".to_string()),
        _ => None,
    }
}

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
        "read_file" => {
            let a: ListDirP = params(p)?;
            ok(read_file(cp, &a.session_pk, &a.rel).await?)
        }
        "read_file_base64" => {
            let a: ListDirP = params(p)?;
            ok(read_file_base64(cp, &a.session_pk, &a.rel).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Resolve `rel` inside `root`'s jail and enforce a byte-size cap before any
/// content is read — escapes/absolute paths and oversize files both fail as
/// a 400, and neither ever gets its bytes touched.
///
/// `fsview::jail` itself is lexical-only (it rejects absolute paths and `..`
/// components but doesn't touch the filesystem), which is correct for its
/// other callers (`list_dir`/`search_files`/`git_diff` may legitimately
/// target non-existent or not-yet-existing paths). A file READ needs the
/// stronger guarantee: canonicalize both `root` and the joined path and
/// re-check with `starts_with`, so a symlink planted inside the session root
/// that points outside it is caught too — mirroring `serve.rs`'s
/// `get_attachment` jail (the other read surface in this crate) rather than
/// changing `fsview::jail`'s shared lexical behavior. A canonicalize failure
/// (missing file, dangling symlink, permission error) folds into the same
/// not_found as a metadata miss, so a jail escape and a missing file are
/// indistinguishable to the caller.
async fn jailed_readable(root: &Path, rel: &str, cap: u64) -> Result<PathBuf, ApiError> {
    let path = fsview::jail(root, rel).map_err(|e| ApiError::bad_request(e.to_string()))?;

    let root_canon = tokio::fs::canonicalize(root)
        .await
        .map_err(|e| ApiError::not_found(format!("cannot read {rel}: {e}")))?;
    let target_canon = tokio::fs::canonicalize(&path)
        .await
        .map_err(|e| ApiError::not_found(format!("cannot read {rel}: {e}")))?;
    if !target_canon.starts_with(&root_canon) {
        return Err(ApiError::not_found(format!(
            "cannot read {rel}: {rel} escapes the workspace"
        )));
    }

    let meta = tokio::fs::metadata(&target_canon)
        .await
        .map_err(|e| ApiError::not_found(format!("cannot read {rel}: {e}")))?;
    if meta.len() > cap {
        return Err(ApiError::bad_request(format!(
            "file too large ({} bytes)",
            meta.len()
        )));
    }
    Ok(target_canon)
}

/// Session-workdir text read for the file viewer — jailed to the session's
/// root and capped at [`MAX_READ_BYTES`]. Remote-safe: never touches the
/// caller's local disk, only the runner's.
async fn read_file(cp: &ControlPlane, session_pk: &str, rel: &str) -> Result<String, ApiError> {
    let root = session_root(cp, session_pk).await?;
    let path = jailed_readable(&root, rel, MAX_READ_BYTES).await?;
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ApiError::not_found(format!("cannot read {rel}: {e}")))
}

/// Session-workdir binary read for the file viewer's image/svg preview —
/// jailed and capped at [`MAX_MEDIA_READ_BYTES`], base64-encoded for the
/// data-URL the viewer renders.
async fn read_file_base64(
    cp: &ControlPlane,
    session_pk: &str,
    rel: &str,
) -> Result<MediaFile, ApiError> {
    use base64::Engine as _;
    let root = session_root(cp, session_pk).await?;
    let path = jailed_readable(&root, rel, MAX_MEDIA_READ_BYTES).await?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| ApiError::not_found(format!("cannot read {rel}: {e}")))?;
    Ok(MediaFile {
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        content_type: content_type_for_path(&path),
    })
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

    /// A session whose worktree is `worktree`, bypassing the project lookup
    /// entirely — `session_root` returns it directly since the dir exists.
    async fn insert_worktree_session(cp: &ControlPlane, session_pk: &str, worktree: &str) {
        let now = crate::paths::now_ms();
        cp.store()
            .insert_session(crate::domain::Session {
                session_pk: session_pk.into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: Some(worktree.into()),
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
    }

    #[tokio::test]
    async fn read_file_round_trips_a_normal_file() {
        let s = state().await;
        let dir = fresh_dir("read-file-ok");
        std::fs::write(dir.join("hello.txt"), "hi there").unwrap();
        insert_worktree_session(&s.cp, "sess-read-ok", dir.to_str().unwrap()).await;
        let out = dispatch(
            &s,
            "read_file",
            json!({"session_pk": "sess-read-ok", "rel": "hello.txt"}),
        )
        .await
        .unwrap();
        assert_eq!(out, json!("hi there"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_file_rejects_parent_traversal() {
        let s = state().await;
        let dir = fresh_dir("read-file-traversal");
        insert_worktree_session(&s.cp, "sess-read-trav", dir.to_str().unwrap()).await;
        let err = dispatch(
            &s,
            "read_file",
            json!({"session_pk": "sess-read-trav", "rel": "../etc/passwd"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "got: {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_file_rejects_an_absolute_path() {
        let s = state().await;
        let dir = fresh_dir("read-file-absolute");
        insert_worktree_session(&s.cp, "sess-read-abs", dir.to_str().unwrap()).await;
        let abs = if cfg!(windows) {
            "C:\\Windows\\win.ini"
        } else {
            "/etc/passwd"
        };
        let err = dispatch(
            &s,
            "read_file",
            json!({"session_pk": "sess-read-abs", "rel": abs}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "got: {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn jailed_readable_rejects_a_file_over_the_cap() {
        let dir = fresh_dir("read-file-cap");
        std::fs::write(dir.join("big.txt"), "0123456789").unwrap(); // 10 bytes
        let err = jailed_readable(&dir, "big.txt", 5).await.unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("too large"), "got: {}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Symlink creation on Windows normally requires elevated privileges (or
    // Developer Mode), unlike Unix — so this proves the canonicalize +
    // starts_with jail escape check on the platform where it's cheap to set
    // up. The check itself (`jailed_readable`'s canonicalize/starts_with
    // logic) compiles and runs on every platform; only this symlink fixture
    // is unix-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn jailed_readable_rejects_a_symlink_escaping_the_root() {
        let outside = fresh_dir("read-file-symlink-outside");
        std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();

        let root = fresh_dir("read-file-symlink-root");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();

        let err = jailed_readable(&root, "link.txt", MAX_READ_BYTES)
            .await
            .unwrap_err();
        assert_eq!(err.status, 404, "got: {err:?}");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// `read_file` end-to-end through `dispatch`, proving the jail escape is
    /// caught at the RPC layer too, not just in the `jailed_readable` helper.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_file_rejects_a_symlink_escaping_the_session_root() {
        let s = state().await;
        let outside = fresh_dir("read-file-rpc-symlink-outside");
        std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();

        let dir = fresh_dir("read-file-rpc-symlink-root");
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();
        insert_worktree_session(&s.cp, "sess-read-symlink", dir.to_str().unwrap()).await;

        let err = dispatch(
            &s,
            "read_file",
            json!({"session_pk": "sess-read-symlink", "rel": "link.txt"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 404, "got: {err:?}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn read_file_base64_round_trips_and_resolves_content_type() {
        use base64::Engine as _;
        let s = state().await;
        let dir = fresh_dir("read-file-b64");
        std::fs::write(dir.join("shot.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();
        insert_worktree_session(&s.cp, "sess-read-b64", dir.to_str().unwrap()).await;
        let out = dispatch(
            &s,
            "read_file_base64",
            json!({"session_pk": "sess-read-b64", "rel": "shot.png"}),
        )
        .await
        .unwrap();
        let media: MediaFile = serde_json::from_value(out).unwrap();
        assert_eq!(media.content_type.as_deref(), Some("image/png"));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(media.data_base64)
                .unwrap(),
            vec![0x89, 0x50, 0x4e, 0x47]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn media_content_types_cover_video_and_audio() {
        let ct = |p: &str| content_type_for_path(Path::new(p));
        assert_eq!(ct("a.webp").as_deref(), Some("image/webp"));
        assert_eq!(ct("a.mp4").as_deref(), Some("video/mp4"));
        assert_eq!(ct("a.mp3").as_deref(), Some("audio/mpeg"));
        assert_eq!(ct("a.wav").as_deref(), Some("audio/wav"));
    }
}
