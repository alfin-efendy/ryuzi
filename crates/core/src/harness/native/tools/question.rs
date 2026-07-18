//! `askuserquestion` — ask the user 1–4 structured multiple-choice questions
//! mid-turn. The card UI renders radio/checkbox options (plus a free-text
//! "Other"); the answers come back as the tool result.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::ApprovalKind;
use async_trait::async_trait;
use serde_json::{json, Value};

const MAX_QUESTIONS: usize = 4;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

/// Structural validation; returns a user-visible error string.
fn validate(input: &Value) -> Result<(), String> {
    let Some(questions) = input.get("questions").and_then(|q| q.as_array()) else {
        return Err("'questions' must be an array".into());
    };
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(format!("provide 1-{MAX_QUESTIONS} questions"));
    }
    for q in questions {
        let text = q.get("question").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return Err("each question needs non-empty 'question' text".into());
        }
        let opts = q
            .get("options")
            .and_then(|o| o.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if opts.len() < MIN_OPTIONS || opts.len() > MAX_OPTIONS {
            return Err(format!(
                "each question needs {MIN_OPTIONS}-{MAX_OPTIONS} options"
            ));
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
    }
    Ok(())
}

pub struct AskUserQuestion;

#[async_trait]
impl Tool for AskUserQuestion {
    fn name(&self) -> &str {
        "askuserquestion"
    }
    fn description(&self) -> &str {
        "Ask the user up to 4 multiple-choice questions when you are blocked on \
         a decision only they can make. Each question has 2-4 options; the user \
         can also type a free-form answer. Do not use this for decisions you can \
         make yourself from the code or the request."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {"type": "string"},
                            "header": {"type": "string", "description": "Short chip label, max ~12 chars"},
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
                            },
                            "multiSelect": {"type": "boolean"}
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("askuserquestion", "answer the agent's question")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(interaction) = ctx.interaction.as_ref() else {
            return Ok(ToolOutput::error(
                "askuserquestion is not available in this context",
            ));
        };
        if let Err(e) = validate(&input) {
            return Ok(ToolOutput::error(format!("askuserquestion: {e}")));
        }
        let resp = interaction
            .request(
                &ctx.session_pk,
                &ctx.tool_call_id,
                "askuserquestion",
                "answer the agent's question",
                ApprovalKind::Question,
                input.clone(),
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
            return Ok(ToolOutput::ok("The user submitted no answers."));
        }
        Ok(ToolOutput::ok(lines.join("\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_with_interaction;
    use super::*;
    use crate::domain::{ApprovalDecision, ApprovalKind, ApprovalResponse, CoreEvent, PermMode};
    use serde_json::json;

    fn valid_input() -> serde_json::Value {
        json!({"questions": [{
            "question": "Which DB?",
            "header": "Database",
            "options": [{"label": "SQLite"}, {"label": "Postgres", "description": "needs a server"}],
            "multiSelect": false
        }]})
    }

    #[tokio::test]
    async fn invalid_input_is_a_tool_error_without_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, _rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        for bad in [
            json!({}),
            json!({"questions": []}),
            json!({"questions": [{"question": "q", "header": "h", "options": [{"label": "only-one"}]}]}),
        ] {
            let out = AskUserQuestion.execute(&ctx, bad).await.unwrap();
            assert!(out.is_error);
        }
        assert!(!hub.has_pending());
    }

    #[tokio::test]
    async fn answers_round_trip_to_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        let waiter = tokio::spawn(async move {
            match rx.recv().await.unwrap() {
                CoreEvent::ApprovalRequested {
                    run_id,
                    request_id,
                    approval_kind,
                    ..
                } => {
                    assert_eq!(approval_kind, ApprovalKind::Question);
                    hub.resolve(
                        &crate::approval::ApprovalKey::new(run_id, request_id),
                        ApprovalResponse {
                            decision: ApprovalDecision::AllowOnce,
                            scope: None,
                            payload: Some(json!({"answers": {"Which DB?": ["SQLite"]}})),
                        },
                    );
                }
                other => panic!("unexpected {other:?}"),
            }
        });
        let out = AskUserQuestion.execute(&ctx, valid_input()).await.unwrap();
        waiter.await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("Which DB?"));
        assert!(out.for_model.contains("SQLite"));
    }

    #[tokio::test]
    async fn decline_is_reported_not_errored() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            }) = rx.recv().await
            {
                hub.resolve_bool(
                    &crate::approval::ApprovalKey::new(run_id, request_id),
                    false,
                );
            }
        });
        let out = AskUserQuestion.execute(&ctx, valid_input()).await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("declined"));
    }

    #[tokio::test]
    async fn stays_pending_until_the_user_answers() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
        let mut asked =
            tokio::spawn(async move { AskUserQuestion.execute(&ctx, valid_input()).await.unwrap() });

        let (run_id, request_id) = match rx.recv().await.unwrap() {
            CoreEvent::ApprovalRequested {
                run_id, request_id, ..
            } => (run_id, request_id),
            other => panic!("unexpected event {other:?}"),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut asked)
                .await
                .is_err(),
            "askuserquestion must remain pending until the user responds"
        );

        hub.resolve(
            &crate::approval::ApprovalKey::new(&run_id, &request_id),
            ApprovalResponse {
                decision: ApprovalDecision::AllowOnce,
                scope: None,
                payload: Some(json!({"answers": {"Which DB?": ["SQLite"]}})),
            },
        );
        let out = asked.await.unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("SQLite"));
    }

    #[tokio::test]
    async fn cancel_decision_is_reported_as_no_interactive_surface() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, hub, mut rx, _perm) = ctx_with_interaction(dir.path(), PermMode::Default).await;
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
        let out = AskUserQuestion.execute(&ctx, valid_input()).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(
            out.for_model,
            "No interactive surface answered this request."
        );
    }
}
