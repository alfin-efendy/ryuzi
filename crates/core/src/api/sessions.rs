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
    "session_queue",
    "enqueue_session_message",
    "remove_session_message",
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
struct EnqueueSessionMessageP {
    session_pk: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
}
#[derive(Deserialize)]
struct RemoveSessionMessageP {
    session_pk: String,
    id: String,
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
            let options = a.options.unwrap_or_default();
            let attachments = attachment_refs_from_paths(&options.attachments).await?;
            let agent_prompt = chat_agent_prompt(&a.prompt, options.context.as_ref());
            ok(state
                .cp
                .start_chat_session_with_runtime(
                    TurnPrompt::text(agent_prompt, a.prompt),
                    "cockpit",
                    &attachments,
                    options.model,
                    options.effort,
                    options.perm_mode,
                )
                .await?)
        }
        "continue_session" => {
            let a: ContinueP = params(p)?;
            ok(continue_session(state, &a.session_pk, &a.prompt, a.options).await?)
        }
        "session_queue" => {
            let a: SessionPkP = params(p)?;
            ok(session_queue(cp, &a.session_pk).await?)
        }
        "enqueue_session_message" => {
            let a: EnqueueSessionMessageP = params(p)?;
            ok(enqueue_session_message(state, &a.session_pk, &a.prompt, a.options).await?)
        }
        "remove_session_message" => {
            let a: RemoveSessionMessageP = params(p)?;
            ensure_session_exists(cp, &a.session_pk).await?;
            let removed = cp
                .store()
                .remove_session_prompt(&a.session_pk, &a.id)
                .await?;
            if removed {
                cp.emit(crate::domain::CoreEvent::SessionQueueChanged {
                    session_pk: a.session_pk,
                });
            }
            ok(removed)
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

async fn ensure_session_exists(cp: &ControlPlane, session_pk: &str) -> Result<(), ApiError> {
    if cp.store().get_session(session_pk).await?.is_none() {
        return Err(ApiError::not_found(format!(
            "unknown session: {session_pk}"
        )));
    }
    Ok(())
}

async fn session_queue(
    cp: &ControlPlane,
    session_pk: &str,
) -> Result<Vec<QueuedMessageInfo>, ApiError> {
    ensure_session_exists(cp, session_pk).await?;
    Ok(cp
        .store()
        .list_session_prompt_queue(session_pk)
        .await?
        .into_iter()
        .map(|prompt| QueuedMessageInfo {
            id: prompt.id,
            text: prompt.display,
        })
        .collect())
}

async fn enqueue_session_message(
    state: &ApiState,
    session_pk: &str,
    prompt: &str,
    options: Option<ChatRequestOptions>,
) -> Result<QueuedMessageInfo, ApiError> {
    let cp = &state.cp;
    ensure_session_exists(cp, session_pk).await?;
    let options = options.unwrap_or_default();
    let id = crate::paths::new_id();
    let attachments = queue_owned_attachments(
        cp,
        &id,
        attachment_refs_from_paths(&options.attachments).await?,
    )
    .await?;
    let queued = crate::domain::QueuedSessionPrompt {
        id: id.clone(),
        session_pk: session_pk.to_string(),
        agent: chat_agent_prompt(prompt, options.context.as_ref()),
        display: prompt.to_string(),
        attachments,
        created_at: crate::paths::now_ms(),
    };
    cp.store().enqueue_session_prompt(queued).await?;
    cp.emit(crate::domain::CoreEvent::SessionQueueChanged {
        session_pk: session_pk.to_string(),
    });
    Ok(QueuedMessageInfo {
        id,
        text: prompt.to_string(),
    })
}

async fn queue_owned_attachments(
    cp: &ControlPlane,
    message_id: &str,
    attachments: Vec<AttachmentRef>,
) -> Result<Vec<AttachmentRef>, ApiError> {
    let root = cp.attachments_root().await;
    let staging = root.join("staging");
    let staging = tokio::fs::canonicalize(&staging).await.ok();
    let destination_dir = root.join("queue").join(message_id);
    let mut durable = Vec::with_capacity(attachments.len());
    for mut attachment in attachments {
        let Ok(source) = url::Url::parse(&attachment.url)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or(())
        else {
            durable.push(attachment);
            continue;
        };
        let source = tokio::fs::canonicalize(source)
            .await
            .map_err(anyhow::Error::from)?;
        if staging.as_ref().is_some_and(|dir| source.starts_with(dir)) {
            tokio::fs::create_dir_all(&destination_dir)
                .await
                .map_err(anyhow::Error::from)?;
            let file_name = source
                .file_name()
                .and_then(|name| name.to_str())
                .map(crate::api::types::sanitize_file_name)
                .unwrap_or_else(|| "file".to_string());
            let mut queued_name = file_name.clone();
            let mut destination = destination_dir.join(&queued_name);
            let mut duplicate = 2;
            while tokio::fs::try_exists(&destination)
                .await
                .map_err(anyhow::Error::from)?
            {
                let path = Path::new(&file_name);
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("file");
                let extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| format!(".{extension}"))
                    .unwrap_or_default();
                queued_name = format!("{stem} ({duplicate}){extension}");
                destination = destination_dir.join(&queued_name);
                duplicate += 1;
            }
            tokio::fs::copy(&source, &destination)
                .await
                .map_err(anyhow::Error::from)?;
            let destination = tokio::fs::canonicalize(&destination)
                .await
                .map_err(anyhow::Error::from)?;
            attachment.name = queued_name;
            attachment.url = crate::attachments::file_url_for_path(&destination)?.to_string();
        }
        durable.push(attachment);
    }
    Ok(durable)
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
            json!({"prompt": "hi", "options": {
                "model": "openai/gpt-5.5", "effort": "high", "permMode": "plan",
                "context": null, "attachments": [], "git": null
            }}),
        )
        .await
        .unwrap();
        assert_eq!(out["projectId"], serde_json::Value::Null);
        assert_eq!(out["kind"], "chat");
        assert_eq!(out["permMode"], "plan");
        let runtime =
            s.cp.store()
                .get_session_runtime_settings(out["sessionPk"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(runtime.model.as_deref(), Some("openai/gpt-5.5"));
        assert_eq!(runtime.effort.as_deref(), Some("high"));
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
    async fn enqueue_list_and_remove_session_messages_via_rpc() {
        let s = state().await;
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "s1".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: crate::domain::SessionStatus::Idle,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: crate::domain::PermMode::Default,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();

        let mut events = s.cp.subscribe();
        let first = dispatch(
            &s,
            "enqueue_session_message",
            json!({"session_pk": "s1", "prompt": "first", "options": {"attachments": []}}),
        )
        .await
        .unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            crate::domain::CoreEvent::SessionQueueChanged {
                session_pk: "s1".into(),
            }
        );
        let second = dispatch(
            &s,
            "enqueue_session_message",
            json!({"session_pk": "s1", "prompt": "second", "options": {"attachments": []}}),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "session_queue", json!({"session_pk": "s1"}))
                .await
                .unwrap(),
            json!([first, second])
        );
        assert!(dispatch(
            &s,
            "remove_session_message",
            json!({"session_pk": "s1", "id": second["id"]}),
        )
        .await
        .unwrap()
        .as_bool()
        .unwrap());
        assert_eq!(
            events.recv().await.unwrap(),
            crate::domain::CoreEvent::SessionQueueChanged {
                session_pk: "s1".into(),
            }
        );
        assert_eq!(
            events.recv().await.unwrap(),
            crate::domain::CoreEvent::SessionQueueChanged {
                session_pk: "s1".into(),
            }
        );
        assert_eq!(
            dispatch(
                &s,
                "remove_session_message",
                json!({"session_pk": "s1", "id": "missing"}),
            )
            .await
            .unwrap(),
            json!(false)
        );
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            dispatch(&s, "session_queue", json!({"session_pk": "s1"}))
                .await
                .unwrap(),
            json!([first])
        );
    }

    #[tokio::test]
    async fn enqueue_session_message_preserves_same_named_staged_attachments() {
        use crate::settings::SettingsStore;

        let s = state().await;
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "s-duplicate-staged".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: crate::domain::SessionStatus::Idle,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: crate::domain::PermMode::Default,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        SettingsStore::new(s.cp.store().clone())
            .set("workdir_root", dir.path().to_str().unwrap())
            .await
            .unwrap();
        let first = dispatch(
            &s,
            "stage_attachment",
            json!({"name": "report.txt", "data_base64": "Zmlyc3Q="}),
        )
        .await
        .unwrap();
        let second = dispatch(
            &s,
            "stage_attachment",
            json!({"name": "report.txt", "data_base64": "c2Vjb25k"}),
        )
        .await
        .unwrap();
        let queued = dispatch(
            &s,
            "enqueue_session_message",
            json!({
                "session_pk": "s-duplicate-staged",
                "prompt": "with duplicate files",
                "options": {"attachments": [first, second]}
            }),
        )
        .await
        .unwrap();
        let root = s.cp.attachments_root().await;
        tokio::fs::remove_dir_all(root.join("staging"))
            .await
            .unwrap();
        let prompt =
            s.cp.store()
                .list_session_prompt_queue("s-duplicate-staged")
                .await
                .unwrap()
                .pop()
                .unwrap();

        assert_eq!(prompt.id, queued["id"]);
        assert_eq!(prompt.attachments.len(), 2);
        assert_ne!(prompt.attachments[0].name, prompt.attachments[1].name);
        let paths = prompt
            .attachments
            .iter()
            .map(|attachment| {
                url::Url::parse(&attachment.url)
                    .unwrap()
                    .to_file_path()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_ne!(paths[0], paths[1]);
        for path in &paths {
            assert!(path.starts_with(root.join("queue").join(queued["id"].as_str().unwrap())));
        }
        let mut contents = vec![
            tokio::fs::read(&paths[0]).await.unwrap(),
            tokio::fs::read(&paths[1]).await.unwrap(),
        ];
        contents.sort();
        assert_eq!(contents, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[tokio::test]
    async fn enqueue_session_message_keeps_staged_attachments_after_staging_is_deleted() {
        use crate::settings::SettingsStore;

        let s = state().await;
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "s-staged".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: crate::domain::SessionStatus::Idle,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: crate::domain::PermMode::Default,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        SettingsStore::new(s.cp.store().clone())
            .set("workdir_root", dir.path().to_str().unwrap())
            .await
            .unwrap();
        let staged = dispatch(
            &s,
            "stage_attachment",
            json!({"name": "note.txt", "data_base64": "cGVyc2lzdA=="}),
        )
        .await
        .unwrap();
        let queued = dispatch(
            &s,
            "enqueue_session_message",
            json!({
                "session_pk": "s-staged",
                "prompt": "with file",
                "options": {"attachments": [staged]}
            }),
        )
        .await
        .unwrap();
        let root = s.cp.attachments_root().await;
        tokio::fs::remove_dir_all(root.join("staging"))
            .await
            .unwrap();
        let prompt =
            s.cp.store()
                .list_session_prompt_queue("s-staged")
                .await
                .unwrap()
                .pop()
                .unwrap();
        assert_eq!(prompt.id, queued["id"]);
        let path = url::Url::parse(&prompt.attachments[0].url)
            .unwrap()
            .to_file_path()
            .unwrap();
        assert!(path.starts_with(root.join("queue").join(queued["id"].as_str().unwrap())));
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"persist");
    }

    #[tokio::test]
    async fn queue_rpc_rejects_unknown_sessions() {
        let s = state().await;

        let err = dispatch(&s, "session_queue", json!({"session_pk": "missing"}))
            .await
            .unwrap_err();
        assert_eq!(err.status, 404);

        let err = dispatch(
            &s,
            "remove_session_message",
            json!({"session_pk": "missing", "id": "message"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 404);
    }

    #[tokio::test]
    async fn enqueue_session_message_unknown_session_is_not_found() {
        let s = state().await;
        let err = dispatch(
            &s,
            "enqueue_session_message",
            json!({"session_pk": "missing", "prompt": "hello", "options": {"attachments": []}}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 404);
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
