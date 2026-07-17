//! `ls` — list a directory within the session worktree.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::file_reference::{
    normalize_resolved_path, resolve_workspace_reference,
};
use crate::harness::native::tool_contract::{NormalizedInput, ToolError, ToolInputCtx};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Ls;

fn input_context(ctx: &ToolCtx) -> ToolInputCtx<'_> {
    ToolInputCtx {
        work_dir: &ctx.work_dir,
        attachments_dir: None,
        extra_skill_dirs: &[],
    }
}

fn normalize_ls_input(ctx: &ToolInputCtx<'_>, input: Value) -> Result<NormalizedInput, ToolError> {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return Ok(NormalizedInput::unchanged(input));
    };
    let target = resolve_workspace_reference(ctx, path)?;
    normalize_resolved_path(input, &target)
}

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
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
    fn normalize_input(
        &self,
        ctx: &ToolInputCtx<'_>,
        input: Value,
    ) -> Result<NormalizedInput, ToolError> {
        normalize_ls_input(ctx, input)
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        PermissionSpec::new("read", format!("list {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let input = match normalize_ls_input(&input_context(ctx), input) {
            Ok(input) => input.value,
            Err(error) => return Ok(ToolOutput::from_error(error)),
        };
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let target = match resolve_workspace_reference(&input_context(ctx), path) {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::from_error(error)),
        };
        let mut rd = match tokio::fs::read_dir(&target.resolved_path).await {
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
    use crate::harness::native::tool_contract::ToolInputCtx;

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

    #[tokio::test]
    async fn location_is_metadata_only_and_lists_the_selected_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/item.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let input_ctx = ToolInputCtx {
            work_dir: &ctx.work_dir,
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let normalized = Ls
            .normalize_input(&input_ctx, json!({"path": "sub:7:3"}))
            .unwrap();
        assert_eq!(normalized.value, json!({"path": "sub"}));
        let metadata = serde_json::to_value(normalized.metadata()).unwrap();
        assert_eq!(metadata[0]["value"]["line"], 7);
        assert_eq!(metadata[0]["value"]["column"], 3);

        let out = Ls.execute(&ctx, normalized.value).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(out.for_model, "item.txt");
    }
}
