//! `bash` — run a shell command in the session worktree.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command with `sh -c` in the working directory. Returns \
         merged stdout and stderr, plus the exit code on failure. Has a \
         timeout (default 120s, max 600s)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run."},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default 120, max 600)."}
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> &'static str {
        "execute"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let brief: String = cmd.chars().take(80).collect();
        PermissionSpec::new("bash", format!("run: {brief}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("bash: `command` is required"))?;
        let secs = input
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&ctx.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("bash: failed to spawn: {e}"))),
        };

        let output = tokio::select! {
            // Cancellation drops the wait future; kill_on_drop reaps the child.
            _ = ctx.cancel.cancelled() => {
                return Ok(ToolOutput::error("bash: interrupted"));
            }
            res = tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()) => {
                match res {
                    Ok(Ok(o)) => o,
                    Ok(Err(e)) => return Ok(ToolOutput::error(format!("bash: {e}"))),
                    Err(_) => return Ok(ToolOutput::error(format!(
                        "bash: timed out after {secs}s"
                    ))),
                }
            }
        };

        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&stderr);
        }
        let is_error = !output.status.success();
        if is_error {
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            text.push_str(&format!("\n[exit code {code}]"));
        }
        let text = truncate(&text, &ctx.caps);
        Ok(ToolOutput {
            for_model: text,
            display: None,
            is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn runs_command_in_workdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash.execute(&ctx, json!({"command": "ls"})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("marker.txt"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_tool_error_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash
            .execute(&ctx, json!({"command": "exit 3"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("exit code 3"));
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash
            .execute(&ctx, json!({"command": "sleep 5", "timeout": 1}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("timed out"));
    }

    #[tokio::test]
    async fn cancel_interrupts() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        ctx.cancel.cancel();
        let out = Bash
            .execute(&ctx, json!({"command": "sleep 5"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("interrupted"));
    }
}
