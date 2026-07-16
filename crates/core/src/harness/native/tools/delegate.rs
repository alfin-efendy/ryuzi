//! `delegate_agent` — delegate work to complete autonomous agent profiles.

use super::{PermissionSpec, SubtaskStatus, Tool, ToolCtx, ToolOutput};
use crate::delegation::{AgentDispatchLink, MainDelegationRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;

pub struct DelegateAgent;

#[derive(Debug)]
struct DelegationSpec {
    agent_id: String,
    task: String,
    context: Option<String>,
}

fn parse_spec(value: &Value, batch_item: bool) -> anyhow::Result<DelegationSpec> {
    let agent_id = value
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("delegate_agent: every delegation needs an `agent_id`"))?;
    let task = value
        .get("task")
        .and_then(Value::as_str)
        .filter(|task| !task.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("delegate_agent: every delegation needs a `task`"))?;
    if batch_item && value.get("background").is_some() {
        anyhow::bail!(
            "delegate_agent: `background` belongs at the top level, not inside a batch item"
        );
    }
    Ok(DelegationSpec {
        agent_id: agent_id.to_string(),
        task: task.to_string(),
        context: value
            .get("context")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

fn request(
    parent_run_id: &str,
    source_tool_call_id: &str,
    dispatch_index: usize,
    spec: DelegationSpec,
    background: bool,
) -> MainDelegationRequest {
    MainDelegationRequest {
        parent_run_id: parent_run_id.to_string(),
        target_agent_id: spec.agent_id,
        task: spec.task,
        context: spec.context,
        background,
        dispatch: Some(AgentDispatchLink {
            source_tool_call_id: source_tool_call_id.to_string(),
            dispatch_index: i64::try_from(dispatch_index).expect("delegation index fits i64"),
        }),
    }
}

#[async_trait]
impl Tool for DelegateAgent {
    fn name(&self) -> &str {
        "delegate_agent"
    }

    fn description(&self) -> &str {
        "Delegate a self-contained task to another complete agent profile. Use the available profile IDs from the agent catalog in the system prompt. Single form: \
         `{agent_id, task, context?, background?}`. Batch form: \
         `{delegations: [{agent_id, task, context?}], background?}`. Foreground \
         work waits for reports; `background: true` returns durable run IDs."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "agent_id": {"type": "string"},
                        "task": {"type": "string"},
                        "context": {"type": "string"},
                        "background": {"type": "boolean"}
                    },
                    "required": ["agent_id", "task"],
                    "not": {"required": ["delegations"]}
                },
                {
                    "properties": {
                        "delegations": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "agent_id": {"type": "string"},
                                    "task": {"type": "string"},
                                    "context": {"type": "string"}
                                },
                                "required": ["agent_id", "task"],
                                "not": {"required": ["background"]}
                            }
                        },
                        "background": {"type": "boolean"}
                    },
                    "required": ["delegations"],
                    "not": {"anyOf": [{"required": ["agent_id"]}, {"required": ["task"]}, {"required": ["context"]}]}
                }
            ]
        })
    }

    fn kind(&self) -> &'static str {
        "other"
    }

    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("read", "Delegate work to another agent")
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(spawner) = &ctx.main_agent_spawn else {
            return Ok(ToolOutput::error(
                "delegate_agent: this agent cannot delegate to main agent profiles",
            ));
        };
        let available = spawner.available().await;
        if available.is_empty() {
            return Ok(ToolOutput::error(
                "delegate_agent: no other executable main agent profiles are available",
            ));
        }
        let available_ids: HashSet<_> = available.iter().map(|(id, _, _)| id.as_str()).collect();
        let has_single = input.get("agent_id").is_some() || input.get("task").is_some();
        let has_batch = input.get("delegations").is_some();
        if has_single && has_batch {
            return Ok(ToolOutput::error(
                "delegate_agent: pass either a single delegation or `delegations`, not both",
            ));
        }
        if !has_single && !has_batch {
            return Ok(ToolOutput::error(
                "delegate_agent: an `agent_id`/`task` pair or `delegations` batch is required",
            ));
        }
        let background = input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if has_single {
            let spec = match parse_spec(&input, false) {
                Ok(spec) => spec,
                Err(error) => return Ok(ToolOutput::error(error.to_string())),
            };
            if !available_ids.contains(spec.agent_id.as_str()) {
                return Ok(ToolOutput::error(format!(
                    "delegate_agent: unknown or unavailable agent `{}`",
                    spec.agent_id
                )));
            }
            let result = spawner
                .run_one(request(&ctx.run_id, &ctx.tool_call_id, 0, spec, background))
                .await;
            return Ok(match result.status {
                super::SubtaskStatus::Completed => {
                    if background {
                        ToolOutput::ok(format!(
                            "Background delegation dispatched (run {}). Its result will re-enter this chat when it finishes.",
                            result.run_id
                        ))
                    } else {
                        ToolOutput::ok(result.report)
                    }
                }
                status => ToolOutput::error(format!(
                    "delegate_agent: `{}` {}: {}",
                    result.agent_id,
                    status.as_str(),
                    result.report
                )),
            });
        }

        let Some(entries) = input.get("delegations").and_then(Value::as_array) else {
            return Ok(ToolOutput::error(
                "delegate_agent: `delegations` must be a non-empty array",
            ));
        };
        if entries.is_empty() {
            return Ok(ToolOutput::error(
                "delegate_agent: `delegations` must not be empty",
            ));
        }
        let mut ids = HashSet::new();
        let mut requests = Vec::with_capacity(entries.len());
        for (dispatch_index, entry) in entries.iter().enumerate() {
            let spec = match parse_spec(entry, true) {
                Ok(spec) => spec,
                Err(error) => return Ok(ToolOutput::error(error.to_string())),
            };
            if !available_ids.contains(spec.agent_id.as_str()) {
                return Ok(ToolOutput::error(format!(
                    "delegate_agent: unknown or unavailable agent `{}`",
                    spec.agent_id
                )));
            }
            if !ids.insert(spec.agent_id.clone()) {
                return Ok(ToolOutput::error(format!(
                    "delegate_agent: duplicate agent_id `{}` in one batch",
                    spec.agent_id
                )));
            }
            requests.push(request(
                &ctx.run_id,
                &ctx.tool_call_id,
                dispatch_index,
                spec,
                background,
            ));
        }
        let results = spawner.run_many(requests).await;
        // The transcript card resolver intentionally prefers a linked child
        // over the aggregate tool output. Preserve terminal errors for batch
        // slots that were never admitted, keyed by their durable dispatch
        // index, so the UI can render those errors beside admitted children.
        let dispatch_failures = results
            .iter()
            .enumerate()
            .filter_map(|(dispatch_index, result)| {
                (result.status != SubtaskStatus::Completed).then(|| {
                    json!({
                        "dispatch_index": dispatch_index,
                        "error": result.report,
                    })
                })
            })
            .collect::<Vec<_>>();
        let text = results
            .into_iter()
            .map(|result| {
                if background && result.status == SubtaskStatus::Completed {
                    format!("{}: dispatched (run {})", result.agent_id, result.run_id)
                } else {
                    format!(
                        "{} — {}\n{}",
                        result.agent_id,
                        result.status.as_str(),
                        result.report
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut output = ToolOutput::ok(text);
        if !dispatch_failures.is_empty() {
            output.display = Some(json!({ "dispatch_failures": dispatch_failures }));
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::super::{MainAgentSpawner, MainDelegationResult, Tool};
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSpawner {
        requests: Mutex<Vec<MainDelegationRequest>>,
    }

    struct PartiallyRejectingSpawner;

    #[async_trait]
    impl MainAgentSpawner for PartiallyRejectingSpawner {
        async fn available(&self) -> Vec<(String, String, String)> {
            vec![
                (
                    "reviewer".into(),
                    "Reviewer".into(),
                    "Audits changes".into(),
                ),
                ("tester".into(), "Tester".into(), "Runs tests".into()),
            ]
        }

        async fn run_one(&self, _request: MainDelegationRequest) -> MainDelegationResult {
            unreachable!("this regression exercises batch dispatches")
        }

        async fn run_many(
            &self,
            _requests: Vec<MainDelegationRequest>,
        ) -> Vec<MainDelegationResult> {
            vec![
                MainDelegationResult::completed("run-1", "reviewer", "background delegation dispatched"),
                MainDelegationResult {
                    run_id: String::new(),
                    agent_id: "tester".into(),
                    status: super::super::SubtaskStatus::Error,
                    report: "Async delegation capacity reached (1 running). Run this task synchronously.".into(),
                },
            ]
        }
    }

    #[async_trait]
    impl MainAgentSpawner for RecordingSpawner {
        async fn available(&self) -> Vec<(String, String, String)> {
            vec![
                (
                    "reviewer".into(),
                    "Reviewer".into(),
                    "Audits changes".into(),
                ),
                ("tester".into(), "Tester".into(), "Runs tests".into()),
            ]
        }

        async fn run_one(&self, request: MainDelegationRequest) -> MainDelegationResult {
            self.requests.lock().unwrap().push(request);
            MainDelegationResult::completed("run-1", "reviewer", "audit complete")
        }

        async fn run_many(
            &self,
            requests: Vec<MainDelegationRequest>,
        ) -> Vec<MainDelegationResult> {
            self.requests.lock().unwrap().extend(requests);
            vec![
                MainDelegationResult::completed("run-1", "reviewer", "audit complete"),
                MainDelegationResult::completed("run-2", "tester", "tests complete"),
            ]
        }
    }

    async fn ctx_with_spawner(
        dir: &std::path::Path,
    ) -> (super::super::ToolCtx, Arc<RecordingSpawner>) {
        let mut ctx = ctx_at(dir).await;
        let spawner = Arc::new(RecordingSpawner::default());
        ctx.main_agent_spawn = Some(spawner.clone());
        (ctx, spawner)
    }

    #[tokio::test]
    async fn foreground_single_waits_for_the_selected_agent_result() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = DelegateAgent.execute(&ctx, json!({"agent_id":"reviewer","task":"audit","context":"focus auth","background":false})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(out.for_model, "audit complete");
        let requests = spawner.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].parent_run_id, "test-run");
        assert_eq!(requests[0].target_agent_id, "reviewer");
        assert_eq!(requests[0].task, "audit");
        assert_eq!(requests[0].context.as_deref(), Some("focus auth"));
        assert!(!requests[0].background);
        assert_eq!(
            requests[0].dispatch,
            Some(crate::delegation::AgentDispatchLink {
                source_tool_call_id: "test-call".into(),
                dispatch_index: 0,
            })
        );
    }

    #[tokio::test]
    async fn batch_waits_for_every_agent_and_returns_each_result() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = DelegateAgent.execute(&ctx, json!({"delegations":[{"agent_id":"reviewer","task":"audit","context":"optional"},{"agent_id":"tester","task":"test"}],"background":false})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("audit complete"));
        assert!(out.for_model.contains("tests complete"));
        let requests = spawner.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].target_agent_id, "reviewer");
        assert_eq!(requests[0].task, "audit");
        assert_eq!(requests[0].context.as_deref(), Some("optional"));
        assert_eq!(requests[1].target_agent_id, "tester");
        assert_eq!(requests[1].task, "test");
        assert_eq!(requests[1].context, None);
        assert_eq!(
            requests
                .iter()
                .map(|request| request
                    .dispatch
                    .as_ref()
                    .map(|link| { (link.source_tool_call_id.as_str(), link.dispatch_index) }))
                .collect::<Vec<_>>(),
            vec![Some(("test-call", 0)), Some(("test-call", 1))]
        );
    }

    #[tokio::test]
    async fn background_batch_exposes_each_unadmitted_dispatch_error_by_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.main_agent_spawn = Some(Arc::new(PartiallyRejectingSpawner));

        let out = DelegateAgent
            .execute(
                &ctx,
                json!({
                    "delegations": [
                        {"agent_id":"reviewer","task":"audit"},
                        {"agent_id":"tester","task":"test"}
                    ],
                    "background": true
                }),
            )
            .await
            .unwrap();

        assert!(
            !out.is_error,
            "a partial batch keeps the admitted dispatch active"
        );
        assert_eq!(
            out.display,
            Some(json!({
                "dispatch_failures": [{
                    "dispatch_index": 1,
                    "error": "Async delegation capacity reached (1 running). Run this task synchronously."
                }]
            }))
        );
    }

    #[tokio::test]
    async fn background_single_returns_its_durable_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, spawner) = ctx_with_spawner(dir.path()).await;
        let out = DelegateAgent
            .execute(
                &ctx,
                json!({"agent_id":"reviewer","task":"audit","background":true}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("run-1"));
        let requests = spawner.requests.lock().unwrap();
        assert!(requests[0].background);
        assert_eq!(
            requests[0].dispatch,
            Some(crate::delegation::AgentDispatchLink {
                source_tool_call_id: "test-call".into(),
                dispatch_index: 0,
            })
        );
    }

    #[tokio::test]
    async fn rejects_mixed_empty_duplicate_and_item_background_forms() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _) = ctx_with_spawner(dir.path()).await;
        for input in [
            json!({"agent_id":"reviewer","task":"audit","delegations":[]}),
            json!({"delegations":[]}),
            json!({"delegations":[{"agent_id":"reviewer","task":"a"},{"agent_id":"reviewer","task":"b"}]}),
            json!({"delegations":[{"agent_id":"reviewer","task":"a","background":true}]}),
        ] {
            let out = DelegateAgent.execute(&ctx, input).await.unwrap();
            assert!(out.is_error, "{}", out.for_model);
        }
    }

    #[test]
    fn schema_exposes_only_the_single_and_batch_forms() {
        let schema = DelegateAgent.input_schema();
        let forms = schema["oneOf"].as_array().expect("oneOf array");
        assert_eq!(forms.len(), 2);
        let batch = forms
            .iter()
            .find(|form| form["required"] == json!(["delegations"]))
            .expect("batch form");
        assert_eq!(batch["properties"]["delegations"]["minItems"], json!(1));
        assert_eq!(
            batch["properties"]["delegations"]["items"]["not"]["required"],
            json!(["background"])
        );
    }
}
