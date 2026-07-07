//! Lightweight worktree snapshots for undo, mirroring opencode's snapshot
//! feature. Before a mutating tool runs, the runner records a snapshot (a git
//! commit SHA capturing tracked state via `git stash create`, or HEAD when the
//! tree is clean). The `revert` tool restores tracked files to the most recent
//! snapshot.
//!
//! Limitation: this reverts modifications to tracked files; it does not delete
//! files that were newly created after the snapshot.

use std::path::Path;
use std::process::Stdio;

async fn git(work_dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed", args);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Capture the current worktree state, returning a git SHA to restore later.
/// `None` if `work_dir` is not a git repo.
pub async fn take(work_dir: &Path) -> Option<String> {
    // `git stash create` captures tracked modifications without touching the
    // worktree, returning a commit SHA (empty when the tree is clean).
    let sha = git(work_dir, &["stash", "create"]).await.ok()?;
    if sha.is_empty() {
        git(work_dir, &["rev-parse", "HEAD"]).await.ok()
    } else {
        Some(sha)
    }
}

/// Restore tracked files to the snapshot `sha`.
pub async fn restore(work_dir: &Path, sha: &str) -> anyhow::Result<()> {
    git(work_dir, &["checkout", sha, "--", "."]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(dir: &Path, args: &[&str]) {
        assert!(tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .unwrap()
            .status
            .success());
    }

    #[tokio::test]
    async fn snapshot_then_restore_reverts_a_modification() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        run(p, &["init", "-q"]).await;
        run(p, &["config", "user.email", "t@t"]).await;
        run(p, &["config", "user.name", "t"]).await;
        // Pin EOL handling: a global core.autocrlf=true (the Git for
        // Windows default) would check "one\n" back out as "one\r\n" and
        // break the byte-for-byte assertions below.
        run(p, &["config", "core.autocrlf", "false"]).await;
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        run(p, &["add", "."]).await;
        run(p, &["commit", "-qm", "init"]).await;

        // Snapshot the clean tree, then modify the file.
        let snap = take(p).await.unwrap();
        std::fs::write(p.join("a.txt"), "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "two\n");

        // Restore reverts the modification.
        restore(p, &snap).await.unwrap();
        assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "one\n");
    }

    #[tokio::test]
    async fn take_on_non_repo_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take(dir.path()).await.is_none());
    }
}
