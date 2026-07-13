use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::api::fsview_api::{content_type_for_path, MediaFile, MAX_MEDIA_READ_BYTES};
use ryuzi_core::branches::BranchList;
use ryuzi_core::domain::{ApprovalResponse, ToolPolicyRow};
use ryuzi_core::llm_router::model_effort::{ModelPreferenceKey, ProjectRuntimeInfo};
use ryuzi_core::{Message, OrchTask, PermMode, Project, Session};
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

// Cockpit's DTOs for these now live in `ryuzi_core::api::types`.
pub use ryuzi_core::api::types::{SessionRuntimeInfo, TurnInput};

type R<T> = Result<T, CmdError>;
// The old in-process `ControlPlane` state extractor is gone: every engine
// command below is a thin proxy over the daemon's HTTP control API instead.
// P3-4: `Engine` now holds the multi-runner `EngineManager`; each command
// resolves the runner-specific `EngineClient` via `runner_id` (default
// `"local"`) before proxying.
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn get_setting(
    engine: Engine<'_>,
    runner_id: Option<String>,
    key: String,
) -> R<Option<String>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("get_setting", serde_json::json!({ "key": key }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_setting(
    engine: Engine<'_>,
    runner_id: Option<String>,
    key: String,
    value: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_setting",
            serde_json::json!({ "key": key, "value": value }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_project(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    model: Option<String>,
    perm_mode: PermMode,
) -> R<Project> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_project",
            serde_json::json!({
                "project_id": project_id, "model": model,
                "perm_mode": perm_mode,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_project_perm_mode(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    perm_mode: PermMode,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_project_perm_mode",
            serde_json::json!({ "project_id": project_id, "perm_mode": perm_mode }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn project_runtime_info(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
) -> R<ProjectRuntimeInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "project_runtime_info",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_project_runtime(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    model: Option<String>,
    effort: Option<String>,
) -> R<ProjectRuntimeInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_project_runtime",
            serde_json::json!({
                "project_id": project_id, "model": model, "effort": effort,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_model_effort_preference(
    engine: Engine<'_>,
    runner_id: Option<String>,
    key: ModelPreferenceKey,
    effort: Option<String>,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_model_effort_preference",
            serde_json::json!({ "key": key, "effort": effort }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn session_runtime_info(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<SessionRuntimeInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "session_runtime_info",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_session_runtime(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    model: Option<String>,
    effort: Option<String>,
) -> R<SessionRuntimeInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_session_runtime",
            serde_json::json!({ "session_pk": session_pk, "model": model, "effort": effort }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_session_perm_mode(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    perm_mode: PermMode,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_session_perm_mode",
            serde_json::json!({ "session_pk": session_pk, "perm_mode": perm_mode }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_projects(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<Project>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_projects", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_sessions(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: Option<String>,
) -> R<Vec<Session>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_sessions",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn connect_project(
    engine: Engine<'_>,
    runner_id: Option<String>,
    workdir: String,
    name: String,
) -> R<Project> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "connect_project",
            serde_json::json!({ "workdir": workdir, "name": name }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn clone_project(
    engine: Engine<'_>,
    runner_id: Option<String>,
    url: String,
    dest_parent: String,
) -> R<Project> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "clone_project",
            serde_json::json!({ "url": url, "dest_parent": dest_parent }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_branches(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
) -> R<BranchList> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_branches",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_submit(
    engine: Engine<'_>,
    project_id: String,
    goal: String,
    decompose: bool,
    home_session_pk: Option<String>,
) -> R<String> {
    let client = engine.client("local")?;
    client
        .rpc(
            "orch_submit",
            serde_json::json!({
                "project_id": project_id, "goal": goal, "decompose": decompose,
                "home_session_pk": home_session_pk,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_list_roots(engine: Engine<'_>) -> R<Vec<OrchTask>> {
    let client = engine.client("local")?;
    client.rpc("orch_list_roots", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_tasks(engine: Engine<'_>, root: String) -> R<Vec<OrchTask>> {
    let client = engine.client("local")?;
    client
        .rpc("orch_tasks", serde_json::json!({ "root": root }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_cancel(engine: Engine<'_>, root: String) -> R<u32> {
    let client = engine.client("local")?;
    client
        .rpc("orch_cancel", serde_json::json!({ "root": root }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_retry(engine: Engine<'_>, task_id: String) -> R<bool> {
    let client = engine.client("local")?;
    client
        .rpc("orch_retry", serde_json::json!({ "task_id": task_id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_answer_block(engine: Engine<'_>, task_id: String, answer: String) -> R<bool> {
    let client = engine.client("local")?;
    client
        .rpc(
            "orch_answer_block",
            serde_json::json!({ "task_id": task_id, "answer": answer }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn orch_steer(engine: Engine<'_>, session_pk: String, text: String) -> R<String> {
    let client = engine.client("local")?;
    client
        .rpc(
            "orch_steer",
            serde_json::json!({ "session_pk": session_pk, "text": text }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn start_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    primary_agent_id: String,
    turn: TurnInput,
) -> R<Session> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "start_session",
            serde_json::json!({
                "project_id": project_id,
                "primary_agent_id": primary_agent_id,
                "turn": turn,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn start_chat_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    primary_agent_id: String,
    turn: TurnInput,
) -> R<Session> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "start_chat_session",
            serde_json::json!({ "primary_agent_id": primary_agent_id, "turn": turn }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn continue_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    turn: TurnInput,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "continue_session",
            serde_json::json!({ "session_pk": session_pk, "turn": turn }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_agent_sessions(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
    limit: u32,
) -> R<Vec<Session>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_agent_sessions",
            serde_json::json!({ "agent_id": agent_id, "limit": limit }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn steer_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    text: String,
) -> R<bool> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "steer",
            serde_json::json!({ "session_pk": session_pk, "text": text }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn stop_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "stop_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn end_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "end_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_tool_policies(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ToolPolicyRow>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_tool_policies", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_tool_policy(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    tool: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "delete_tool_policy",
            serde_json::json!({ "project_id": project_id, "tool": tool }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub fn resolve_approval(
    engine: Engine<'_>,
    runner_id: Option<String>,
    request_id: String,
    response: ApprovalResponse,
) -> bool {
    // Tauri requires async commands that take a reference input (`State<'_,
    // _>` included) to return `Result` — `bool` doesn't qualify, so this stays
    // sync (bindings-stable: specta emits `Promise<boolean>` either way) and
    // bridges into the async engine call via `block_in_place` + `block_on`,
    // which is safe here because command handlers already run on a blocking
    // thread of the tauri async runtime. The runner-client resolution happens
    // up front (sync, no I/O) — an unknown runner_id resolves to `false`, the
    // same "not resolved" signal `EngineClient::resolve_approval` itself
    // returns on any transport error.
    let Ok(client) = engine.client(runner_id.as_deref().unwrap_or("local")) else {
        return false;
    };
    tokio::task::block_in_place(|| {
        tauri::async_runtime::block_on(client.resolve_approval(&request_id, response))
    })
}

#[tauri::command]
#[specta::specta]
pub async fn list_messages(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
) -> R<Vec<Message>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "list_messages",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

/// Write pasted bytes into the attachments staging area and return the
/// absolute path — from there the file flows through the normal attachment
/// pipeline on send. Staging is wiped on app start (see lib.rs setup).
#[tauri::command]
#[specta::specta]
pub async fn stage_attachment(
    engine: Engine<'_>,
    runner_id: Option<String>,
    name: String,
    data_base64: String,
) -> R<String> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "stage_attachment",
            serde_json::json!({ "name": name, "data_base64": data_base64 }),
        )
        .await
}

/// Read a media file as base64 for composer thumbnails: paths here are
/// CLIENT-LOCAL (files the user picked/dropped to attach, staged via
/// `stage_attachment`), never the session workdir — those arbitrary user
/// paths sit outside the asset-protocol scope, so previews go through this
/// instead. This reads THIS machine's disk unconditionally, which is correct
/// even for a remote session: the attachment lives on the user's machine
/// until it's uploaded. Session-workdir file reads (the file viewer) go
/// through the jailed, size-capped `fsview::read_file`/`read_file_base64`
/// RPCs instead — see `fsview_cmd.rs`.
#[tauri::command]
#[specta::specta]
pub async fn read_local_media(path: String) -> R<MediaFile> {
    use base64::Engine as _;
    let meta = tokio::fs::metadata(&path).await?;
    if meta.len() > MAX_MEDIA_READ_BYTES {
        return Err(CmdError {
            message: format!("file too large ({} bytes)", meta.len()),
        });
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok(MediaFile {
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        content_type: content_type_for_path(Path::new(&path)),
    })
}

/// Base64-encoded read of one saved attachment, proxied through the engine's
/// authed `GET /attachments/{rel}` route (`EngineClient::get_attachment_bytes`)
/// — remote-safe: the bytes are read on the RUNNER's disk (local or a
/// pinned-TLS remote), unlike `read_local_media` above (which is correctly
/// always-local, since composer previews are of files still on the user's
/// own machine). `rel` is the `RowAttachment.rel` the transcript row carries
/// (or the caller's `sessionPk + basename(path)` fallback for pre-P4-3 rows
/// with no `rel` recorded).
#[tauri::command]
#[specta::specta]
pub async fn fetch_attachment(
    engine: Engine<'_>,
    runner_id: Option<String>,
    rel: String,
) -> R<MediaFile> {
    use base64::Engine as _;
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    let (bytes, content_type) = client.get_attachment_bytes(&rel).await?;
    Ok(MediaFile {
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        content_type: content_type.or_else(|| content_type_for_path(Path::new(&rel))),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn pick_directory(app: tauri::AppHandle) -> Option<String> {
    tokio::task::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .ok()
        .flatten()
        .map(|p| p.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn pick_files(app: tauri::AppHandle) -> Vec<String> {
    tokio::task::spawn_blocking(move || app.dialog().file().blocking_pick_files())
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string())
        .collect()
}

#[tauri::command]
#[specta::specta]
pub fn backdrop_capability(
    state: State<'_, crate::backdrop::BackdropState>,
) -> crate::backdrop::BackdropCapability {
    state.0
}
