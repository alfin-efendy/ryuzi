//! `orch_block` — a worker raises a blocking question for a human (spec §8).
//! Flips its orchestration task `running → blocked`, posts a block card into
//! the home chat, and returns a sentinel so the worker's turn ends cleanly
//! (so the session goes idle). The human's answer arrives later over the
//! rail (`kind='unblock'`) as a new user turn — never a mid-turn splice.
//!
//! Gated to `kind='worker'` sessions two ways: schema-level (hidden from the
//! tool definitions advertised to every other session kind — see
//! `runner::visible_tool_defs`) and at runtime (this tool's own
//! `Store::task_by_session` lookup below — a non-worker session has no orch
//! task row for its `session_pk`, so it errors regardless of what was
//! advertised).

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::Value;

pub struct OrchBlock;

#[async_trait]
impl Tool for OrchBlock {
    fn name(&self) -> &str {
        "orch_block"
    }
    fn description(&self) -> &str {
        "Pause this orchestrated subtask and ask the human a blocking question. \
         Use only when you cannot proceed without human input. Your session resumes \
         automatically when the user answers."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "The question or blocker for the human."
                }
            },
            "required": ["reason"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("clarify", "ask the human a blocking question")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let reason = input
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();
        if reason.is_empty() {
            return Ok(ToolOutput::error(
                "orch_block requires a non-empty `reason`",
            ));
        }
        // Runtime guard (the real safety boundary — see the module doc): a
        // session with no matching orch task row is not an orchestrated
        // worker turn.
        let Some(task) = ctx.store.task_by_session(&ctx.session_pk).await? else {
            return Ok(ToolOutput::error(
                "orch_block is only available inside an orchestrated worker session",
            ));
        };
        // running → blocked (no-op if a cancel already won the race).
        let flipped = crate::orch::set_status(&ctx.store, &task.id, "running", "blocked").await?;
        if !flipped {
            return Ok(ToolOutput::error(
                "this task is no longer running; cannot block",
            ));
        }
        // Post the block card into the home chat (display row; `tick`'s
        // per-pass announcement emits the live OrchTaskChanged=blocked that
        // tells Cockpit to render it). Best-effort — a goal submitted
        // without a home chat (CLI/tests) has nowhere to post into, and a
        // failed post must never undo the block.
        if let Ok(Some(home)) = crate::orch::home_session(&ctx.store, &task).await {
            let payload = serde_json::json!({ "text": reason, "task_id": task.id });
            let _ = ctx
                .store
                .insert_message(crate::domain::NewMessage::speaker_block(
                    &home,
                    &task.agent,
                    "orch_block",
                    payload,
                ))
                .await;
        }
        Ok(ToolOutput::ok(
            "Blocked awaiting human input. Your session has paused and will resume \
             automatically with the user's answer. Do not call more tools now.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::tools::testutil::ctx_at;
    use serde_json::json;

    /// The runtime backstop (module doc): a `ToolCtx` with no matching
    /// `orch_tasks.session_pk` row — exactly what a non-worker (chat/project)
    /// session looks like — errors instead of blocking anything.
    #[tokio::test]
    async fn errors_outside_an_orchestrated_worker_session() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = OrchBlock
            .execute(&ctx, json!({"reason": "need a decision"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("orchestrated worker session"));
    }

    #[tokio::test]
    async fn rejects_an_empty_reason() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = OrchBlock
            .execute(&ctx, json!({"reason": "  "}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("non-empty"));
    }

    #[tokio::test]
    async fn blocks_a_running_task_and_posts_a_home_chat_card() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.session_pk = "worker-1".into();
        let root = crate::orch::insert_root(&ctx.store, "p1", "goal", "waiting", Some("home-1"))
            .await
            .unwrap();
        let ids = crate::orch::insert_children(
            &ctx.store,
            &root,
            "p1",
            &[crate::orch::PlannedTask {
                title: "a".into(),
                body: "do a".into(),
                agent: "build".into(),
                parents: vec![],
            }],
        )
        .await
        .unwrap();
        let child = ids[0].clone();
        crate::orch::set_status(&ctx.store, &child, "todo", "running")
            .await
            .unwrap();
        crate::orch::set_task_session(&ctx.store, &child, &ctx.session_pk)
            .await
            .unwrap();

        let out = OrchBlock
            .execute(&ctx, json!({"reason": "which port?"}))
            .await
            .unwrap();
        assert!(!out.is_error);

        let task = crate::orch::get_task(&ctx.store, &child)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "blocked");

        let rows = ctx.store.list_messages("home-1").await.unwrap();
        assert!(rows
            .iter()
            .any(|m| m.block_type == "orch_block" && m.speaker.as_deref() == Some("build")));
    }
}
