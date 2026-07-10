//! Native runtime introspection: agents, slash commands, and per-session
//! todos exposed to Cockpit. Moved verbatim (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/native_cmd.rs`; that file keeps its own copy
//! until the proxy rewrite in Tasks 15-16. `AgentRegistry::load` and
//! `CommandRegistry::load` walk the project's workdir on disk — this now
//! runs in the daemon process, which is correct since the daemon is where
//! sessions actually run.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::harness::native::agents::AgentRegistry;
use crate::harness::native::commands::CommandRegistry;
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

pub(crate) const HANDLES: &[&str] = &["native_agents", "native_commands", "session_todos"];

#[derive(Deserialize)]
struct ProjectIdP {
    project_id: String,
}
#[derive(Deserialize)]
struct SessionPkP {
    session_pk: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "native_agents" => {
            let a: ProjectIdP = params(p)?;
            ok(native_agents(cp, &a.project_id).await?)
        }
        "native_commands" => {
            let a: ProjectIdP = params(p)?;
            ok(native_commands(cp, &a.project_id).await?)
        }
        "session_todos" => {
            let a: SessionPkP = params(p)?;
            let rows = cp.store().list_todos(&a.session_pk).await?;
            ok(rows
                .into_iter()
                .map(|(content, status)| TodoItem { content, status })
                .collect::<Vec<_>>())
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn project_workdir(cp: &ControlPlane, project_id: &str) -> Result<String, ApiError> {
    let projects = cp.list_projects().await?;
    projects
        .into_iter()
        .find(|p| p.project_id == project_id)
        .map(|p| p.workdir)
        .ok_or_else(|| ApiError::not_found(format!("unknown project {project_id}")))
}

/// The agents available for a project (built-ins plus discovered custom agents).
async fn native_agents(cp: &ControlPlane, project_id: &str) -> Result<Vec<AgentInfo>, ApiError> {
    let workdir = project_workdir(cp, project_id).await?;
    let reg = AgentRegistry::load(Path::new(&workdir));
    Ok(reg
        .all()
        .into_iter()
        .map(|a| AgentInfo {
            name: a.name,
            description: a.description,
            mode: a.mode.as_str().to_string(),
            builtin: a.builtin,
        })
        .collect())
}

/// The slash commands available for a project.
async fn native_commands(
    cp: &ControlPlane,
    project_id: &str,
) -> Result<Vec<CommandInfo>, ApiError> {
    let workdir = project_workdir(cp, project_id).await?;
    let reg = CommandRegistry::load(Path::new(&workdir));
    Ok(reg
        .all()
        .into_iter()
        .map(|c| CommandInfo {
            name: c.name,
            description: c.description,
            agent: c.agent,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[tokio::test]
    async fn session_todos_returns_empty_on_fresh_store_via_rpc() {
        let s = state().await;
        let out = dispatch(&s, "session_todos", json!({"session_pk": "nope"}))
            .await
            .unwrap();
        assert_eq!(out, json!([]));
    }

    #[tokio::test]
    async fn native_agents_unknown_project_is_not_found() {
        let s = state().await;
        let err = dispatch(&s, "native_agents", json!({"project_id": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(err.status, 404);
    }
}
