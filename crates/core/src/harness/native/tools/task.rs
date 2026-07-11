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
         read-only investigation, `orchestrator` to coordinate a wide goal). \
         Batch form: `tasks: [{subagent_type, prompt}]` runs independent \
         subtasks IN PARALLEL and returns every report — prefer it when \
         subtasks don't depend on each other. Sub-agents do not see this \
         conversation, so each prompt must be fully self-contained. Sub-agents \
         cannot use `task` or `memory` themselves (unless the target agent is \
         a delegator like `orchestrator`). Choose exactly one form per call: \
         never send a top-level `prompt` together with `tasks`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "description": {"type": "string", "description": "A short (3-5 word) label for the subtask."},
                        "prompt": {"type": "string", "description": "Single form: the full, self-contained task."},
                        "subagent_type": {"type": "string", "description": "Single form: which sub-agent to use."}
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
                match spawner.run(ty, prompt).await {
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
                let results = spawner.run_many(specs).await;
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

    /// A scripted spawner: echoes prompts back, failing agent type `bad`.
    struct EchoSpawner;

    #[async_trait]
    impl SubagentSpawner for EchoSpawner {
        async fn run_many(&self, specs: Vec<SubtaskSpec>) -> Vec<SubtaskResult> {
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
    }

    async fn ctx_with_spawner(dir: &std::path::Path) -> ToolCtx {
        let mut ctx = ctx_at(dir).await;
        ctx.spawn = Some(Arc::new(EchoSpawner));
        ctx
    }

    #[tokio::test]
    async fn batch_digest_orders_reports_and_summarizes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_spawner(dir.path()).await;
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
        let ctx = ctx_with_spawner(dir.path()).await;
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
        let ctx = ctx_with_spawner(dir.path()).await;
        let out = Task.execute(&ctx, json!({"tasks": []})).await.unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("must not be empty"));
    }

    #[tokio::test]
    async fn single_form_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_spawner(dir.path()).await;
        let out = Task
            .execute(
                &ctx,
                json!({"subagent_type": "explore", "prompt": "find it"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "echo: find it");
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
