//! Orchestrated-task-graph RPCs: the 4 operations the deleted `ryuzi orch`
//! CLI used to call directly against the store, now exposed in-daemon so a
//! live event bus backs `submit_goal` (see `orch::submit`'s doc comment).

use super::{ok, params, ApiError};
use crate::orch;
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "submit_goal",
    "list_orch_tasks",
    "cancel_orch_task",
    "retry_orch_task",
];

#[derive(Deserialize)]
struct SubmitP {
    project_id: String,
    goal: String,
    decompose: bool,
}
#[derive(Deserialize)]
struct RootP {
    root: Option<String>,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "submit_goal" => {
            let a: SubmitP = params(p)?;
            ok(orch::submit(cp, &a.project_id, &a.goal, a.decompose).await?)
        }
        "list_orch_tasks" => {
            let a: RootP = params(p)?;
            ok(orch::list_tasks(cp.store(), a.root.as_deref()).await?)
        }
        "cancel_orch_task" => {
            let a: IdP = params(p)?;
            ok(orch::cancel_tree(cp.store(), &a.id).await?)
        }
        "retry_orch_task" => {
            let a: IdP = params(p)?;
            ok(orch::retry_task(cp.store(), &a.id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::state;
    use crate::domain::{PermMode, Project};
    use serde_json::json;

    async fn state_with_project() -> ApiState {
        let s = state().await;
        s.cp.store()
            .insert_project(Project {
                project_id: "p1".into(),
                name: "p1".into(),
                workdir: ".".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        s
    }

    #[tokio::test]
    async fn submit_goal_returns_a_root_id_for_a_known_project() {
        let s = state_with_project().await;
        let v = dispatch(
            &s,
            "submit_goal",
            json!({"project_id": "p1", "goal": "ship it", "decompose": false}),
        )
        .await
        .unwrap();
        let root = v.as_str().unwrap();
        assert!(root.starts_with("ot-"));
    }

    #[tokio::test]
    async fn submit_goal_rejects_an_unknown_project() {
        let s = state().await;
        let err = dispatch(
            &s,
            "submit_goal",
            json!({"project_id": "nope", "goal": "ship it", "decompose": false}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500);
    }

    #[tokio::test]
    async fn list_orch_tasks_returns_the_submitted_root() {
        let s = state_with_project().await;
        let root = dispatch(
            &s,
            "submit_goal",
            json!({"project_id": "p1", "goal": "ship it", "decompose": false}),
        )
        .await
        .unwrap();
        let root = root.as_str().unwrap().to_string();
        let list = dispatch(&s, "list_orch_tasks", json!({"root": null}))
            .await
            .unwrap();
        let tasks = list.as_array().unwrap();
        assert!(tasks.iter().any(|t| t["id"] == root));
        // Field naming: camelCase per the RPC param/response convention.
        let r = tasks.iter().find(|t| t["id"] == root).unwrap();
        assert_eq!(r["rootId"], Value::Null);
        assert_eq!(r["projectId"], "p1");
    }

    #[tokio::test]
    async fn cancel_orch_task_returns_a_count() {
        let s = state_with_project().await;
        let root = dispatch(
            &s,
            "submit_goal",
            json!({"project_id": "p1", "goal": "ship it", "decompose": false}),
        )
        .await
        .unwrap();
        let root = root.as_str().unwrap().to_string();
        let n = dispatch(&s, "cancel_orch_task", json!({"id": root}))
            .await
            .unwrap();
        // The root plus its single planned child.
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn retry_orch_task_on_a_non_failed_row_returns_false() {
        let s = state_with_project().await;
        let root = dispatch(
            &s,
            "submit_goal",
            json!({"project_id": "p1", "goal": "ship it", "decompose": false}),
        )
        .await
        .unwrap();
        let root = root.as_str().unwrap().to_string();
        let retried = dispatch(&s, "retry_orch_task", json!({"id": root}))
            .await
            .unwrap();
        assert_eq!(retried, false);
    }
}
