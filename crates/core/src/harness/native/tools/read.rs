//! `read` — read a text file within the session worktree.

use super::{jail, truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

/// 2 MiB read cap, matching the ACP fs handler and Cockpit's `read_file`.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file relative to the working directory. Supports an \
         optional line offset and limit. Output lines are prefixed with their \
         1-based line number."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."},
                "offset": {"type": "integer", "description": "1-based line to start from (default 1)."},
                "limit": {"type": "integer", "description": "Maximum number of lines to read (default 2000)."}
            },
            "required": ["path"]
        })
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("read {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("read: `path` is required"))?;
        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        let meta = match tokio::fs::metadata(&resolved).await {
            Ok(m) => m,
            Err(e) => return Ok(ToolOutput::error(format!("read: {path}: {e}"))),
        };
        if meta.len() > MAX_READ_BYTES {
            return Ok(ToolOutput::error(format!(
                "read: {path} is {} bytes, over the {MAX_READ_BYTES} byte cap",
                meta.len()
            )));
        }
        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("read: {path}: {e}"))),
        };
        let offset = input
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;
        let numbered = content
            .lines()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(truncate(&numbered, &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn reads_numbered_lines_and_honors_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "f.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("     2\tb"));
        assert!(out.for_model.contains("     3\tc"));
        assert!(!out.for_model.contains("\ta\n") && !out.for_model.contains("     1\ta"));
    }

    #[tokio::test]
    async fn missing_file_is_a_tool_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "nope.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "../secret"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
