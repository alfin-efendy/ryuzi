//! `edit` — exact-string replacement within a worktree file, with a diff.

use super::{jail, truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use similar::TextDiff;

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace an exact string in a file relative to the working directory. \
         By default `old_string` must occur exactly once; set `replace_all` to \
         replace every occurrence. Returns a unified diff of the change."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."},
                "old_string": {"type": "string", "description": "Exact text to replace."},
                "new_string": {"type": "string", "description": "Replacement text."},
                "replace_all": {"type": "boolean", "description": "Replace all occurrences (default false)."}
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn kind(&self) -> &'static str {
        "edit"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("edit", format!("edit {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `path` is required"))?;
        let old = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `old_string` is required"))?;
        let new = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `new_string` is required"))?;
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("edit: {path}: {e}"))),
        };
        let count = content.matches(old).count();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "edit: `old_string` not found in {path}"
            )));
        }
        if count > 1 && !replace_all {
            return Ok(ToolOutput::error(format!(
                "edit: `old_string` occurs {count} times in {path}; make it unique or set replace_all"
            )));
        }
        let updated = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        if let Err(e) = tokio::fs::write(&resolved, &updated).await {
            return Ok(ToolOutput::error(format!("edit: {path}: {e}")));
        }
        let diff = TextDiff::from_lines(&content, &updated)
            .unified_diff()
            .header(path, path)
            .to_string();
        Ok(ToolOutput::ok(truncate(
            &format!("edited {path}\n{diff}"),
            &ctx.caps,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn replaces_unique_string_and_returns_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "world", "new_string": "rust"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "hello rust\n"
        );
        assert!(out.for_model.contains("-hello world"));
        assert!(out.for_model.contains("+hello rust"));
    }

    #[tokio::test]
    async fn non_unique_match_without_replace_all_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a a a").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("occurs 3 times"));
    }

    #[tokio::test]
    async fn missing_old_string_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "abc").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "zzz", "new_string": "y"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
