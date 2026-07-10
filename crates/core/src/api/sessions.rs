//! Sessions/projects/settings/attachments RPC family — the largest
//! surface Cockpit's Tauri layer proxies today. Moved verbatim (per the
//! Move Recipe) from `apps/cockpit/src-tauri/src/commands.rs`; that file
//! keeps its own copy until the proxy rewrite in Tasks 15-16.

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
    "list_projects",
    "list_sessions",
    "connect_project",
    "clone_project",
    "list_branches",
    "start_session",
    "continue_session",
    "stop_session",
    "end_session",
    "list_messages",
    "stage_attachment",
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
    harness: String,
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
struct StageP {
    name: String,
    data_base64: String,
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
                .update_project(&a.project_id, a.model, a.perm_mode, &a.harness)
                .await?
                .ok_or_else(|| ApiError::not_found(format!("unknown project: {}", a.project_id)))?)
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
        "continue_session" => {
            let a: ContinueP = params(p)?;
            ok(continue_session(state, &a.session_pk, &a.prompt, a.options).await?)
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
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn apply_runtime_choice(
    cp: &ControlPlane,
    project_id: &str,
    runtime_id: Option<&str>,
    model: Option<&str>,
) -> Result<(), ApiError> {
    let runtime_id = runtime_id.filter(|v| !v.trim().is_empty());
    let model = model
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if runtime_id.is_none() && model.is_none() {
        return Ok(());
    };
    let harness = match runtime_id {
        Some(runtime_id) => harness_for_runtime(runtime_id)?,
        None => "",
    };
    let Some(project) = cp.store().get_project(project_id).await? else {
        return Err(ApiError::not_found(format!(
            "unknown project: {project_id}"
        )));
    };
    let next_harness = if harness.is_empty() {
        project.harness.as_str()
    } else {
        harness
    };
    let current_model = project.model.clone();
    // Ryuzi-only: a runtime choice no longer implies a model reset — the
    // composer always sends runtimeId "native", so `model: null` must keep
    // the project's pinned model instead of clearing it.
    let next_model = model.or_else(|| current_model.clone());
    if project.harness != next_harness || current_model != next_model {
        cp.store()
            .update_project(project_id, next_model, project.perm_mode, next_harness)
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
    apply_runtime_choice(
        cp,
        project_id,
        options.runtime_id.as_deref(),
        options.model.as_deref(),
    )
    .await?;
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
        let opts: crate::api::types::ChatRequestOptions = serde_json::from_value(json!({
            "runtimeId": "native",
            "model": "fable",
            "git": {"useWorktree": false, "createBranch": false, "branchName": null, "baseBranch": null}
        }))
        .unwrap();
        assert_eq!(opts.runtime_id.as_deref(), Some("native"));
        assert!(!opts.git.unwrap().use_worktree);
    }
}
