//! `lsp` — on-demand LSP diagnostics for a file, via the native LSP client.

use super::{jail, truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::lsp;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Lsp;

#[async_trait]
impl Tool for Lsp {
    fn name(&self) -> &str {
        "lsp"
    }
    fn description(&self) -> &str {
        "Get language-server diagnostics (errors/warnings) for a file. Requires \
         the language server (rust-analyzer, typescript-language-server, pylsp, \
         gopls) to be installed. Useful to check work after an edit."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."}
            },
            "required": ["path"]
        })
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("lsp diagnostics {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("lsp: `path` is required"))?;
        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(e.to_string())),
        };
        match lsp::diagnostics(&ctx.work_dir, &resolved).await {
            Ok(None) => Ok(ToolOutput::ok(format!(
                "lsp: no language server configured for {path}"
            ))),
            Ok(Some(lines)) if lines.is_empty() => {
                Ok(ToolOutput::ok(format!("no diagnostics for {path}")))
            }
            Ok(Some(lines)) => Ok(ToolOutput::ok(truncate(
                &format!("{path}:\n{}", lines.join("\n")),
                &ctx.caps,
            ))),
            Err(e) => Ok(ToolOutput::error(format!("lsp: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn unknown_extension_reports_no_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Lsp.execute(&ctx, json!({"path": "a.txt"})).await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("no language server"));
    }
}
