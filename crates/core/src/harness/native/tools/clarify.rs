//! `clarify` — ask the user ONE structured question (≤4 choices) mid-task and
//! block on the answer (spec §9.1). Reuses the approval/interaction channel
//! (`ApprovalKind::Question`) exactly like `askuserquestion`, but is part of
//! the app toolset and waits for an explicit response or cancellation.
//! Permission key `clarify` auto-allows (its execution IS the prompt).

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::{ApprovalDecision, ApprovalKind};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;
const DEFAULT_TIMEOUT_SECS: usize = 300;

pub struct Clarify;

/// Structural validation; returns a user-visible error string.
fn validate(input: &Value) -> Result<(), String> {
    let text = input.get("question").and_then(|v| v.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Err("'question' text is required".into());
    }
    let opts = input
        .get("options")
        .and_then(|o| o.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if opts.len() < MIN_OPTIONS || opts.len() > MAX_OPTIONS {
        return Err(format!("provide {MIN_OPTIONS}-{MAX_OPTIONS} options"));
    }
    if opts.iter().any(|o| {
        o.get("label")
            .and_then(|l| l.as_str())
            .unwrap_or("")
            .trim()
            .is_empty()
    }) {
        return Err("every option needs a non-empty 'label'".into());
    }
    Ok(())
}

#[async_trait]
impl Tool for Clarify {
    fn name(&self) -> &str {
        "clarify"
    }
    fn description(&self) -> &str {
        "Ask the user ONE multiple-choice question (2-4 options) when you are \
         blocked on a decision only they can make. Blocks until they answer or are \
         cancelled; do not use it for decisions you can make yourself."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "header": {"type": "string", "description": "short chip label, ~12 chars"},
                "options": {
                    "type": "array",
                    "minItems": MIN_OPTIONS,
                    "maxItems": MAX_OPTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {"type": "string"},
                            "description": {"type": "string"}
                        },
                        "required": ["label"]
                    }
                }
            },
            "required": ["question", "options"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("clarify", "answer the agent's question")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(interaction) = ctx.interaction.as_ref() else {
            return Ok(ToolOutput::error(
                "clarify is not available in this context",
            ));
        };
        if let Err(e) = validate(&input) {
            return Ok(ToolOutput::error(format!("clarify: {e}")));
        }
        // Wrap the multi-question card shape the UI already renders for
        // `askuserquestion` so both tools share one interaction surface.
        let card = json!({"questions": [ {
            "question": input.get("question").cloned().unwrap_or(Value::Null),
            "header": input.get("header").cloned().unwrap_or_else(|| json!("Clarify")),
            "options": input.get("options").cloned().unwrap_or_else(|| json!([])),
            "multiSelect": false
        } ]});

        let secs = crate::settings::usize_setting(
            &ctx.store,
            "clarify.timeout_secs",
            DEFAULT_TIMEOUT_SECS,
        )
        .await as u64;

        let fut = interaction.request(
            &ctx.session_pk,
            &ctx.tool_call_id,
            "clarify",
            "answer the agent's question",
            ApprovalKind::Question,
            card,
            &ctx.cancel,
        );
        let resp = match tokio::time::timeout(Duration::from_secs(secs), fut).await {
            Ok(r) => r,
            Err(_elapsed) => {
                // Deregister the pending approval so a late click can't hit a
                // stale entry and nothing leaks — never hang the turn.
                interaction.approvals.resolve_bool(
                    &crate::approval::ApprovalKey::new(&ctx.run_id, &ctx.tool_call_id),
                    false,
                );
                return Ok(ToolOutput::ok(
                    "No answer within the time limit; proceeding without it.",
                ));
            }
        };

        let Some(resp) = resp else {
            return Ok(ToolOutput::error("Interrupted by user"));
        };
        if resp.decision == ApprovalDecision::Cancel {
            return Ok(ToolOutput::ok(
                "No interactive surface answered this request.",
            ));
        }
        if !resp.allowed() {
            return Ok(ToolOutput::ok("The user declined to answer."));
        }
        let answers = resp
            .payload
            .as_ref()
            .and_then(|p| p.get("answers"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut lines = Vec::new();
        if let Some(map) = answers.as_object() {
            for (question, picked) in map {
                let picked = picked
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                lines.push(format!("Q: {question}\nA: {picked}"));
            }
        }
        if lines.is_empty() {
            return Ok(ToolOutput::ok("The user submitted no answer."));
        }
        Ok(ToolOutput::ok(lines.join("\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApprovalResponse, CoreEvent, PermMode};
    use crate::harness::native::tools::testutil::ctx_with_interaction;
    use serde_json::json;

    fn q() -> serde_json::Value {
        json!({"question": "Deploy now?", "options": [{"label": "Yes"}, {"label": "No"}]})
    }

    #[tokio::test]
    async fn waits_for_explicit_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        let mut clarify = tokio::spawn(async move { Clarify.execute(&ctx, q()).await.unwrap() });

        let (run_id, request_id) = match rx.recv().await.unwrap() {
            CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            } => (run_id, request_id),
            other => panic!("unexpected event {other:?}"),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut clarify)
                .await
                .is_err(),
            "clarify must remain pending until the user responds"
        );

        hub.resolve(
            &crate::approval::ApprovalKey::new(&run_id, &request_id),
            ApprovalResponse {
                decision: ApprovalDecision::AllowOnce,
                scope: None,
                payload: Some(json!({"answers": {"Deploy now?": ["No"]}})),
            },
        );
        let out = clarify.await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("No"));
    }

    #[tokio::test]
    async fn answer_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        let waiter = tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            }) = rx.recv().await
            {
                hub.resolve(
                    &crate::approval::ApprovalKey::new(run_id, request_id),
                    ApprovalResponse {
                        decision: ApprovalDecision::AllowOnce,
                        scope: None,
                        payload: Some(json!({"answers": {"Deploy now?": ["No"]}})),
                    },
                );
            }
        });
        let out = Clarify.execute(&ctx, q()).await.unwrap();
        waiter.await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("No"));
    }

    #[tokio::test]
    async fn missing_interaction_is_not_available() {
        use crate::harness::native::tools::testutil::ctx_at;
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await; // interaction: None
        let out = Clarify.execute(&ctx, q()).await.unwrap();
        assert!(out.is_error);
    }
}
