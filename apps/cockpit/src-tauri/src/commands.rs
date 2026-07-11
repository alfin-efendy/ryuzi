use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::branches::BranchList;
use ryuzi_core::domain::{ApprovalResponse, ToolPolicyRow};
use ryuzi_core::{Message, PermMode, Project, Session};
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

// Cockpit's DTOs for these now live in `ryuzi_core::api::types`; only
// `ChatRequestOptions` is referenced by name here (as a command param), but
// specta's TS generation walks the type graph from every collected command,
// so `ChatContextArg`/`GitOptions` are still emitted to `bindings.ts` as
// fields of `ChatRequestOptions` without needing a local import.
pub use ryuzi_core::api::types::ChatRequestOptions;

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
pub async fn start_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    project_id: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<Session> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "start_session",
            serde_json::json!({
                "project_id": project_id, "prompt": prompt, "options": options,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn start_chat_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<Session> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "start_chat_session",
            serde_json::json!({ "prompt": prompt, "options": options }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn continue_session(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "continue_session",
            serde_json::json!({
                "session_pk": session_pk, "prompt": prompt, "options": options,
            }),
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

/// Largest file the viewer will load into memory.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024; // 2 MB cap

/// Reject reads past the viewer's size cap before touching file contents; the
/// error carries the offending size.
fn check_read_size(len: u64) -> Result<(), CmdError> {
    if len > MAX_READ_BYTES {
        return Err(CmdError {
            message: format!("file too large ({len} bytes)"),
        });
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn read_file(path: String) -> R<String> {
    let meta = tokio::fs::metadata(&path).await?;
    check_read_size(meta.len())?;
    Ok(tokio::fs::read_to_string(&path).await?)
}

/// Largest media file inlined as a composer preview.
const MAX_MEDIA_READ_BYTES: u64 = 8 * 1024 * 1024;

fn content_type_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "rs" | "ts" | "tsx" | "js" | "jsx" | "json" | "toml" | "yaml" | "yml" => {
            Some("text/plain".to_string())
        }
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "zip" => Some("application/zip".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "webm" => Some("video/webm".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "mkv" => Some("video/x-matroska".to_string()),
        "mp3" => Some("audio/mpeg".to_string()),
        "wav" => Some("audio/wav".to_string()),
        "ogg" => Some("audio/ogg".to_string()),
        "m4a" => Some("audio/mp4".to_string()),
        "flac" => Some("audio/flac".to_string()),
        _ => None,
    }
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    pub data_base64: String,
    pub content_type: Option<String>,
}

/// Read a media file as base64 for composer thumbnails (arbitrary user paths
/// sit outside the asset-protocol scope, so previews go through this instead).
#[tauri::command]
#[specta::specta]
pub async fn read_file_base64(path: String) -> R<MediaFile> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_up_to_the_cap_pass() {
        assert!(check_read_size(0).is_ok());
        assert!(check_read_size(MAX_READ_BYTES).is_ok());
    }

    #[test]
    fn sizes_over_the_cap_are_rejected_with_the_size() {
        let err = check_read_size(MAX_READ_BYTES + 1).unwrap_err();
        assert_eq!(err.message, "file too large (2097153 bytes)");
    }

    #[test]
    fn media_content_types_cover_video_and_audio() {
        let ct = |p: &str| content_type_for_path(Path::new(p));
        assert_eq!(ct("a.webp").as_deref(), Some("image/webp"));
        assert_eq!(ct("a.mp4").as_deref(), Some("video/mp4"));
        assert_eq!(ct("a.mp3").as_deref(), Some("audio/mpeg"));
        assert_eq!(ct("a.wav").as_deref(), Some("audio/wav"));
    }
}
