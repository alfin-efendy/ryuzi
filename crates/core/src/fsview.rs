//! Read-only project/worktree views for the session right dock: a jailed
//! directory listing, the real git diff, and filename search for the palette.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
}

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".sidecar-build", "dist", ".next"];

/// Resolve `rel` under `root`, rejecting absolute paths and `..` escapes.
pub fn jail(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("absolute paths are not allowed");
    }
    if rel_path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        anyhow::bail!("path escapes the workspace");
    }
    Ok(root.join(rel_path))
}

/// List one directory level (dirs first, then files, both sorted).
pub fn list_dir(root: &Path, rel: &str) -> anyhow::Result<Vec<DirEntry>> {
    let dir = jail(root, rel)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        out.push(DirEntry { name, dir: is_dir });
    }
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// Whether the working tree has uncommitted work (staged, unstaged, or
/// untracked) — the guard before destructive worktree teardown.
pub async fn is_dirty(workdir: &str) -> anyhow::Result<bool> {
    let out = tokio::process::Command::new("git")
        .args(["-C", workdir, "status", "--porcelain"])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(!out.stdout.is_empty())
}

/// Commits reachable from the worktree's HEAD but from no other ref — work
/// that would become unreachable if the session branch were deleted. Guards
/// teardown of worktrees where the agent committed instead of leaving edits.
pub async fn unmerged_commit_count(workdir: &str, branch: &str) -> anyhow::Result<u32> {
    // NOT --all: it includes HEAD, which would negate the very commits we're
    // counting. Enumerate every OTHER branch/tag/remote instead (--exclude
    // applies to the next enumerator only, and patterns for --branches are
    // matched without the refs/heads/ prefix).
    let exclude = format!("--exclude={branch}");
    let out = tokio::process::Command::new("git")
        .args([
            "-C", workdir, "rev-list", "--count", "HEAD", "--not",
            &exclude, "--branches", "--tags", "--remotes",
        ])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0))
}

/// The working tree's diff against HEAD (staged + unstaged), unified format.
pub async fn git_diff(workdir: &str) -> anyhow::Result<String> {
    let out = tokio::process::Command::new("git")
        .args(["-C", workdir, "diff", "HEAD"])
        .output()
        .await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git diff failed: {}", err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Case-insensitive substring search over relative file paths (capped).
pub fn search_files(root: &Path, query: &str, cap: usize) -> Vec<String> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= cap {
                return out;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.to_lowercase().contains(&needle) {
                out.push(rel);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/components")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("node_modules/x")).unwrap();
        std::fs::write(tmp.path().join("README.md"), "hi").unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(tmp.path().join("src/components/App.tsx"), "x").unwrap();
        tmp
    }

    #[test]
    fn lists_dirs_first_and_skips_noise() {
        let tmp = tree();
        let entries = list_dir(tmp.path(), "").unwrap();
        let names: Vec<(String, bool)> = entries.iter().map(|e| (e.name.clone(), e.dir)).collect();
        assert_eq!(names, vec![("src".to_string(), true), ("README.md".to_string(), false)]);

        let sub = list_dir(tmp.path(), "src").unwrap();
        assert_eq!(sub[0].name, "components");
        assert!(sub.iter().any(|e| e.name == "main.rs" && !e.dir));
    }

    #[test]
    fn jail_rejects_escapes() {
        let tmp = tree();
        assert!(list_dir(tmp.path(), "../..").is_err());
        assert!(jail(tmp.path(), "C:\\Windows").is_err() || cfg!(not(windows)));
        assert!(jail(tmp.path(), "src/../..").is_err());
    }

    #[tokio::test]
    async fn dirty_and_unmerged_detection_on_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().into_owned();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-b", "main"]);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);

        // Clean checkout on a session branch that matches main → nothing at risk.
        git(&["checkout", "-b", "harness/abc123"]);
        assert!(!is_dirty(&dir).await.unwrap());
        assert_eq!(unmerged_commit_count(&dir, "harness/abc123").await.unwrap(), 0);

        // Uncommitted (untracked) work → dirty.
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();
        assert!(is_dirty(&dir).await.unwrap());

        // Committed work that exists ONLY on the session branch → unmerged.
        git(&["add", "."]);
        git(&["commit", "-m", "session work"]);
        assert!(!is_dirty(&dir).await.unwrap());
        assert_eq!(unmerged_commit_count(&dir, "harness/abc123").await.unwrap(), 1);
    }

    #[test]
    fn search_matches_relative_paths_case_insensitively() {
        let tmp = tree();
        let hits = search_files(tmp.path(), "app.TSX", 50);
        assert_eq!(hits, vec!["src/components/App.tsx".to_string()]);
        assert!(search_files(tmp.path(), "node_modules", 50).is_empty());
        assert!(search_files(tmp.path(), "", 50).is_empty());
    }
}
