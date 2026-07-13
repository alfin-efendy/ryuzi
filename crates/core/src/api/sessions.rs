//! Sessions/projects/settings/attachments RPC family — the largest
//! surface Cockpit's Tauri layer proxies today. Moved verbatim (per the
//! Move Recipe) from `apps/cockpit/src-tauri/src/commands.rs`; that file now
//! proxies every handle here through `EngineClient::rpc` (Task 15).

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::branches::BranchList;
use crate::domain::{AttachmentRef, Session, SessionGitOptions};
use crate::harness::TurnPrompt;
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

pub(crate) const HANDLES: &[&str] = &[
    "get_setting",
    "set_setting",
    "update_project",
    "update_session_perm_mode",
    "list_projects",
    "list_sessions",
    "list_agent_sessions",
    "connect_project",
    "clone_project",
    "list_branches",
    "start_session",
    "start_chat_session",
    "continue_session",
    "steer",
    "stop_session",
    "end_session",
    "list_messages",
    "stage_attachment",
    "attachments_root",
    "list_tool_policies",
    "delete_tool_policy",
];

/// Largest pasted attachment accepted from the webview (decoded size).
const MAX_STAGE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Deserialize)]
struct Key {
    key: String,
}
#[derive(Deserialize)]
struct KeyValue {
    key: String,
    value: String,
}
#[derive(Deserialize)]
struct UpdateProjectP {
    project_id: String,
    model: Option<String>,
    perm_mode: crate::domain::PermMode,
}
#[derive(Deserialize)]
struct UpdateSessionPermModeP {
    session_pk: String,
    perm_mode: crate::domain::PermMode,
}
#[derive(Deserialize)]
struct ProjectIdOpt {
    project_id: Option<String>,
}
#[derive(Deserialize)]
struct ConnectP {
    workdir: String,
    name: String,
}
#[derive(Deserialize)]
struct CloneP {
    url: String,
    dest_parent: String,
}
#[derive(Deserialize)]
struct ProjectIdP {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartP {
    project_id: String,
    primary_agent_id: String,
    turn: TurnInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartChatP {
    primary_agent_id: String,
    turn: TurnInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueP {
    session_pk: String,
    turn: TurnInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSessionsP {
    agent_id: String,
    limit: u32,
}
#[derive(Deserialize)]
struct SessionPkP {
    session_pk: String,
}
#[derive(Deserialize)]
struct SteerP {
    session_pk: String,
    text: String,
}
#[derive(Deserialize)]
struct StageP {
    name: String,
    data_base64: String,
}
#[derive(Deserialize)]
struct DeleteToolPolicyP {
    project_id: String,
    tool: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "get_setting" => {
            let a: Key = params(p)?;
            ok(cp.store().get_setting(&a.key).await?)
        }
        "set_setting" => {
            let a: KeyValue = params(p)?;
            ok(cp
                .store()
                .set_setting(crate::domain::WriteOrigin::User, &a.key, &a.value)
                .await?)
        }
        "update_project" => {
            let a: UpdateProjectP = params(p)?;
            ok(cp
                .store()
                .update_project(&a.project_id, a.model, a.perm_mode)
                .await?
                .ok_or_else(|| ApiError::not_found(format!("unknown project: {}", a.project_id)))?)
        }
        "update_session_perm_mode" => {
            let a: UpdateSessionPermModeP = params(p)?;
            ok(cp
                .store()
                .update_session_perm_mode(&a.session_pk, a.perm_mode)
                .await?)
        }
        "list_projects" => ok(cp.list_projects().await?),
        "list_sessions" => {
            let a: ProjectIdOpt = params(p)?;
            ok(cp.list_sessions(a.project_id.as_deref()).await?)
        }
        "list_agent_sessions" => {
            let a: AgentSessionsP = params(p)?;
            ok(cp.list_agent_sessions(&a.agent_id, a.limit).await?)
        }
        "connect_project" => {
            let a: ConnectP = params(p)?;
            ok(cp
                .connect_project(std::path::Path::new(&a.workdir), &a.name)
                .await?)
        }
        "clone_project" => {
            let a: CloneP = params(p)?;
            ok(cp
                .clone_project(&a.url, std::path::Path::new(&a.dest_parent))
                .await?)
        }
        "list_branches" => {
            let a: ProjectIdP = params(p)?;
            ok(list_branches(state, &a.project_id).await?)
        }
        "start_session" => {
            let a: StartP = params(p)?;
            ok(start_session(state, &a.project_id, &a.primary_agent_id, a.turn).await?)
        }
        "start_chat_session" => {
            let a: StartChatP = params(p)?;
            let attachments = attachment_refs_from_paths(&a.turn.attachments).await?;
            let agent_prompt = chat_agent_prompt(&a.turn.text, a.turn.context.as_ref());
            ok(state
                .cp
                .start_agent_session_with_prompt(
                    None,
                    &a.primary_agent_id,
                    TurnPrompt::text(agent_prompt, a.turn.text),
                    "cockpit",
                    &attachments,
                    None,
                )
                .await?)
        }
        "continue_session" => {
            let a: ContinueP = params(p)?;
            ok(continue_session(state, &a.session_pk, a.turn).await?)
        }
        "steer" => {
            let a: SteerP = params(p)?;
            ok(cp.steer_session(&a.session_pk, &a.text).await?)
        }
        "stop_session" => {
            let a: SessionPkP = params(p)?;
            ok(cp.stop_session(&a.session_pk).await?)
        }
        "end_session" => {
            let a: SessionPkP = params(p)?;
            ok(cp.end_session(&a.session_pk).await?)
        }
        "list_messages" => {
            let a: SessionPkP = params(p)?;
            ok(cp.list_messages(&a.session_pk).await?)
        }
        "stage_attachment" => {
            let a: StageP = params(p)?;
            ok(stage_attachment(state, &a.name, &a.data_base64).await?)
        }
        "attachments_root" => ok(state
            .cp
            .attachments_root()
            .await
            .to_string_lossy()
            .into_owned()),
        "list_tool_policies" => ok(cp.list_tool_policies().await?),
        "delete_tool_policy" => {
            let a: DeleteToolPolicyP = params(p)?;
            ok(cp.delete_tool_policy(&a.project_id, &a.tool).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn attachment_refs_from_paths(paths: &[String]) -> Result<Vec<AttachmentRef>, ApiError> {
    let mut out = Vec::new();
    for raw in paths {
        if raw.trim().is_empty() {
            continue;
        }
        let path = tokio::fs::canonicalize(raw)
            .await
            .map_err(anyhow::Error::from)?;
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(anyhow::Error::from)?;
        if !meta.is_file() {
            return Err(ApiError::bad_request(format!(
                "attachment is not a file: {}",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        out.push(AttachmentRef {
            name,
            url: crate::attachments::file_url_for_path(&path)?.to_string(),
            content_type: content_type_for_path(&path),
            size: meta.len(),
        });
    }
    Ok(out)
}

async fn list_branches(state: &ApiState, project_id: &str) -> Result<BranchList, ApiError> {
    let cp = &state.cp;
    let project = cp
        .store()
        .get_project(project_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown project: {project_id}")))?;
    // git2 is blocking; keep it off the async runtime's worker thread.
    let list = tokio::task::spawn_blocking(move || {
        crate::branches::list_branches(Path::new(&project.workdir))
    })
    .await
    .map_err(|e| ApiError {
        status: 500,
        message: format!("list_branches task failed: {e}"),
    })??;
    Ok(list)
}

async fn start_session(
    state: &ApiState,
    project_id: &str,
    primary_agent_id: &str,
    turn: TurnInput,
) -> Result<Session, ApiError> {
    let cp = &state.cp;
    let git: Option<SessionGitOptions> = turn.git.clone().map(Into::into);
    let attachments = attachment_refs_from_paths(&turn.attachments).await?;
    let agent_prompt = chat_agent_prompt(&turn.text, turn.context.as_ref());
    Ok(cp
        .start_agent_session_with_prompt(
            Some(project_id),
            primary_agent_id,
            TurnPrompt::text(agent_prompt, turn.text),
            "cockpit",
            &attachments,
            git,
        )
        .await?)
}

async fn continue_session(
    state: &ApiState,
    session_pk: &str,
    turn: TurnInput,
) -> Result<(), ApiError> {
    let attachments = attachment_refs_from_paths(&turn.attachments).await?;
    let agent_prompt = chat_agent_prompt(&turn.text, turn.context.as_ref());
    state
        .cp
        .continue_agent_session_with_prompt(
            session_pk,
            TurnPrompt::text(agent_prompt, turn.text),
            &attachments,
        )
        .await?;
    Ok(())
}

/// Write pasted bytes into the attachments staging area and return the
/// absolute path — from there the file flows through the normal attachment
/// pipeline on send. Staging is wiped on app start (see cockpit's lib.rs
/// setup, until Task 15+ moves that responsibility here too).
async fn stage_attachment(
    state: &ApiState,
    name: &str,
    data_base64: &str,
) -> Result<String, ApiError> {
    use base64::Engine as _;
    let cp = &state.cp;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| ApiError::bad_request(format!("invalid attachment data: {e}")))?;
    if bytes.len() > MAX_STAGE_BYTES {
        return Err(ApiError::bad_request(format!(
            "attachment too large ({} bytes)",
            bytes.len()
        )));
    }
    let dir = cp
        .attachments_root()
        .await
        .join("staging")
        .join(crate::paths::new_id());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(sanitize_file_name(name));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;
    use serial_test::serial;

    #[tokio::test]
    #[serial]
    async fn start_chat_session_dispatches_owned_turn_input() {
        let s = crate::api::tests_support::state_with_fake_native().await;
        let primary_agent_id = s.cp.registry().default_agent_id().await;
        let out = dispatch(
            &s,
            "start_chat_session",
            json!({
                "primaryAgentId": primary_agent_id,
                "turn": { "text": "hi", "attachments": [] }
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["projectId"], serde_json::Value::Null);
        assert_eq!(out["kind"], "chat");
        assert!(out["primaryAgentId"].is_string());
    }

    #[tokio::test]
    async fn steer_on_an_unknown_session_errors_like_continue_session() {
        // No live handle AND no session row at all: `steer` dispatches through
        // to `ControlPlane::steer_session`'s fallback, which — like
        // `continue_session` — must fail cleanly on an unknown session_pk
        // rather than panic or silently succeed. (The "live handle received
        // it" / "fell back to a new turn" branching itself is covered by
        // `control::tests::steer_session_*`, which can synchronize on the
        // background-started live handle that this dispatch-only layer
        // cannot.)
        let s = state().await;
        let err = dispatch(
            &s,
            "steer",
            json!({"session_pk": "no-such-session", "text": "hi"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500);
    }

    #[tokio::test]
    async fn settings_round_trip_via_rpc() {
        let s = state().await;
        dispatch(&s, "set_setting", json!({"key": "k1", "value": "v1"}))
            .await
            .unwrap();
        let got = dispatch(&s, "get_setting", json!({"key": "k1"}))
            .await
            .unwrap();
        assert_eq!(got, json!("v1"));
    }
}
