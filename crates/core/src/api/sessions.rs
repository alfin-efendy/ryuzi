//! Sessions/projects/settings/attachments RPC family — the largest
//! surface Cockpit's Tauri layer proxies today. Moved verbatim (per the
//! Move Recipe) from `apps/cockpit/src-tauri/src/commands.rs`; that file now
//! proxies every handle here through `EngineClient::rpc` (Task 15).

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::branches::BranchList;
use crate::control::ControlPlane;
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
struct StartP {
    project_id: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
}
#[derive(Deserialize)]
struct StartChatP {
    prompt: String,
    options: Option<ChatRequestOptions>,
}
#[derive(Deserialize)]
struct ContinueP {
    session_pk: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
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
            ok(cp.store().set_setting(&a.key, &a.value).await?)
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
            ok(start_session(state, &a.project_id, &a.prompt, a.options).await?)
        }
        "start_chat_session" => {
            let a: StartChatP = params(p)?;
            let attachments = attachment_refs_from_paths(
                &a.options
                    .as_ref()
                    .map(|o| o.attachments.clone())
                    .unwrap_or_default(),
            )
            .await?;
            let agent_prompt = chat_agent_prompt(
                &a.prompt,
                a.options.as_ref().and_then(|o| o.context.as_ref()),
            );
            ok(state
                .cp
                .start_chat_session(
                    TurnPrompt::text(agent_prompt, a.prompt),
                    "cockpit",
                    &attachments,
                )
                .await?)
        }
        "continue_session" => {
            let a: ContinueP = params(p)?;
            ok(continue_session(state, &a.session_pk, &a.prompt, a.options).await?)
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

/// Persist the composer's model choice on the project row. `model: None`
/// keeps the project's pinned model instead of clearing it — the composer
/// sends null when the user didn't touch the picker.
async fn apply_model_choice(
    cp: &ControlPlane,
    project_id: &str,
    model: Option<&str>,
) -> Result<(), ApiError> {
    let model = model
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let Some(project) = cp.store().get_project(project_id).await? else {
        return Err(ApiError::not_found(format!(
            "unknown project: {project_id}"
        )));
    };
    let next_model = model.or_else(|| project.model.clone());
    if project.model != next_model {
        cp.store()
            .update_project(project_id, next_model, project.perm_mode)
            .await?;
    }
    Ok(())
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
    prompt: &str,
    options: Option<ChatRequestOptions>,
) -> Result<Session, ApiError> {
    let cp = &state.cp;
    let options = options.unwrap_or_default();
    apply_model_choice(cp, project_id, options.model.as_deref()).await?;
    let git: Option<SessionGitOptions> = options.git.clone().map(Into::into);
    let attachments = attachment_refs_from_paths(&options.attachments).await?;
    let agent_prompt = chat_agent_prompt(prompt, options.context.as_ref());
    Ok(cp
        .start_session_with_prompt(
            project_id,
            TurnPrompt::text(agent_prompt, prompt),
            "cockpit",
            &attachments,
            git,
            options.perm_mode,
            None,
        )
        .await?)
}

async fn continue_session(
    state: &ApiState,
    session_pk: &str,
    prompt: &str,
    options: Option<ChatRequestOptions>,
) -> Result<(), ApiError> {
    let cp = &state.cp;
    let options = options.unwrap_or_default();
    let attachments = attachment_refs_from_paths(&options.attachments).await?;
    let agent_prompt = chat_agent_prompt(prompt, options.context.as_ref());
    Ok(cp
        .continue_session_with_prompt(
            session_pk,
            TurnPrompt::text(agent_prompt, prompt),
            &attachments,
        )
        .await?)
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
    async fn start_chat_session_dispatches() {
        let s = crate::api::tests_support::state_with_fake_native().await;
        let out = dispatch(
            &s,
            "start_chat_session",
            json!({"prompt": "hi", "options": null}),
        )
        .await
        .unwrap();
        assert_eq!(out["projectId"], serde_json::Value::Null);
        assert_eq!(out["kind"], "chat");
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

    #[tokio::test]
    async fn start_session_decodes_camel_case_options() {
        // Params come from the Tauri proxy as the SAME camelCase JSON the
        // frontend already sends — the DTOs' serde attrs must accept it.
        // Native-only: a legacy `runtimeId` is ignored, never deserialized.
        let opts: crate::api::types::ChatRequestOptions = serde_json::from_value(json!({
            "runtimeId": "native",
            "model": "fable",
            "git": {"useWorktree": false, "createBranch": false, "branchName": null, "baseBranch": null}
        }))
        .unwrap();
        assert_eq!(opts.model.as_deref(), Some("fable"));
        assert!(!opts.git.unwrap().use_worktree);
    }

    #[tokio::test]
    async fn apply_model_choice_keeps_the_pinned_model_when_none_is_sent() {
        use crate::domain::{PermMode, Project};

        let s = state().await;
        s.cp.store()
            .insert_project(Project {
                project_id: "p1".into(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                model: Some("openrouter/qwen3:free".into()),
                effort: None,
                perm_mode: PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();

        // The composer may send model: null; the pinned model must survive.
        super::apply_model_choice(&s.cp, "p1", None).await.unwrap();

        let got = s.cp.store().get_project("p1").await.unwrap().unwrap();
        assert_eq!(
            got.model.as_deref(),
            Some("openrouter/qwen3:free"),
            "model:null must not clear the pinned model"
        );
    }
}
