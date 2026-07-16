//! `task` — delegate subtasks to sub-agents.
//!
//! The runner supplies a [`super::SubagentSpawner`] in the `ToolCtx`; this tool
//! resolves the requested sub-agent(s), runs them to completion in isolated
//! (ephemeral-history) sub-loops, and returns their final reports as the tool
//! result. Two forms: a single `prompt` + `subagent_type`, or a `tasks` batch
//! executed concurrently (bounded by the `max_concurrent_runs` setting).
//! Sub-agents cannot spawn further sub-agents unless their agent definition
//! sets `delegate: true` and spawn depth remains available.

use super::{PermissionSpec, SubtaskSpec, SubtaskStatus, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Task;

/// Parse the batch form into specs. `None` when `tasks` is absent; a present
/// but non-array `tasks` is an explicit error (silently degrading to the
/// single form would mask a malformed batch).
fn parse_batch(input: &Value) -> Option<anyhow::Result<Vec<SubtaskSpec>>> {
    let raw = input.get("tasks")?;
    let Some(tasks) = raw.as_array() else {
        return Some(Err(anyhow::anyhow!(
            "task: `tasks` must be an array of {{subagent_type, prompt}} objects"
        )));
    };
    if tasks.is_empty() {
        return Some(Err(anyhow::anyhow!("task: `tasks` must not be empty")));
    }
    let specs = tasks
        .iter()
        .map(|t| {
            let prompt = t
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("task: every batch entry needs a `prompt`"))?;
            Ok(SubtaskSpec {
                agent_type: t
                    .get("subagent_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general")
                    .to_string(),
                prompt: prompt.to_string(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>();
    Some(specs)
}

#[async_trait]
impl Tool for Task {
    fn name(&self) -> &str {
        "task"
    }
    fn description(&self) -> &str {
        "Delegate self-contained subtasks to sub-agents. Single form: `prompt` \
         + `subagent_type` (e.g. `general` for multi-step work, `explore` for \
         read-only investigation). \
         Batch form: `tasks: [{subagent_type, prompt}]` runs independent \
         subtasks IN PARALLEL and returns every report — prefer it when \
         subtasks don't depend on each other. Sub-agents do not see this \
         conversation, so each prompt must be fully self-contained. Sub-agents \
         cannot use `task` or `memory` themselves unless their configuration \
         permits delegation. Choose exactly one form per call: never send a \
         top-level `prompt` together with `tasks`. Add \
         `background: true` (single form) to dispatch without blocking — the \
         result re-enters the chat on completion."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "description": {"type": "string", "description": "A short (3-5 word) label for the subtask."},
                        "prompt": {"type": "string", "description": "Single form: the full, self-contained task."},
                        "subagent_type": {"type": "string", "description": "Single form: which sub-agent to use."},
                        "background": {"type": "boolean", "description": "Single form only: run the subtask in the BACKGROUND — it does not block this turn; its result re-enters the chat when it finishes. Rejected (with a note) if too many background subagents are already running."}
                    },
                    "required": ["prompt"],
                    "not": {"required": ["tasks"]}
                },
                {
                    "properties": {
                        "description": {"type": "string", "description": "A short (3-5 word) label for the subtask."},
                        "tasks": {
                            "type": "array",
                            "description": "Batch form: independent subtasks run in parallel.",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "subagent_type": {"type": "string"},
                                    "prompt": {"type": "string"}
                                },
                                "required": ["prompt"]
                            }
                        }
                    },
                    "required": ["tasks"],
                    "not": {
                        "anyOf": [
                            {"required": ["prompt"]},
                            {"required": ["subagent_type"]}
                        ]
                    }
                }
            ]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let what = match input.get("tasks").and_then(|t| t.as_array()) {
            Some(batch) => format!("delegate {} parallel subtasks", batch.len()),
            None => {
                let ty = input
                    .get("subagent_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent");
                format!("delegate to {ty}")
            }
        };
        PermissionSpec::new("task", what)
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(spawner) = &ctx.spawn else {
            return Ok(ToolOutput::error(
                "task: this agent cannot spawn sub-agents (no spawn capability at this depth)",
            ));
        };
        let single = input.get("prompt").and_then(|v| v.as_str());
        match (single, parse_batch(&input)) {
            (Some(_), Some(_)) => Ok(ToolOutput::error(
                "task: pass either `prompt` (single) or `tasks` (batch), not both",
            )),
            (None, None) => Ok(ToolOutput::error(
                "task: `prompt` (single form) or `tasks` (batch form) is required",
            )),
            (Some(prompt), None) => {
                let ty = input
                    .get("subagent_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general");
                let background = input
                    .get("background")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if background {
                    use super::BackgroundDispatch;
                    return Ok(
                        match spawner
                            .run_background(
                                &ctx.tool_call_id,
                                super::SubtaskSpec {
                                    agent_type: ty.to_string(),
                                    prompt: prompt.to_string(),
                                },
                            )
                            .await
                        {
                            BackgroundDispatch::Dispatched { id } => ToolOutput::ok(format!(
                                "Background subagent `{ty}` dispatched (id {id}). Its result will \
                             re-enter this chat when it finishes — continue with other work."
                            )),
                            BackgroundDispatch::Rejected { note } => ToolOutput::ok(note),
                        },
                    );
                }
                match spawner.run(&ctx.tool_call_id, ty, prompt).await {
                    Ok(report) => Ok(ToolOutput::ok(report)),
                    Err(e) => Ok(ToolOutput::error(format!(
                        "task: sub-agent `{ty}` failed: {e} (available: {})",
                        spawner.available().join(", ")
                    ))),
                }
            }
            (None, Some(specs)) => {
                let specs = match specs {
                    Ok(s) => s,
                    Err(e) => return Ok(ToolOutput::error(e.to_string())),
                };
                let total = specs.len();
                let results = spawner.run_many(&ctx.tool_call_id, specs).await;
                let ok = results
                    .iter()
                    .filter(|r| r.status == SubtaskStatus::Completed)
                    .count();
                let digest = results
                    .iter()
                    .map(|r| {
                        format!(
                            "[{}] {} — {}\n{}",
                            r.index + 1,
                            r.agent_type,
                            r.status.as_str(),
                            r.report
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Ok(ToolOutput {
                    for_model: format!("{total} sub-agents finished ({ok} ok):\n\n{digest}"),
                    model_blocks: None,
                    display: Some(json!({
                        "summary": format!("{total} sub-agents: {ok} ok, {} failed", total - ok)
                    })),
                    is_error: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::super::{SubagentSpawner, SubtaskResult};
    use super::*;
    use std::sync::Arc;

    /// A scripted spawner: echoes prompts back, failing agent type `bad`, and
    /// records the dispatch identity the tool contract supplies.
    #[derive(Default)]
    struct RecordingSpawner {
        dispatches: std::sync::Mutex<Vec<(String, Vec<usize>)>>,
    }

    #[async_trait]
    impl SubagentSpawner for RecordingSpawner {
        async fn run_many(
            &self,
            source_tool_call_id: &str,
            specs: Vec<SubtaskSpec>,
        ) -> Vec<SubtaskResult> {
            self.dispatches
                .lock()
                .unwrap()
                .push((source_tool_call_id.to_string(), (0..specs.len()).collect()));
            specs
                .into_iter()
                .enumerate()
                .map(|(index, s)| {
                    let failed = s.agent_type == "bad";
                    SubtaskResult {
                        index,
                        agent_type: s.agent_type,
                        status: if failed {
                            SubtaskStatus::Error
                        } else {
                            SubtaskStatus::Completed
                        },
                        report: if failed {
                            "boom".into()
                        } else {
                            format!("echo: {}", s.prompt)
                        },
                    }
                })
                .collect()
        }
        fn available(&self) -> Vec<String> {
            vec!["general".into(), "explore".into()]
        }
        async fn run_background(
            &self,
            source_tool_call_id: &str,
            _spec: SubtaskSpec,
        ) -> super::super::BackgroundDispatch {
            self.dispatches
                .lock()
                .unwrap()
                .push((source_tool_call_id.to_string(), vec![0]));
            super::super::BackgroundDispatch::Dispatched {
                id: "bg-1".to_string(),
            }
        }
    }

    async fn ctx_with_spawner(dir: &std::path::Path) -> (ToolCtx, Arc<RecordingSpawner>) {
        let mut ctx = ctx_at(dir).await;
        ctx.tool_call_id = "test-tool-call".into();
        let spawner = Arc::new(RecordingSpawner::default());
        ctx.spawn = Some(spawner.clone());
        (ctx, spawner)
    }

    #[tokio::test]
    async fn batch_digest_orders_reports_and_summarizes() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"tasks": [
                    {"subagent_type": "general", "prompt": "first job"},
                    {"subagent_type": "bad", "prompt": "second job"},
                    {"prompt": "third job"}
                ]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("3 sub-agents finished (2 ok)"));
        let first = out.for_model.find("echo: first job").unwrap();
        let second = out.for_model.find("boom").unwrap();
        let third = out.for_model.find("echo: third job").unwrap();
        assert!(first < second && second < third, "index order");
        assert!(out.for_model.contains("[2] bad — error"));
        assert_eq!(
            out.display.unwrap()["summary"],
            "3 sub-agents: 2 ok, 1 failed"
        );
    }

    #[tokio::test]
    async fn both_forms_at_once_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(&ctx, json!({"prompt": "x", "tasks": [{"prompt": "y"}]}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("not both"));
    }

    #[tokio::test]
    async fn empty_batch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _) = ctx_with_spawner(dir.path()).await;
        let out = Task.execute(&ctx, json!({"tasks": []})).await.unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("must not be empty"));
    }

    #[tokio::test]
    async fn dispatch_link_single_form_uses_tool_call_id_and_zero_index() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"subagent_type": "explore", "prompt": "find it"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "echo: find it");
        assert_eq!(
            *spawner.dispatches.lock().unwrap(),
            vec![("test-tool-call".to_string(), vec![0])]
        );
    }

    #[tokio::test]
    async fn dispatch_link_batch_uses_tool_call_id_and_input_indices() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"tasks": [
                    {"prompt": "first"},
                    {"prompt": "second"},
                    {"prompt": "third"}
                ]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            *spawner.dispatches.lock().unwrap(),
            vec![("test-tool-call".to_string(), vec![0, 1, 2])]
        );
    }

    #[tokio::test]
    async fn background_single_form_dispatches_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"subagent_type": "general", "prompt": "long job", "background": true}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("dispatched"), "{}", out.for_model);
        // Not the synchronous echo — the turn was not blocked on the child.
        assert!(!out.for_model.contains("echo: long job"));
        assert_eq!(
            *spawner.dispatches.lock().unwrap(),
            vec![("test-tool-call".to_string(), vec![0])]
        );
    }

    #[tokio::test]
    async fn batch_form_ignores_background_flag() {
        // Batch form has no `background` handling — an errant `background`
        // alongside `tasks` is silently ignored, not an error, and does NOT
        // dispatch any background workers (EchoSpawner's `run_background`
        // would return "bg-1"; its absence here proves `run_many` — the
        // synchronous batch path — is what actually ran).
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _) = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"background": true, "tasks": [{"subagent_type": "general", "prompt": "job"}]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("echo: job"), "{}", out.for_model);
        assert!(!out.for_model.contains("dispatched"), "{}", out.for_model);
    }

    #[tokio::test]
    async fn no_spawner_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await; // spawn: None
        let out = Task
            .execute(&ctx, json!({"prompt": "x", "subagent_type": "general"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("cannot spawn"));
    }

    #[test]
    fn input_schema_requires_exactly_one_task_form() {
        let schema = Task.input_schema();
        let alts = schema["oneOf"].as_array().expect("oneOf array");
        assert_eq!(alts.len(), 2);

        let single = alts
            .iter()
            .find(|a| a["required"] == json!(["prompt"]))
            .expect("single-form alternative with required == [\"prompt\"]");
        assert_eq!(single["not"]["required"], json!(["tasks"]));

        let batch = alts
            .iter()
            .find(|a| a["required"] == json!(["tasks"]))
            .expect("batch-form alternative with required == [\"tasks\"]");
        let not_any_of = batch["not"]["anyOf"].as_array().expect("not.anyOf array");
        assert!(not_any_of.contains(&json!({"required": ["prompt"]})));
        assert!(not_any_of.contains(&json!({"required": ["subagent_type"]})));
        assert_eq!(batch["properties"]["tasks"]["minItems"], json!(1));
        assert_eq!(
            batch["properties"]["tasks"]["items"]["required"],
            json!(["prompt"])
        );
    }
}
