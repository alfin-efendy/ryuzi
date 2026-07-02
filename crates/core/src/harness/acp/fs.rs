//! Client-side ACP fs handler: sandboxed `fs/read_text_file` and
//! `fs/write_text_file` implementations.
//!
//! The ACP protocol allows the agent to request file reads and writes from the
//! client. These handlers enforce that all paths are confined to the session's
//! `work_dir` (the session worktree). Any path that would escape — via `..`
//! traversal or an absolute path outside the root — is rejected with an error.
//!
//! # 2 MB read cap
//! `read_text_file` enforces a 2 MB cap identical to the one in
//! `apps/cockpit/src-tauri/src/commands.rs::read_file`.

use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest, WriteTextFileResponse,
};

/// Maximum file size allowed by `read_text_file` (2 MiB).
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Resolve `path` relative to `work_dir` and verify it stays inside `work_dir`.
///
/// Rules:
/// - If `path` is relative it is joined onto `work_dir`.
/// - If `path` is absolute it must already start with `work_dir`.
/// - After joining, `..` components are resolved lexically by normalizing the
///   combined path, then the lowest existing ancestor is canonicalized. This
///   blocks traversal escapes while allowing the file (or its parent dirs) to
///   not exist yet (e.g. for write targets).
///
/// Returns the resolved absolute path on success, or an error if the path
/// escapes the worktree.
pub fn sandbox(work_dir: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    // Canonicalize work_dir so we compare against the real on-disk root and so
    // a symlinked work_dir doesn't cause false rejections on relative paths.
    let canonical_root = work_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!("sandbox: cannot canonicalize work_dir {}: {e}", work_dir.display())
    })?;

    // Construct the candidate (absolute) path, resolving `..` lexically.
    // Use the *canonicalized* root as the base for relative joins so that any
    // symlink in work_dir is resolved before we concatenate the user path.
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        canonical_root.join(path)
    };

    // Lexically normalize: walk components and collapse `..` without I/O.
    // This catches `..` escapes before any canonicalize call.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in raw.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();

    // Quick check on the lexically normalized path before canonicalization.
    // An absolute path that isn't a prefix of canonical_root after normalization
    // is definitely an escape.
    if !normalized.starts_with(&canonical_root) {
        anyhow::bail!(
            "sandbox: path {} escapes the worktree {}",
            path.display(),
            canonical_root.display()
        );
    }

    // Now canonicalize the deepest existing ancestor to resolve any symlinks in
    // the directory chain and re-verify. Walk upward until we find an extant dir.
    let mut ancestor = normalized.as_path();
    loop {
        if ancestor.exists() {
            let canonical_ancestor = ancestor.canonicalize().map_err(|e| {
                anyhow::anyhow!(
                    "sandbox: cannot canonicalize {}: {e}",
                    ancestor.display()
                )
            })?;
            // Verify the canonicalized ancestor is still under the root.
            if !canonical_ancestor.starts_with(&canonical_root) {
                anyhow::bail!(
                    "sandbox: path {} escapes the worktree {} (symlink)",
                    path.display(),
                    canonical_root.display()
                );
            }
            // Reconstruct: canonical_ancestor + the suffix that didn't exist.
            // NOTE: PathBuf::join("") appends a trailing slash which causes
            // "Not a directory" on stat, so guard the empty-suffix case.
            let suffix = normalized.strip_prefix(ancestor).unwrap_or(std::path::Path::new(""));
            if suffix == std::path::Path::new("") {
                return Ok(canonical_ancestor);
            }
            return Ok(canonical_ancestor.join(suffix));
        }
        match ancestor.parent() {
            Some(p) => ancestor = p,
            None => anyhow::bail!("sandbox: cannot resolve any ancestor of {}", path.display()),
        }
    }
}

/// Handle an agent `fs/read_text_file` request.
///
/// Sandboxes the path to `work_dir`, enforces a 2 MiB cap, honours the
/// `.line` / `.limit` window parameters, and returns a `ReadTextFileResponse`.
pub fn read_text_file(
    work_dir: &Path,
    req: ReadTextFileRequest,
) -> anyhow::Result<ReadTextFileResponse> {
    let resolved = sandbox(work_dir, &req.path)?;

    // Size cap (synchronous stat is fine — we're in a blocking context).
    let meta = std::fs::metadata(&resolved)?;
    if meta.len() > MAX_READ_BYTES {
        anyhow::bail!(
            "fs/read_text_file: file too large ({} bytes, max {})",
            meta.len(),
            MAX_READ_BYTES
        );
    }

    let full_content = std::fs::read_to_string(&resolved)?;

    // Apply line / limit window (1-based).
    let content = if req.line.is_some() || req.limit.is_some() {
        let start = req.line.unwrap_or(1).saturating_sub(1) as usize;
        let lines: Vec<&str> = full_content.lines().collect();
        let slice = &lines[start.min(lines.len())..];
        let slice = if let Some(limit) = req.limit {
            &slice[..slice.len().min(limit as usize)]
        } else {
            slice
        };
        slice.join("\n")
    } else {
        full_content
    };

    Ok(ReadTextFileResponse::new(content))
}

/// Handle an agent `fs/write_text_file` request.
///
/// Sandboxes the path to `work_dir`, creates any missing parent directories,
/// and writes the content. Returns a `WriteTextFileResponse` on success.
pub fn write_text_file(
    work_dir: &Path,
    req: WriteTextFileRequest,
) -> anyhow::Result<WriteTextFileResponse> {
    let resolved = sandbox(work_dir, &req.path)?;

    // Create parent directories if needed (the sandbox check already passed).
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&resolved, &req.content)?;

    tracing::debug!(
        path = %resolved.display(),
        bytes = req.content.len(),
        "fs/write_text_file: wrote file"
    );

    Ok(WriteTextFileResponse::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::SessionId;

    #[test]
    fn sandbox_confines_to_work_dir_and_rejects_escapes() {
        let root = tempfile::tempdir().unwrap();
        // an in-root relative path resolves under root:
        let ok = sandbox(root.path(), std::path::Path::new("sub/file.txt")).unwrap();
        assert!(ok.starts_with(root.path()));
        // escapes are rejected:
        assert!(
            sandbox(root.path(), std::path::Path::new("../../etc/passwd")).is_err(),
            "expected .. escape to be rejected"
        );
        assert!(
            sandbox(root.path(), std::path::Path::new("/etc/passwd")).is_err(),
            "expected absolute path outside root to be rejected"
        );
    }

    #[test]
    fn read_text_file_returns_content_within_sandbox() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("hello.txt");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let req = ReadTextFileRequest::new(SessionId::from("test-session"), &file_path);
        let resp = read_text_file(root.path(), req).unwrap();
        assert_eq!(resp.content, "line1\nline2\nline3\n");
    }

    #[test]
    fn read_text_file_honours_line_and_limit() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("data.txt");
        std::fs::write(&file_path, "a\nb\nc\nd\ne\n").unwrap();

        let req = ReadTextFileRequest::new(SessionId::from("test-session"), &file_path)
            .line(2u32)
            .limit(3u32);
        let resp = read_text_file(root.path(), req).unwrap();
        // Lines 2-4: b, c, d
        assert_eq!(resp.content, "b\nc\nd");
    }

    #[test]
    fn write_text_file_creates_file_in_sandbox() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("output.txt");

        let req = WriteTextFileRequest::new(
            SessionId::from("test-session"),
            &target,
            "hello from agent",
        );
        write_text_file(root.path(), req).unwrap();

        let got = std::fs::read_to_string(&target).unwrap();
        assert_eq!(got, "hello from agent");
    }

    #[test]
    fn write_text_file_creates_parent_dirs() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("deep/nested/dir/file.txt");

        let req = WriteTextFileRequest::new(
            SessionId::from("test-session"),
            &target,
            "content",
        );
        write_text_file(root.path(), req).unwrap();
        assert!(target.exists());
    }

    #[test]
    fn write_text_file_rejects_escape() {
        let root = tempfile::tempdir().unwrap();
        let bad_path = std::path::PathBuf::from("/etc/passwd");

        let req =
            WriteTextFileRequest::new(SessionId::from("test-session"), bad_path, "content");
        assert!(write_text_file(root.path(), req).is_err());
    }
}
