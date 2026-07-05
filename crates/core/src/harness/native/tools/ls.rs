//! `ls` — list a directory within the session worktree.

use super::{jail, truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Ls;

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn description(&self) -> &'static str {
        "List the entries of a directory relative to the working directory. \
         Directories are suffixed with `/`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory path relative to the working directory (default `.`)."}
            }
        })
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        PermissionSpec::new("read", format!("list {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        let mut rd = match tokio::fs::read_dir(&resolved).await {
            Ok(r) => r,
            Err(e) => return Ok(ToolOutput::error(format!("ls: {path}: {e}"))),
        };
        let mut entries: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(if is_dir { format!("{name}/") } else { name });
        }
        entries.sort();
        Ok(ToolOutput::ok(truncate(&entries.join("\n"), &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn lists_files_and_dirs_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Ls.execute(&ctx, json!({"path": "."})).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "adir/\nb.txt");
    }
}
