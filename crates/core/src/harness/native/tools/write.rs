//! `write` — create or overwrite a text file within the session worktree.

use super::{jail, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write `content` to a file relative to the working directory, creating \
         parent directories as needed. Overwrites an existing file."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."},
                "content": {"type": "string", "description": "Full file contents to write."}
            },
            "required": ["path", "content"]
        })
    }
    fn kind(&self) -> &'static str {
        "edit"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("edit", format!("write {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("write: `path` is required"))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("write: `content` is required"))?;
        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        if let Some(parent) = resolved.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolOutput::error(format!("write: {path}: {e}")));
            }
        }
        if let Err(e) = tokio::fs::write(&resolved, content).await {
            return Ok(ToolOutput::error(format!("write: {path}: {e}")));
        }
        let mut msg = format!("wrote {} bytes to {path}", content.len());
        if let Some(fmt) = crate::harness::native::format::maybe_format(&resolved).await {
            msg.push_str(&format!(" (formatted with {fmt})"));
        }
        Ok(ToolOutput::ok(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn writes_file_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Write
            .execute(&ctx, json!({"path": "sub/dir/f.txt", "content": "hi"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        let got = std::fs::read_to_string(dir.path().join("sub/dir/f.txt")).unwrap();
        assert_eq!(got, "hi");
    }

    #[tokio::test]
    async fn escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Write
            .execute(&ctx, json!({"path": "../evil.txt", "content": "x"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
