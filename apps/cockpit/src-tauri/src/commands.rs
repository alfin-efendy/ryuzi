use crate::error::CmdError;
use ryuzi_core::{ControlPlane, Message, Project, Session};
use std::sync::Arc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

type R<T> = Result<T, CmdError>;

#[tauri::command]
#[specta::specta]
pub async fn list_projects(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<Project>> {
    Ok(cp.list_projects().await?)
}

#[tauri::command]
#[specta::specta]
pub async fn list_sessions(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: Option<String>,
) -> R<Vec<Session>> {
    Ok(cp.list_sessions(project_id.as_deref()).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn connect_project(
    cp: State<'_, Arc<ControlPlane>>,
    workdir: String,
    name: String,
) -> R<Project> {
    Ok(cp
        .connect_project(std::path::Path::new(&workdir), &name)
        .await?)
}

#[tauri::command]
#[specta::specta]
pub async fn start_session(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
    prompt: String,
) -> R<Session> {
    // `.inner()` -> &Arc<ControlPlane>: start/continue_session take `self: &Arc<Self>`.
    Ok(cp.inner().start_session(&project_id, &prompt).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn continue_session(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
    prompt: String,
) -> R<()> {
    // `.inner()` -> &Arc<ControlPlane>: start/continue_session take `self: &Arc<Self>`.
    Ok(cp.inner().continue_session(&session_pk, &prompt).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_session(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<()> {
    Ok(cp.stop_session(&session_pk).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn end_session(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<()> {
    Ok(cp.end_session(&session_pk).await?)
}

#[tauri::command]
#[specta::specta]
pub fn resolve_approval(cp: State<'_, Arc<ControlPlane>>, request_id: String, allow: bool) -> bool {
    cp.resolve_approval(&request_id, allow)
}

#[tauri::command]
#[specta::specta]
pub async fn read_file(path: String) -> R<String> {
    const MAX: u64 = 2 * 1024 * 1024; // 2 MB cap
    let meta = tokio::fs::metadata(&path).await?;
    if meta.len() > MAX {
        return Err(CmdError {
            message: format!("file too large ({} bytes)", meta.len()),
        });
    }
    Ok(tokio::fs::read_to_string(&path).await?)
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
pub async fn list_messages(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
) -> R<Vec<Message>> {
    Ok(cp.list_messages(&session_pk).await?)
}

#[tauri::command]
#[specta::specta]
pub fn backdrop_capability(
    state: State<'_, crate::backdrop::BackdropState>,
) -> crate::backdrop::BackdropCapability {
    state.0
}
