use crate::error::CmdError;
use ryuzi_core::branches::BranchList;
use ryuzi_core::domain::AttachmentRef;
use ryuzi_core::{
    ControlPlane, Message, PermMode, Project, Session, SessionGitOptions, TurnPrompt,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

type R<T> = Result<T, CmdError>;

#[tauri::command]
#[specta::specta]
pub async fn get_setting(cp: State<'_, Arc<ControlPlane>>, key: String) -> R<Option<String>> {
    Ok(cp.store().get_setting(&key).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_setting(cp: State<'_, Arc<ControlPlane>>, key: String, value: String) -> R<()> {
    Ok(cp.store().set_setting(&key, &value).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_project(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
    model: Option<String>,
    perm_mode: PermMode,
    harness: String,
) -> R<Project> {
    cp.store()
        .update_project(&project_id, model, perm_mode, &harness)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown project: {project_id}"),
        })
}

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
pub async fn list_branches(cp: State<'_, Arc<ControlPlane>>, project_id: String) -> R<BranchList> {
    let project = cp
        .store()
        .get_project(&project_id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown project: {project_id}"),
        })?;
    // git2 is blocking; keep it off the async runtime's worker thread.
    let list = tokio::task::spawn_blocking(move || {
        ryuzi_core::branches::list_branches(Path::new(&project.workdir))
    })
    .await
    .map_err(|e| CmdError {
        message: format!("list_branches task failed: {e}"),
    })??;
    Ok(list)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChatContextArg {
    pub branch: Option<String>,
    pub voice_transcript: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
}

/// Per-start git controls from the composer (behavior matrix, workstream B).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct GitOptions {
    pub use_worktree: bool,
    pub create_branch: bool,
    pub branch_name: Option<String>,
    pub base_branch: Option<String>,
}

impl From<GitOptions> for SessionGitOptions {
    fn from(g: GitOptions) -> Self {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        SessionGitOptions {
            use_worktree: g.use_worktree,
            create_branch: g.create_branch,
            branch_name: clean(g.branch_name),
            base_branch: clean(g.base_branch),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequestOptions {
    pub runtime_id: Option<String>,
    pub model: Option<String>,
    pub context: Option<ChatContextArg>,
    #[serde(default)]
    pub attachments: Vec<String>,
    /// None => engine default (worktree ON, new engine-named branch from HEAD).
    pub git: Option<GitOptions>,
}

/// Ryuzi-only sessions: every runtime id resolves to the native harness.
/// Legacy ids ("claude", "native") and anything else are accepted so old
/// frontends or queued payloads can never error here; the Result shape is
/// kept so call sites stay `?`-compatible.
fn harness_for_runtime(_runtime_id: &str) -> Result<&'static str, CmdError> {
    Ok("native")
}

async fn apply_runtime_choice(
    cp: &ControlPlane,
    project_id: &str,
    runtime_id: Option<&str>,
    model: Option<&str>,
) -> R<()> {
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
        return Err(CmdError {
            message: format!("unknown project: {project_id}"),
        });
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

fn chat_agent_prompt(prompt: &str, context: Option<&ChatContextArg>) -> String {
    let Some(context) = context else {
        return prompt.to_string();
    };
    let mut lines = Vec::new();
    if let Some(branch) = context
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("- Branch: {branch}"));
    }
    if let Some(voice) = context
        .voice_transcript
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("- Voice transcript: {voice}"));
    }
    for reference in context
        .references
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("- Referenced file: {reference}"));
    }
    if lines.is_empty() {
        prompt.to_string()
    } else if prompt.trim().is_empty() {
        format!("[Chat context]\n{}", lines.join("\n"))
    } else {
        format!("{prompt}\n\n[Chat context]\n{}", lines.join("\n"))
    }
}

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

async fn attachment_refs_from_paths(paths: &[String]) -> R<Vec<AttachmentRef>> {
    let mut out = Vec::new();
    for raw in paths {
        if raw.trim().is_empty() {
            continue;
        }
        let path = tokio::fs::canonicalize(raw).await?;
        let meta = tokio::fs::metadata(&path).await?;
        if !meta.is_file() {
            return Err(CmdError {
                message: format!("attachment is not a file: {}", path.display()),
            });
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        out.push(AttachmentRef {
            name,
            url: ryuzi_core::attachments::file_url_for_path(&path)?.to_string(),
            content_type: content_type_for_path(&path),
            size: meta.len(),
        });
    }
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn start_session(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<Session> {
    let options = options.unwrap_or_default();
    apply_runtime_choice(
        &cp,
        &project_id,
        options.runtime_id.as_deref(),
        options.model.as_deref(),
    )
    .await?;
    let git: Option<SessionGitOptions> = options.git.clone().map(Into::into);
    let attachments = attachment_refs_from_paths(&options.attachments).await?;
    let agent_prompt = chat_agent_prompt(&prompt, options.context.as_ref());
    // `.inner()` -> &Arc<ControlPlane>: start/continue_session take `self: &Arc<Self>`.
    Ok(cp
        .inner()
        .start_session_with_prompt(
            &project_id,
            TurnPrompt::text(agent_prompt, prompt),
            "cockpit",
            &attachments,
            git,
        )
        .await?)
}

#[tauri::command]
#[specta::specta]
pub async fn continue_session(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
    prompt: String,
    options: Option<ChatRequestOptions>,
) -> R<()> {
    let options = options.unwrap_or_default();
    let attachments = attachment_refs_from_paths(&options.attachments).await?;
    let agent_prompt = chat_agent_prompt(&prompt, options.context.as_ref());
    // `.inner()` -> &Arc<ControlPlane>: start/continue_session take `self: &Arc<Self>`.
    Ok(cp
        .inner()
        .continue_session_with_prompt(
            &session_pk,
            TurnPrompt::text(agent_prompt, prompt),
            &attachments,
        )
        .await?)
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

/// Largest pasted attachment accepted from the webview (decoded size).
const MAX_STAGE_BYTES: usize = 25 * 1024 * 1024;
/// Largest media file inlined as a composer preview.
const MAX_MEDIA_READ_BYTES: u64 = 8 * 1024 * 1024;

/// Keep only the final path segment and strip characters unsafe in a file name.
fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Write pasted bytes into the attachments staging area and return the
/// absolute path — from there the file flows through the normal attachment
/// pipeline on send. Staging is wiped on app start (see lib.rs setup).
#[tauri::command]
#[specta::specta]
pub async fn stage_attachment(
    cp: State<'_, Arc<ControlPlane>>,
    name: String,
    data_base64: String,
) -> R<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| CmdError {
            message: format!("invalid attachment data: {e}"),
        })?;
    if bytes.len() > MAX_STAGE_BYTES {
        return Err(CmdError {
            message: format!("attachment too large ({} bytes)", bytes.len()),
        });
    }
    let dir = cp
        .attachments_root()
        .await
        .join("staging")
        .join(ryuzi_core::paths::new_id());
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(sanitize_file_name(&name));
    tokio::fs::write(&path, &bytes).await?;
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
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
    fn harness_for_runtime_always_resolves_native() {
        // Ryuzi-only sessions: any id — current, legacy, or unknown —
        // resolves to the native harness instead of erroring.
        assert_eq!(harness_for_runtime("native").unwrap(), "native");
        assert_eq!(harness_for_runtime("claude").unwrap(), "native");
        assert_eq!(harness_for_runtime("codex").unwrap(), "native");
        assert_eq!(harness_for_runtime("anything-legacy").unwrap(), "native");
    }

    #[tokio::test]
    async fn apply_runtime_choice_keeps_the_pinned_model_when_none_is_sent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        let cp = ryuzi_core::ControlPlane::new(store, ryuzi_core::Registries::new()).await;
        cp.store()
            .insert_project(Project {
                project_id: "p1".into(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                harness: "claude-code".into(),
                model: Some("openrouter/qwen3:free".into()),
                effort: None,
                perm_mode: PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();

        // The composer always sends runtimeId "native"; model may be null.
        apply_runtime_choice(&cp, "p1", Some("native"), None)
            .await
            .unwrap();

        let got = cp.store().get_project("p1").await.unwrap().unwrap();
        assert_eq!(
            got.harness, "native",
            "legacy harness migrates on next start"
        );
        assert_eq!(
            got.model.as_deref(),
            Some("openrouter/qwen3:free"),
            "model:null must not clear the pinned model"
        );
    }

    #[test]
    fn chat_request_options_deserializes_model() {
        let opts: ChatRequestOptions =
            serde_json::from_value(serde_json::json!({"runtimeId": "native", "model": "fable"}))
                .unwrap();
        assert_eq!(opts.runtime_id.as_deref(), Some("native"));
        assert_eq!(opts.model.as_deref(), Some("fable"));
    }

    #[test]
    fn chat_agent_prompt_appends_context_without_changing_display_text() {
        let out = chat_agent_prompt(
            "/review auth",
            Some(&ChatContextArg {
                branch: Some("feature/auth".into()),
                voice_transcript: Some("review the auth changes".into()),
                references: vec![],
            }),
        );
        assert!(out.starts_with("/review auth\n\n[Chat context]"));
        assert!(out.contains("- Branch: feature/auth"));
        assert!(out.contains("- Voice transcript: review the auth changes"));
    }

    #[test]
    fn chat_agent_prompt_appends_referenced_files_from_context_mentions() {
        let out = chat_agent_prompt(
            "explain this",
            Some(&ChatContextArg {
                references: vec!["src/main.rs".into(), "crates/core/src/lib.rs".into()],
                ..Default::default()
            }),
        );
        assert!(out.contains("- Referenced file: src/main.rs"));
        assert!(out.contains("- Referenced file: crates/core/src/lib.rs"));
    }

    #[test]
    fn chat_request_options_git_defaults_to_none_and_deserializes() {
        // Old payloads (no `git` key) keep parsing.
        let opts: ChatRequestOptions =
            serde_json::from_value(serde_json::json!({"runtimeId": "native", "model": "fable"}))
                .unwrap();
        assert!(opts.git.is_none());

        let opts: ChatRequestOptions = serde_json::from_value(serde_json::json!({
            "git": {
                "useWorktree": false,
                "createBranch": true,
                "branchName": "feat/x",
                "baseBranch": null
            }
        }))
        .unwrap();
        let git = opts.git.unwrap();
        assert!(!git.use_worktree);
        assert!(git.create_branch);
        assert_eq!(git.branch_name.as_deref(), Some("feat/x"));
        assert_eq!(git.base_branch, None);
    }

    #[test]
    fn git_options_convert_to_session_git_options_trimming_blanks() {
        let core: ryuzi_core::SessionGitOptions = GitOptions {
            use_worktree: true,
            create_branch: false,
            branch_name: Some("   ".into()),
            base_branch: Some(" develop ".into()),
        }
        .into();
        assert!(core.use_worktree);
        assert!(!core.create_branch);
        assert_eq!(core.branch_name, None, "blank names collapse to None");
        assert_eq!(core.base_branch.as_deref(), Some("develop"));
    }

    #[test]
    fn sanitize_file_name_strips_directories_and_unsafe_chars() {
        assert_eq!(sanitize_file_name("shot.png"), "shot.png");
        // rsplit keeps only the last path segment — traversal collapses away.
        assert_eq!(sanitize_file_name("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name("a/b/c.png"), "c.png");
        assert_eq!(sanitize_file_name("we|ird?.png"), "weird.png");
        assert_eq!(sanitize_file_name("   "), "file");
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
