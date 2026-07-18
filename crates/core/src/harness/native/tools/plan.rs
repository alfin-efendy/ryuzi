//! `exitplanmode` — present a Plan-mode plan for user review. Approval flips
//! the session's permission mode and persists it to THIS session's row only
//! (per-session mode — sibling sessions on the same project are unaffected);
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
        if resp.decision == crate::domain::ApprovalDecision::Cancel {
            return Ok(ToolOutput::ok(
                "No interactive surface answered this request.",
            ));
        }
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
            // the SESSION row) keeps the new mode instead of snapping back to
            // Plan. Per-session by design — approving a plan here must not
            // change sibling sessions.
            let _ = ctx
                .store
                .update_session_perm_mode(&ctx.session_pk, mode)
                .await;
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
                    run_id,
                    request_id,
                    approval_kind,
                    input,
                    ..
                } => {
                    assert_eq!(approval_kind, ApprovalKind::Plan);
                    assert_eq!(input["plan"], "do X");
                    hub.resolve(
                        &crate::approval::ApprovalKey::new(run_id, request_id),
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
    async fn approve_persists_perm_mode_to_the_session_row() {
        // ctx_with_interaction's in-memory store never seeds a session row,
        // so update_session_perm_mode's UPDATE previously matched zero rows
        // and silently no-opped — the plan-approval tests passed vacuously
        // without ever proving the persistence actually took effect. Seed a
        // real project + session (shape mirrors runner.rs's
        // seed_pinned_project) whose pk matches ctx.session_pk so the write
        // lands, then read the row back.
        use crate::domain::{Project, Session, SessionKind, SessionStatus};

        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, perm) = ctx_with_interaction(dir.path(), PermMode::Plan).await;
        ctx.store
            .insert_project(Project {
                project_id: "p".into(),
                name: "p".into(),
                workdir: "/w".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::Plan,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        ctx.store
            .insert_session(Session {
                session_pk: ctx.session_pk.clone(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: Some("p".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("titled".into()),
                status: SessionStatus::Running,
                perm_mode: PermMode::Plan,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: true,
                kind: SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
                archived_at: None,
            })
            .await
            .unwrap();

        let waiter = tokio::spawn(async move {
            match rx.recv().await.unwrap() {
                CoreEvent::ApprovalRequested {
                    run_id, request_id, ..
                } => {
                    hub.resolve(
                        &crate::approval::ApprovalKey::new(run_id, request_id),
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
        assert_eq!(*perm.lock().unwrap(), PermMode::AcceptEdits);

        let session = ctx
            .store
            .get_session(&ctx.session_pk)
            .await
            .unwrap()
            .expect("seeded session row must still exist");
        assert_eq!(
            session.perm_mode,
            PermMode::AcceptEdits,
            "approval must persist to the session row, not just the in-memory mutex"
        );
    }

    #[tokio::test]
    async fn reject_returns_feedback_and_stays_in_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, perm) = ctx_with_interaction(dir.path(), PermMode::Plan).await;
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            }) = rx.recv().await
            {
                hub.resolve(
                    &crate::approval::ApprovalKey::new(run_id, request_id),
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
    async fn cancel_decision_is_reported_as_no_interactive_surface() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, perm) = ctx_with_interaction(dir.path(), PermMode::Plan).await;
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            }) = rx.recv().await
            {
                hub.resolve(
                    &crate::approval::ApprovalKey::new(run_id, request_id),
                    ApprovalResponse {
                        decision: ApprovalDecision::Cancel,
                        scope: None,
                        payload: None,
                    },
                );
            }
        });
        let out = ExitPlanMode
            .execute(&ctx, json!({"plan": "do X"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            out.for_model,
            "No interactive surface answered this request."
        );
        // A timed-out headless prompt must leave the session in Plan mode —
        // it's neither an approval nor a user rejection.
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
