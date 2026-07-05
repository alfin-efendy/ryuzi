//! `grep` — ripgrep-style content search under the worktree.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::{Searcher, SearcherBuilder};
use ignore::WalkBuilder;
use serde_json::{json, Value};

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search file contents for a regular expression under the working \
         directory, ignoring gitignored files. Returns `path:line:text` for \
         each match. Optionally restrict to files whose name matches `include` \
         (a glob)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression to search for."},
                "path": {"type": "string", "description": "Subdirectory to search under (default `.`)."},
                "include": {"type": "string", "description": "Only search files matching this glob (e.g. `*.rs`)."}
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> &'static str {
        "search"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("grep {pat}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("grep: `pattern` is required"))?
            .to_string();
        let sub = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let include = input
            .get("include")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let root = match super::jail(&ctx.work_dir, &sub) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        let base = ctx.work_dir.canonicalize().unwrap_or(ctx.work_dir.clone());

        let matcher = match RegexMatcher::new(&pattern) {
            Ok(m) => m,
            Err(e) => return Ok(ToolOutput::error(format!("grep: bad pattern: {e}"))),
        };
        let include_matcher = match &include {
            Some(g) => match globset::GlobBuilder::new(g).build() {
                Ok(gg) => Some(gg.compile_matcher()),
                Err(e) => return Ok(ToolOutput::error(format!("grep: bad include glob: {e}"))),
            },
            None => None,
        };

        let out = tokio::task::spawn_blocking(move || {
            let mut lines: Vec<String> = Vec::new();
            let mut searcher: Searcher = SearcherBuilder::new().build();
            for result in WalkBuilder::new(&root).hidden(false).build() {
                let Ok(entry) = result else { continue };
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(true) {
                    continue;
                }
                let path = entry.path();
                if let Some(im) = &include_matcher {
                    let name = path.file_name().map(std::path::Path::new);
                    if !name.map(|n| im.is_match(n)).unwrap_or(false) {
                        continue;
                    }
                }
                let rel = path
                    .strip_prefix(&base)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                let _ = searcher.search_path(
                    &matcher,
                    path,
                    UTF8(|lnum, line| {
                        lines.push(format!("{rel}:{lnum}:{}", line.trim_end()));
                        Ok(true)
                    }),
                );
                if lines.len() > 5000 {
                    break;
                }
            }
            lines
        })
        .await
        .unwrap_or_default();

        if out.is_empty() {
            return Ok(ToolOutput::ok(format!("no matches for `{pattern}`")));
        }
        Ok(ToolOutput::ok(truncate(&out.join("\n"), &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn finds_matching_lines_with_path_and_lineno() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Grep
            .execute(&ctx, json!({"pattern": "fn main"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("a.rs:1:fn main() {}"));
    }

    #[tokio::test]
    async fn include_filter_restricts_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "needle\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Grep
            .execute(&ctx, json!({"pattern": "needle", "include": "*.rs"}))
            .await
            .unwrap();
        assert!(out.for_model.contains("a.rs"));
        assert!(!out.for_model.contains("b.txt"));
    }

    #[tokio::test]
    async fn no_match_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "nothing\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Grep.execute(&ctx, json!({"pattern": "zzz"})).await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("no matches"));
    }
}
