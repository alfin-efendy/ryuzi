//! Orchestration RPC family (spec §8): submit a goal, read the task DAG,
//! cancel/retry, answer a block-for-human, and steer a live run. Cockpit's
//! task strip + drill-down + orchestrate toggle proxy these.

use super::{ok, params, ApiError};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "orch_submit",
    "orch_list_roots",
    "orch_tasks",
    "orch_cancel",
    "orch_retry",
    "orch_answer_block",
    "orch_steer",
];

#[derive(Deserialize)]
struct SubmitP {
    project_id: String,
    goal: String,
    decompose: bool,
    /// The originating chat/project session (its home chat). Optional so the
    /// agent's own tool path and headless callers still work.
    #[serde(default)]
    home_session_pk: Option<String>,
}
#[derive(Deserialize)]
struct RootP {
    root: String,
}
#[derive(Deserialize)]
struct TaskIdP {
    task_id: String,
}
#[derive(Deserialize)]
struct AnswerP {
    task_id: String,
    answer: String,
}
#[derive(Deserialize)]
struct SteerP {
    session_pk: String,
    text: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "orch_submit" => {
            let a: SubmitP = params(p)?;
            ok(crate::orch::submit(
                cp,
                &a.project_id,
                &a.goal,
                a.decompose,
                a.home_session_pk.as_deref(),
            )
            .await?)
        }
        "orch_list_roots" => {
            let all = crate::orch::list_tasks(cp.store(), None).await?;
            ok(all
                .into_iter()
                .filter(|t| t.root_id.is_none())
                .collect::<Vec<_>>())
        }
        "orch_tasks" => {
            let a: RootP = params(p)?;
            ok(crate::orch::list_tasks(cp.store(), Some(&a.root)).await?)
        }
        "orch_cancel" => {
            let a: RootP = params(p)?;
            let n = crate::orch::cancel_tree(cp.store(), &a.root).await?;
            crate::orch::emit_root_cancelled(cp, &a.root);
            ok(n)
        }
        "orch_retry" => {
            let a: TaskIdP = params(p)?;
            ok(crate::orch::retry_task(cp.store(), &a.task_id).await?)
        }
        "orch_answer_block" => {
            let a: AnswerP = params(p)?;
            ok(cp.answer_orch_block(&a.task_id, &a.answer).await?)
        }
        "orch_steer" => {
            let a: SteerP = params(p)?;
            let outcome = crate::orch::note_steer(cp, &a.session_pk, &a.text).await?;
            ok(match outcome {
                crate::orch::SteerOutcome::NoOrchestration => "noOrchestration",
                crate::orch::SteerOutcome::Noted => "noted",
                crate::orch::SteerOutcome::Cancelled => "cancelled",
            })
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support;

    #[tokio::test]
    async fn orch_submit_then_tasks_round_trips() {
        let s = tests_support::state_with_project().await;
        let project_id = s.cp.store().list_projects().await.unwrap()[0]
            .project_id
            .clone();
        let root = crate::api::dispatch(
            &s,
            "orch_submit",
            serde_json::json!({ "project_id": project_id, "goal": "do it", "decompose": false }),
        )
        .await
        .unwrap();
        let root = root.as_str().unwrap().to_string();
        let tasks = crate::api::dispatch(&s, "orch_tasks", serde_json::json!({ "root": root }))
            .await
            .unwrap();
        assert!(tasks
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["rootId"].is_null())); // the root row
    }
}
