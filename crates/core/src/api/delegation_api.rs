//! Child-run RPC boundary: scoped reads and lifecycle controls for Cockpit.

use super::{ok, params, ApiError};
use crate::domain::{AgentRun, AgentRunRosterInfo};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "get_child_runs",
    "get_child_transcript",
    "cancel_child_run",
    "retry_child_run",
];

#[derive(Deserialize)]
struct SessionP {
    session_pk: String,
}

#[derive(Deserialize)]
struct RunP {
    session_pk: String,
    run_id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        "get_child_runs" => {
            let a: SessionP = params(p)?;
            require_session(state, &a.session_pk).await?;
            let mut runs = state
                .cp
                .store()
                .list_session_agent_runs(&a.session_pk)
                .await?;
            let root_run_id = runs
                .iter()
                .find(|run| {
                    run.parent_run_id.is_none()
                        && run.agent_kind == crate::domain::AgentRunKind::Primary
                })
                .map(|run| run.run_id.clone());
            runs.retain(|run| run.parent_run_id.is_some());
            runs.sort_by(|left, right| {
                let rank = |run: &AgentRun| match run.status {
                    crate::domain::AgentRunStatus::Queued => 0,
                    crate::domain::AgentRunStatus::Running => 1,
                    _ => 2,
                };
                rank(left).cmp(&rank(right)).then_with(|| {
                    right
                        .finished_at
                        .unwrap_or(i64::MIN)
                        .cmp(&left.finished_at.unwrap_or(i64::MIN))
                })
            });
            for run in &mut runs {
                if let Ok(Some(models)) = state
                    .cp
                    .store()
                    .get_agent_run_cost_models(&run.run_id)
                    .await
                {
                    let tally = crate::harness::native::cost::Tally::from_payload(
                        &serde_json::json!({ "models": models }),
                    );
                    if !tally.is_empty() {
                        let (total_usd, models) =
                            crate::harness::native::cost::price_tally(state.cp.store(), &tally)
                                .await;
                        run.cost = Some(crate::domain::AgentRunCostBreakdown { total_usd, models });
                    }
                }
            }
            ok(AgentRunRosterInfo { root_run_id, runs })
        }
        "get_child_transcript" => {
            let a: RunP = params(p)?;
            require_child_run(state, &a.session_pk, &a.run_id).await?;
            ok(state
                .cp
                .store()
                .list_run_messages(&a.session_pk, &a.run_id)
                .await?)
        }
        "cancel_child_run" => {
            let a: RunP = params(p)?;
            // Historical (legacy / deleted-owner) sessions are read-only:
            // reject before touching the child run.
            crate::sessions::ownership::require_executable_session_agent(
                state.cp.store(),
                &state.agents,
                &a.session_pk,
            )
            .await?;
            require_child_run(state, &a.session_pk, &a.run_id).await?;
            state
                .cp
                .delegation()
                .cancel_child(&a.session_pk, &a.run_id)
                .await
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
            ok(())
        }
        "retry_child_run" => {
            let a: RunP = params(p)?;
            // Historical (legacy / deleted-owner) sessions are read-only:
            // reject before any run lookup or retry dispatch.
            crate::sessions::ownership::require_executable_session_agent(
                state.cp.store(),
                &state.agents,
                &a.session_pk,
            )
            .await?;
            let previous = require_child_run(state, &a.session_pk, &a.run_id).await?;
            if let Some(agent_id) = previous.executing_agent_id.as_deref() {
                let snapshot = state
                    .agents
                    .resolved_snapshot(agent_id)
                    .await
                    .map_err(|error| ApiError::bad_request(error.to_string()))?;
                if !snapshot.executable {
                    return Err(ApiError::bad_request(format!(
                        "agent `{agent_id}` is not executable"
                    )));
                }
            }
            let run = state
                .cp
                .dispatch_child_retry(&a.session_pk, &a.run_id)
                .await
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
            ok(run)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn require_session(state: &ApiState, session_pk: &str) -> Result<(), ApiError> {
    state
        .cp
        .store()
        .get_session(session_pk)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown session: {session_pk}")))?;
    Ok(())
}

async fn require_child_run(
    state: &ApiState,
    session_pk: &str,
    run_id: &str,
) -> Result<AgentRun, ApiError> {
    require_session(state, session_pk).await?;
    let run = state
        .cp
        .store()
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.session_pk == session_pk)
        .ok_or_else(|| ApiError::not_found(format!("unknown agent run: {run_id}")))?;
    if run.parent_run_id.is_none() {
        return Err(ApiError::bad_request(
            "primary runs cannot be controlled as children",
        ));
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support};
    use crate::delegation::{MainDelegationRequest, SubagentRunRequest};
    use crate::domain::{
        AgentRunKind, AgentRunStatus, NewAgentRun, NewMessage, PermMode, Session, SessionKind,
        SessionStatus,
    };
    use crate::harness::native::llm::{LlmStream, LlmStreamFactory};
    use crate::harness::native::runner::testutil::{
        message_delta, message_stop, text_delta, ScriptedLlm,
    };
    use crate::serve::ApiState;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    fn session(session_pk: &str) -> Session {
        // Child-run controls require an executable primary agent; every
        // fixture registry here bootstraps the built-in `ryuzi` owner.
        Session {
            session_pk: session_pk.into(),
            primary_agent_id: Some("ryuzi".into()),
            primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                id: "ryuzi".into(),
                name: "Ryuzi".into(),
                avatar_color: "violet".into(),
            }),
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status: SessionStatus::Idle,
            perm_mode: PermMode::Default,
            started_by: None,
            created_at: None,
            last_active: None,
            resume_attempts: 0,
            branch_owned: false,
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
            archived_at: None,
        }
    }

    async fn primary(s: &ApiState, session_pk: &str) -> crate::delegation::RunHandle {
        s.cp.store()
            .insert_session(session(session_pk))
            .await
            .unwrap();
        let agent_id = s.agents.default_agent_id().await;
        let snapshot = s.agents.resolved_snapshot(&agent_id).await.unwrap();
        s.cp.delegation()
            .begin_primary(session_pk, snapshot, "root")
            .await
            .unwrap()
    }

    async fn subagent(
        s: &ApiState,
        parent_run_id: &str,
        task: &str,
    ) -> crate::delegation::RunHandle {
        s.cp.delegation()
            .queue_subagent(SubagentRunRequest {
                parent_run_id: parent_run_id.into(),
                subagent_type: task.into(),
                task: task.into(),
                context: None,
                background: false,
                dispatch: None,
            })
            .await
            .unwrap()
    }

    struct FixedLlmFactory(Arc<dyn LlmStream>);

    impl LlmStreamFactory for FixedLlmFactory {
        fn create(&self, _store: Arc<crate::store::Store>) -> Arc<dyn LlmStream> {
            self.0.clone()
        }
    }

    async fn retry_terminal(s: &ApiState, run_id: &str) -> crate::domain::AgentRun {
        for _ in 0..400 {
            let run = s.cp.store().get_agent_run(run_id).await.unwrap().unwrap();
            if run.status.is_terminal() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for retry {run_id} to reach a terminal status");
    }

    #[tokio::test]
    async fn child_runs_omit_primary_and_order_active_before_latest_terminals() {
        let s = tests_support::state_with_agents().await;
        let root = primary(&s, "s").await;
        let completed_first = subagent(&s, &root.run.run_id, "completed-first").await;
        s.cp.delegation()
            .complete(&completed_first.run.run_id, "done")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let completed_latest = subagent(&s, &root.run.run_id, "completed-latest").await;
        s.cp.delegation()
            .complete(&completed_latest.run.run_id, "done")
            .await
            .unwrap();
        let running = subagent(&s, &root.run.run_id, "running").await;
        s.cp.delegation()
            .mark_running(&running.run.run_id)
            .await
            .unwrap();
        let queued = subagent(&s, &root.run.run_id, "queued").await;

        let roster = dispatch(&s, "get_child_runs", json!({ "session_pk": "s" }))
            .await
            .unwrap();
        assert_eq!(roster["rootRunId"], root.run.run_id);
        let run_ids = roster["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["runId"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            run_ids,
            vec![
                queued.run.run_id.as_str(),
                running.run.run_id.as_str(),
                completed_latest.run.run_id.as_str(),
                completed_first.run.run_id.as_str(),
            ]
        );
        assert!(!run_ids.contains(&root.run.run_id.as_str()));
    }

    #[tokio::test]
    async fn child_runs_return_null_root_for_legacy_sessions() {
        let s = tests_support::state_with_agents().await;
        s.cp.store()
            .insert_session(session("legacy"))
            .await
            .unwrap();

        let roster = dispatch(&s, "get_child_runs", json!({ "session_pk": "legacy" }))
            .await
            .unwrap();

        assert!(roster["rootRunId"].is_null());
        assert_eq!(roster["runs"], json!([]));
    }

    #[tokio::test]
    async fn child_run_reads_require_matching_session_and_run_scope() {
        let s = tests_support::state_with_agents().await;
        let root = primary(&s, "s-one").await;
        s.cp.store().insert_session(session("s-two")).await.unwrap();
        let child = subagent(&s, &root.run.run_id, "child").await;
        s.cp.store()
            .insert_run_message(
                &child.run.run_id,
                NewMessage::block(
                    "s-one",
                    "assistant",
                    "text",
                    json!({ "text": "child only" }),
                ),
            )
            .await
            .unwrap();
        s.cp.store()
            .insert_message(NewMessage::block(
                "s-one",
                "assistant",
                "text",
                json!({ "text": "root only" }),
            ))
            .await
            .unwrap();

        let transcript = dispatch(
            &s,
            "get_child_transcript",
            json!({ "session_pk": "s-one", "run_id": child.run.run_id }),
        )
        .await
        .unwrap();
        assert_eq!(transcript.as_array().unwrap().len(), 1);
        assert_eq!(transcript[0]["payload"]["text"], "child only");

        for method in [
            "get_child_transcript",
            "cancel_child_run",
            "retry_child_run",
        ] {
            let error = dispatch(
                &s,
                method,
                json!({ "session_pk": "s-two", "run_id": child.run.run_id }),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 404, "{method}");
        }
    }

    #[tokio::test]
    async fn retry_child_runs_dispatches_main_delegate_and_subagent_executors() {
        let scripted: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![
            vec![
                text_delta("main retry complete"),
                message_delta("end_turn"),
                message_stop(),
            ],
            vec![
                text_delta("subagent retry complete"),
                message_delta("end_turn"),
                message_stop(),
            ],
        ]));
        let s = tests_support::state_with_native_llm(Arc::new(FixedLlmFactory(scripted))).await;
        let root = primary(&s, "s").await;
        let agent_id = s
            .agents
            .create(crate::agents::types::AgentMutationInput {
                name: "Retry target".into(),
                description: "retry target".into(),
                avatar: crate::agents::types::AgentAvatar {
                    color: "blue".into(),
                },
                model: crate::agents::types::AgentModel::Route {
                    route: "free".into(),
                },
                personality: crate::agents::personality::AgentPersonality::default_profile(),
                permissions: crate::agents::types::AgentPermissions {
                    mode: PermMode::Default,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: crate::agents::types::AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: crate::agents::types::AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap()
            .profile
            .id;
        let main =
            s.cp.delegation()
                .queue_main(MainDelegationRequest {
                    parent_run_id: root.run.run_id.clone(),
                    target_agent_id: agent_id,
                    task: "retry main delegate".into(),
                    context: None,
                    background: false,
                    dispatch: None,
                })
                .await
                .unwrap();
        let sub = subagent(&s, &root.run.run_id, "general").await;
        for run in [&main.run, &sub.run] {
            s.cp.delegation().fail(&run.run_id, "failed").await.unwrap();
        }

        let main_retry = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": main.run.run_id }),
        )
        .await
        .unwrap();
        let sub_retry = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": sub.run.run_id }),
        )
        .await
        .unwrap();

        let main_terminal = retry_terminal(&s, main_retry["runId"].as_str().unwrap()).await;
        let sub_terminal = retry_terminal(&s, sub_retry["runId"].as_str().unwrap()).await;
        assert_eq!(main_terminal.status, AgentRunStatus::Completed);
        assert_eq!(main_terminal.result.as_deref(), Some("main retry complete"));
        assert_eq!(sub_terminal.status, AgentRunStatus::Completed);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn retry_subagent_terminalizes_when_the_active_harness_cannot_dispatch_it() {
        let s = tests_support::state_with_fake_native().await;
        let root = primary(&s, "s").await;
        let child = subagent(&s, &root.run.run_id, "general").await;
        s.cp.delegation()
            .fail(&child.run.run_id, "failed")
            .await
            .unwrap();

        let error = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": child.run.run_id }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);

        let retried =
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .into_iter()
                .find(|run| run.retry_of.as_deref() == Some(child.run.run_id.as_str()))
                .expect("admitted retry must be retained for its terminal error");
        assert_eq!(retried.status, AgentRunStatus::Failed);
        assert!(retried
            .error
            .as_deref()
            .is_some_and(|error| error.contains("does not support subagent retries")));
    }

    #[tokio::test]
    async fn retry_rejects_completed_child_without_creating_a_sibling() {
        let s = tests_support::state_with_agents().await;
        let root = primary(&s, "s").await;
        let child = subagent(&s, &root.run.run_id, "completed").await;
        s.cp.delegation()
            .complete(&child.run.run_id, "done")
            .await
            .unwrap();
        let before =
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len();

        let error = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": child.run.run_id }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.status, 400);
        assert!(error.message.contains("failed, cancelled, or interrupted"));
        assert_eq!(
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len(),
            before,
            "the API must reject before a retry sibling is inserted"
        );
    }

    #[tokio::test]
    async fn child_controls_reject_primary_and_active_retry_then_retry_terminal_child() {
        let s = tests_support::state_with_agents().await;
        let root = primary(&s, "s").await;
        let active = subagent(&s, &root.run.run_id, "active").await;
        let terminal = subagent(&s, &root.run.run_id, "terminal").await;
        s.cp.delegation()
            .fail(&terminal.run.run_id, "failed")
            .await
            .unwrap();

        for method in ["cancel_child_run", "retry_child_run"] {
            let error = dispatch(
                &s,
                method,
                json!({ "session_pk": "s", "run_id": root.run.run_id }),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 400, "{method}");
        }
        let active_retry = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": active.run.run_id }),
        )
        .await
        .unwrap_err();
        assert_eq!(active_retry.status, 400);

        let retry = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": terminal.run.run_id }),
        )
        .await
        .unwrap();
        assert_eq!(retry["status"], "queued");
        assert_eq!(retry["retryOf"], terminal.run.run_id);
    }

    #[tokio::test]
    async fn retry_rejects_deleted_profile_without_mutating_runs() {
        let s = tests_support::state_with_agents().await;
        let root = primary(&s, "s").await;
        let primary_id = s.agents.default_agent_id().await;
        let target = s
            .agents
            .create(crate::agents::types::AgentMutationInput {
                name: "Target".into(),
                description: "Target agent".into(),
                avatar: crate::agents::types::AgentAvatar {
                    color: "blue".into(),
                },
                model: crate::agents::types::AgentModel::Route {
                    route: "free".into(),
                },
                personality: crate::agents::personality::AgentPersonality::default_profile(),
                permissions: crate::agents::types::AgentPermissions {
                    mode: PermMode::Default,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: crate::agents::types::AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: crate::agents::types::AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        let child =
            s.cp.delegation()
                .queue_main(MainDelegationRequest {
                    parent_run_id: root.run.run_id,
                    target_agent_id: target.profile.id.clone(),
                    task: "delegated".into(),
                    context: None,
                    background: false,
                    dispatch: None,
                })
                .await
                .unwrap();
        s.cp.delegation()
            .fail(&child.run.run_id, "failed")
            .await
            .unwrap();
        s.agents.delete(&target.profile.id).await.unwrap();
        assert_eq!(s.agents.default_agent_id().await, primary_id);
        let before =
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len();

        let error = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": child.run.run_id }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len(),
            before
        );
    }

    #[tokio::test]
    async fn retry_rejects_invalid_profile_without_mutating_runs() {
        let root = tempfile::tempdir().unwrap().keep();
        let store = Arc::new(
            crate::store::Store::open(&root.join("core.sqlite"))
                .await
                .unwrap(),
        );
        let persistence =
            crate::agents::bootstrap::initialize_agent_persistence(root.clone(), store.clone())
                .await
                .unwrap();
        std::fs::write(
            root.join("agents/ryuzi/agent.yaml"),
            "schema_version: 1\nid: ryuzi\nname: Ryuzi\ndescription: invalid\navatar: { color: blue }\nmodel: { route: missing }\npermissions: { mode: ask, rules: [] }\nskills: { enabled: [] }\ntools: { native: [], plugins: [], apps: [] }\nloop: { max_turns: 1, max_tool_rounds: 1 }\n",
        )
        .unwrap();
        let registry = Arc::new(
            crate::agents::registry::AgentRegistry::load(root, store.clone())
                .await
                .unwrap(),
        );
        let persistence = crate::agents::bootstrap::AgentPersistence {
            registry: registry.clone(),
            ..persistence
        };
        let cp = crate::control::ControlPlane::new(
            store.clone(),
            crate::plugins::Registries::new(),
            persistence.clone(),
        )
        .await;
        let s = ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(store.clone())),
            cp,
            agents: registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            control_token: "t".into(),
        };
        s.cp.store().insert_session(session("s")).await.unwrap();
        let root_run =
            s.cp.store()
                .insert_primary_agent_run(NewAgentRun {
                    run_id: "root".into(),
                    session_pk: "s".into(),
                    parent_run_id: None,
                    retry_of: None,
                    source_tool_call_id: None,
                    dispatch_index: None,
                    primary_agent_id: "ryuzi".into(),
                    executing_agent_id: Some("ryuzi".into()),
                    executing_agent_name_snapshot: "Ryuzi".into(),
                    agent_kind: AgentRunKind::Primary,
                    task: "root".into(),
                    status: AgentRunStatus::Queued,
                    resolved_model: None,
                    resolved_effort: None,
                })
                .await
                .unwrap();
        let child =
            s.cp.store()
                .insert_agent_run(NewAgentRun {
                    run_id: "invalid-child".into(),
                    session_pk: "s".into(),
                    parent_run_id: Some(root_run.run_id),
                    retry_of: None,
                    source_tool_call_id: None,
                    dispatch_index: None,
                    primary_agent_id: "ryuzi".into(),
                    executing_agent_id: Some("ryuzi".into()),
                    executing_agent_name_snapshot: "Ryuzi".into(),
                    agent_kind: AgentRunKind::MainDelegate,
                    task: "child".into(),
                    status: AgentRunStatus::Failed,
                    resolved_model: None,
                    resolved_effort: None,
                })
                .await
                .unwrap();
        let before =
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len();

        let error = dispatch(
            &s,
            "retry_child_run",
            json!({ "session_pk": "s", "run_id": child.run_id }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(
            s.cp.store()
                .list_session_agent_runs("s")
                .await
                .unwrap()
                .len(),
            before
        );
    }
}
