//! Best-effort code formatting after an edit/write, mirroring opencode's
//! formatter hook. Runs a well-known formatter for the file's extension if the
//! binary is on PATH; any failure (missing tool, non-zero exit) is silently
//! skipped so it never turns a successful edit into an error.

use std::path::Path;
use std::process::Stdio;

/// Run a formatter for `path` if one is known and available. Returns the
/// formatter name on success, `None` otherwise.
pub async fn maybe_format(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let p = path.to_string_lossy().to_string();
    let (cmd, args): (&str, Vec<String>) = match ext {
        "rs" => ("rustfmt", vec![p]),
        "go" => ("gofmt", vec!["-w".into(), p]),
        "py" => ("black", vec!["-q".into(), p]),
        "js" | "jsx" | "ts" | "tsx" | "json" | "jsonc" | "css" | "scss" | "html" | "md"
        | "yaml" | "yml" => ("prettier", vec!["--write".into(), p]),
        _ => return None,
    };
    let mut command = tokio::process::Command::new(cmd);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::process_util::no_window(&mut command);
    let status = command.status().await;
    match status {
        Ok(s) if s.success() => Some(cmd.to_string()),
        _ => None, // tool missing or failed — skip silently
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn formats_rust_with_rustfmt_when_available() {
        // rustfmt ships with the Rust toolchain, so this exercises the real path.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn  main( ){let x=1;}\n").unwrap();
        let formatted = maybe_format(&file).await;
        if formatted.is_some() {
            let out = std::fs::read_to_string(&file).unwrap();
            assert!(
                out.contains("fn main()"),
                "rustfmt should tidy spacing: {out:?}"
            );
        }
        // If rustfmt isn't on PATH the call returns None and the file is
        // unchanged — still a valid outcome for this best-effort hook.
    }

    #[tokio::test]
    async fn unknown_extension_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.unknownext");
        std::fs::write(&file, "x").unwrap();
        assert!(maybe_format(&file).await.is_none());
    }
}
