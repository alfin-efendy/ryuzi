//! `app_orchestrate` — the agent dispatches / inspects a group-chat
//! orchestration (spec §9.1, §8). `submit` opens an orchestration for a goal;
//! `status`/`cancel`/`retry` operate on an existing tree. Reads auto-allow
//! (`orch.read`); mutations prompt (`orch.write`).
//!
//! `submit` requires an attached project: `orch::submit` needs a non-null
//! project_id and the home-chat linkage is Phase-5 work (see plan Open
//! Question 1). From a project-less chat we return an actionable error.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct AppOrchestrate;

fn action(input: &Value) -> &str {
    input.get("action").and_then(|a| a.as_str()).unwrap_or("")
}

#[async_trait]
impl Tool for AppOrchestrate {
    fn name(&self) -> &str {
        "app_orchestrate"
    }
    fn description(&self) -> &str {
        "Break a goal into a coordinated multi-agent orchestration and run it as \
         a group chat: submit a goal, check status, cancel, or retry a failed \
         task. Requires an attached project."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["submit", "status", "cancel", "retry"]},
                "goal": {"type": "string", "description": "the goal to decompose (submit)"},
                "project_id": {"type": "string", "description": "target project (submit)"},
                "id": {"type": "string", "description": "root id (status) or task id (cancel/retry)"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        match action(input) {
            "status" => PermissionSpec::new("orch.read", "inspect an orchestration"),
            other => PermissionSpec::new("orch.write", format!("{other} an orchestration")),
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(app) = ctx.app.as_ref() else {
            return Ok(ToolOutput::error(
                "app_orchestrate is not available in this context",
            ));
        };
        match action(&input) {
            "submit" => {
                let goal = input
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if goal.is_empty() {
                    return Ok(ToolOutput::error("submit requires a non-empty 'goal'"));
                }
                // The originating session's project, else the explicit param.
                let project_id = input
                    .get("project_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| ctx.interaction.as_ref().and_then(|i| i.project_id.clone()));
                let Some(project_id) = project_id else {
                    return Ok(ToolOutput::error(
                        "orchestration needs a project — attach one to this chat, \
                         or pass 'project_id'.",
                    ));
                };
                let root = app.submit_orchestration(&project_id, &goal).await?;
                Ok(ToolOutput::ok(format!(
                    "Orchestration {root} submitted. Workers will post into this chat as they run."
                )))
            }
            "status" => {
                let root = input.get("id").and_then(|v| v.as_str());
                let tasks = app.list_orchestrations(root).await?;
                if tasks.is_empty() {
                    return Ok(ToolOutput::ok("No orchestration tasks."));
                }
                let lines: Vec<String> = tasks
                    .iter()
                    .map(|t| {
                        let verdict = t
                            .result
                            .as_deref()
                            .filter(|r| !r.trim().is_empty())
                            .map(|r| format!("\n    ↳ {r}"))
                            .unwrap_or_default();
                        format!(
                            "- [{}] {} — {} ({}){}",
                            t.id, t.title, t.status, t.agent, verdict
                        )
                    })
                    .collect();
                Ok(ToolOutput::ok(lines.join("\n")))
            }
            "cancel" => {
                let Some(id) = input.get("id").and_then(|v| v.as_str()) else {
                    return Ok(ToolOutput::error("cancel requires 'id'"));
                };
                let n = app.cancel_orchestration(id).await?;
                Ok(ToolOutput::ok(format!("Cancelled {n} task(s) under {id}.")))
            }
            "retry" => {
                let Some(id) = input.get("id").and_then(|v| v.as_str()) else {
                    return Ok(ToolOutput::error("retry requires 'id'"));
                };
                let ok = app.retry_orchestration(id).await?;
                Ok(ToolOutput::ok(if ok {
                    format!("Task {id} re-queued.")
                } else {
                    format!("Task {id} was not retryable.")
                }))
            }
            other => Ok(ToolOutput::error(format!("unknown action {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::tools::testutil::{ctx_at, FakeAppControl};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn submit_requires_a_project_then_calls_facade() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.app = Some(Arc::new(FakeAppControl::default()));
        // No project attached → clear error, no submit.
        let out = AppOrchestrate
            .execute(&ctx, json!({"action": "submit", "goal": "ship it"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("project"));
        // With a project id → submits.
        let out = AppOrchestrate
            .execute(
                &ctx,
                json!({"action": "submit", "goal": "ship it", "project_id": "p1"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("orch-root"));
    }

    #[test]
    fn permission_keys() {
        assert_eq!(
            AppOrchestrate.permission(&json!({"action": "status"})).key,
            "orch.read"
        );
        assert_eq!(
            AppOrchestrate.permission(&json!({"action": "submit"})).key,
            "orch.write"
        );
    }
}
