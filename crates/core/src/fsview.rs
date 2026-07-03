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

    #[test]
    fn search_matches_relative_paths_case_insensitively() {
        let tmp = tree();
        let hits = search_files(tmp.path(), "app.TSX", 50);
        assert_eq!(hits, vec!["src/components/App.tsx".to_string()]);
        assert!(search_files(tmp.path(), "node_modules", 50).is_empty());
        assert!(search_files(tmp.path(), "", 50).is_empty());
    }
}
