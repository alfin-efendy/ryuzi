//! `revert` — undo the most recent file changes by restoring the worktree to
//! the latest snapshot the runner recorded before a mutating tool ran.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::snapshot;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Revert;

#[async_trait]
impl Tool for Revert {
    fn name(&self) -> &str {
        "revert"
    }
    fn description(&self) -> &str {
        "Undo the most recent file modifications by restoring the worktree to \
         the last snapshot (taken automatically before edits and shell \
         commands). Reverts tracked-file changes; does not delete newly \
         created files."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn kind(&self) -> &'static str {
        "edit"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("edit", "revert recent changes")
    }
    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> anyhow::Result<ToolOutput> {
        let sha = {
            let mut stack = ctx.snapshots.lock().await;
            stack.pop()
        };
        let Some(sha) = sha else {
            return Ok(ToolOutput::error("revert: nothing to undo (no snapshots)"));
        };
        match snapshot::restore(&ctx.work_dir, &sha).await {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "reverted tracked files to snapshot {}",
                &sha[..sha.len().min(12)]
            ))),
            Err(e) => Ok(ToolOutput::error(format!("revert: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use std::sync::Arc;

    async fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .unwrap()
            .status
            .success());
    }

    #[tokio::test]
    async fn revert_restores_the_last_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q"]).await;
        git(p, &["config", "user.email", "t@t"]).await;
        git(p, &["config", "user.name", "t"]).await;
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        git(p, &["add", "."]).await;
        git(p, &["commit", "-qm", "init"]).await;

        let mut ctx = ctx_at(p).await;
        let snap = snapshot::take(p).await.unwrap();
        ctx.snapshots = Arc::new(tokio::sync::Mutex::new(vec![snap]));

        std::fs::write(p.join("a.txt"), "two\n").unwrap();
        let out = Revert.execute(&ctx, json!({})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "one\n");
    }

    #[tokio::test]
    async fn revert_with_no_snapshots_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Revert.execute(&ctx, json!({})).await.unwrap();
        assert!(out.is_error);
    }
}
