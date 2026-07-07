//! `glob` — match file names under the worktree, gitignore-aware.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files by glob pattern (e.g. `**/*.rs`) under the working \
         directory, ignoring gitignored files. Returns matching paths relative \
         to the working directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern, e.g. `src/**/*.ts`."},
                "path": {"type": "string", "description": "Subdirectory to search under (default `.`)."}
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> &'static str {
        "search"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("glob {pat}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("glob: `pattern` is required"))?
            .to_string();
        let sub = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let root = match super::jail(&ctx.work_dir, &sub) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        let base = ctx.work_dir.canonicalize().unwrap_or(ctx.work_dir.clone());

        let matcher = match GlobBuilder::new(&pattern).literal_separator(true).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return Ok(ToolOutput::error(format!("glob: bad pattern: {e}"))),
        };

        // WalkBuilder is blocking; run it off the async runtime.
        let out = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<(std::time::SystemTime, String)> = Vec::new();
            for result in WalkBuilder::new(&root).hidden(false).build() {
                let Ok(entry) = result else { continue };
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(true) {
                    continue;
                }
                let path = entry.path();
                let rel: PathBuf = path.strip_prefix(&base).unwrap_or(path).to_path_buf();
                if matcher.is_match(&rel) {
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    // Model-visible paths use forward slashes on every
                    // platform (Windows PathBuf joins with `\`).
                    let rel = rel.to_string_lossy().to_string();
                    #[cfg(windows)]
                    let rel = rel.replace('\\', "/");
                    hits.push((mtime, rel));
                }
            }
            // Newest first.
            hits.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
            hits.into_iter().map(|(_, p)| p).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        if out.is_empty() {
            return Ok(ToolOutput::ok(format!("no files match `{pattern}`")));
        }
        Ok(ToolOutput::ok(truncate(&out.join("\n"), &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn matches_by_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/b.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Glob
            .execute(&ctx, json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("src/a.rs"));
        assert!(!out.for_model.contains("b.txt"));
    }

    #[tokio::test]
    async fn no_match_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Glob
            .execute(&ctx, json!({"pattern": "**/*.zzz"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("no files match"));
    }
}
