use crate::engine::EngineClient;
use crate::error::CmdError;
use ryuzi_core::branches::BranchList;
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
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn get_setting(engine: Engine<'_>, key: String) -> R<Option<String>> {
    engine
        .rpc("get_setting", serde_json::json!({ "key": key }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_setting(engine: Engine<'_>, key: String, value: String) -> R<()> {
    engine
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
    project_id: String,
    model: Option<String>,
    perm_mode: PermMode,
    harness: String,
) -> R<Project> {
    engine
        .rpc(
            "update_project",
            serde_json::json!({
                "project_id": project_id, "model": model,
                "perm_mode": perm_mode, "harness": harness,
            }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_projects(engine: Engine<'_>) -> R<Vec<Project>> {
    engine.rpc("list_projects", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_sessions(engine: Engine<'_>, project_id: Option<String>) -> R<Vec<Session>> {
    engine
        .rpc(
            "list_sessions",
            serde_json::json!({ "project_id": project_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn connect_project(engine: Engine<'_>, workdir: String, name: String) -> R<Project> {
    engine
        .rpc(
            "connect_project",
            serde_json::json!({ "workdir": workdir, "name": name }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn clone_project(engine: Engine<'_>, url: String, dest_parent: String) -> R<Project> {
    engine
        .rpc(
            "clone_project",
            serde_json::json!({ "url": url, "dest_parent": dest_parent }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_branches(engine: Engine<'_>, project_id: String) -> R<BranchList> {
    engine
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
    project_id: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<Session> {
    engine
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
pub async fn continue_session(
    engine: Engine<'_>,
    session_pk: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<()> {
    engine
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
pub async fn stop_session(engine: Engine<'_>, session_pk: String) -> R<()> {
    engine
        .rpc(
            "stop_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn end_session(engine: Engine<'_>, session_pk: String) -> R<()> {
    engine
        .rpc(
            "end_session",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub fn resolve_approval(engine: Engine<'_>, request_id: String, allow: bool) -> bool {
    // Tauri requires async commands that take a reference input (`State<'_,
    // _>` included) to return `Result` — `bool` doesn't qualify, so this stays
    // sync (bindings-stable: specta emits `Promise<boolean>` either way) and
    // bridges into the async engine call via `block_in_place` + `block_on`,
    // which is safe here because command handlers already run on a blocking
    // thread of the tauri async runtime.
    tokio::task::block_in_place(|| {
        tauri::async_runtime::block_on(engine.resolve_approval(&request_id, allow))
    })
}

#[tauri::command]
#[specta::specta]
pub async fn list_messages(engine: Engine<'_>, session_pk: String) -> R<Vec<Message>> {
    engine
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
pub async fn stage_attachment(engine: Engine<'_>, name: String, data_base64: String) -> R<String> {
    engine
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
