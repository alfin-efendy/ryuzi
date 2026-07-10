//! `exitplanmode` — present a Plan-mode plan for user review. Approval flips
//! the session's permission mode (and the project's persisted default);
//! rejection returns the user's feedback so the model can revise the plan.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::{ApprovalKind, PermMode};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ExitPlanMode;

#[async_trait]
impl Tool for ExitPlanMode {
    fn name(&self) -> &str {
        "exitplanmode"
    }
    fn description(&self) -> &str {
        "Present your implementation plan for user review when you are in plan \
         mode and ready to implement. The plan (markdown) is shown to the user; \
         they approve it (switching permissions so you can edit) or reject it \
         with feedback. Only callable in plan mode."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan": {"type": "string", "description": "The implementation plan, as markdown."}
            },
            "required": ["plan"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("exitplanmode", "review the proposed plan")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(interaction) = ctx.interaction.as_ref() else {
            return Ok(ToolOutput::error(
                "exitplanmode is not available in this context",
            ));
        };
        let plan = input.get("plan").and_then(|v| v.as_str()).unwrap_or("");
        if plan.trim().is_empty() {
            return Ok(ToolOutput::error("exitplanmode: 'plan' must be non-empty"));
        }
        if *interaction.perm_mode.lock().unwrap() != PermMode::Plan {
            return Ok(ToolOutput::error(
                "exitplanmode: session is not in plan mode",
            ));
        }
        let resp = interaction
            .request(
                &ctx.session_pk,
                &ctx.tool_call_id,
                "exitplanmode",
                "review the proposed plan",
                ApprovalKind::Plan,
                json!({ "plan": plan }),
                &ctx.cancel,
            )
            .await;
        let Some(resp) = resp else {
            return Ok(ToolOutput::error("Interrupted by user"));
        };
        if resp.allowed() {
            let mode = match resp
                .payload
                .as_ref()
                .and_then(|p| p.get("mode"))
                .and_then(|m| m.as_str())
            {
                Some("acceptEdits") => PermMode::AcceptEdits,
                _ => PermMode::Default,
            };
            *interaction.perm_mode.lock().unwrap() = mode;
            // Persist so the control plane's per-turn refresh (which re-reads
            // the project row) keeps the new mode instead of snapping back to
            // Plan.
            if let Some(pid) = interaction.project_id.as_deref() {
                let _ = ctx
                    .store
                    .update_project_prefs(pid, None, None, Some(mode))
                    .await;
            }
            Ok(ToolOutput::ok(format!(
                "Plan approved. Permission mode is now {} — proceed with the implementation.",
                mode.as_str()
            )))
        } else {
            let feedback = resp
                .payload
                .as_ref()
                .and_then(|p| p.get("feedback"))
                .and_then(|f| f.as_str())
                .unwrap_or("");
            let msg = if feedback.trim().is_empty() {
                "The user rejected the plan. Revise it and present again.".to_string()
            } else {
                format!(
                    "The user rejected the plan with this feedback:\n{feedback}\nRevise the plan and present it again."
                )
            };
            Ok(ToolOutput::ok(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{ctx_at, ctx_with_interaction};
    use super::*;
    use crate::domain::{ApprovalDecision, ApprovalKind, ApprovalResponse, CoreEvent, PermMode};
    use serde_json::json;

    #[tokio::test]
    async fn errors_outside_plan_mode() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _hub, _rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        let out = ExitPlanMode
            .execute(&ctx, json!({"plan": "do X"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("plan mode"));
    }

    #[tokio::test]
    async fn approve_switches_mode_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, perm) = ctx_with_interaction(dir.path(), PermMode::Plan).await;
        let waiter = tokio::spawn(async move {
            match rx.recv().await.unwrap() {
                CoreEvent::ApprovalRequested {
                    request_id,
                    approval_kind,
                    input,
                    ..
                } => {
                    assert_eq!(approval_kind, ApprovalKind::Plan);
                    assert_eq!(input["plan"], "do X");
                    hub.resolve(
                        &request_id,
                        ApprovalResponse {
                            decision: ApprovalDecision::AllowOnce,
                            scope: None,
                            payload: Some(json!({"mode": "acceptEdits"})),
                        },
                    );
                }
                other => panic!("unexpected {other:?}"),
            }
        });
        let out = ExitPlanMode
            .execute(&ctx, json!({"plan": "do X"}))
            .await
            .unwrap();
        waiter.await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("acceptEdits"));
        assert_eq!(*perm.lock().unwrap(), PermMode::AcceptEdits);
    }

    #[tokio::test]
    async fn reject_returns_feedback_and_stays_in_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, perm) = ctx_with_interaction(dir.path(), PermMode::Plan).await;
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                hub.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::RejectOnce,
                        scope: None,
                        payload: Some(json!({"feedback": "missing tests"})),
                    },
                );
            }
        });
        let out = ExitPlanMode
            .execute(&ctx, json!({"plan": "do X"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("missing tests"));
        assert_eq!(*perm.lock().unwrap(), PermMode::Plan);
    }

    #[tokio::test]
    async fn no_interaction_context_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await; // interaction: None
        let out = ExitPlanMode
            .execute(&ctx, json!({"plan": "p"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
