//! Shared request/response shapes for the RPC command families under
//! `api/`. Populated by each command family as it moves its DTOs and
//! private helpers out of the src-tauri `commands.rs` module (see the Move
//! Recipe) — bindings-stable, so every serde/specta attribute here must stay
//! byte-identical to the source it was moved from.

use super::ApiError;
use crate::domain::SessionGitOptions;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;

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
pub(crate) fn harness_for_runtime(_runtime_id: &str) -> Result<&'static str, ApiError> {
    Ok("native")
}

pub(crate) fn chat_agent_prompt(prompt: &str, context: Option<&ChatContextArg>) -> String {
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

pub(crate) fn content_type_for_path(path: &Path) -> Option<String> {
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

/// Keep only the final path segment and strip characters unsafe in a file name.
pub(crate) fn sanitize_file_name(name: &str) -> String {
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

// --- scheduler_api (moved verbatim from apps/cockpit/src-tauri/src/scheduler_cmd.rs) ---

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub started_at_ms: i64,
    pub duration_ms: Option<i64>,
    pub add_lines: Option<i64>,
    pub del_lines: Option<i64>,
    pub note: Option<String>,
    pub error: Option<String>,
    pub session_pk: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub mode: String,
    pub natural: String,
    pub project_id: String,
    pub project_name: String,
    pub branch: String,
    pub agent: String,
    pub gateway: String,
    pub enabled: bool,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
    pub next_run_ms: Option<i64>,
    pub history: Vec<RunInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobInput {
    pub name: String,
    pub mode: String,
    pub natural: String,
    pub cron: String,
    pub project_id: String,
    pub branch: String,
    pub agent: String,
    pub gateway: String,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
}

// --- gateways_api (moved verbatim from apps/cockpit/src-tauri/src/gateways_cmd.rs) ---

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResourceInfo {
    pub label: String,
    pub sub: String,
    pub pct: u32,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInfo {
    pub id: String,
    pub name: String,
    pub badge: String,
    /// local | wsl | ssh
    pub kind: String,
    pub detail: String,
    pub meta_line: String,
    /// connected | offline
    pub status: String,
    pub latency: Option<String>,
    pub daemon_version: String,
    pub uptime: Option<String>,
    pub last_seen_ms: Option<i64>,
    pub resources: Vec<GatewayResourceInfo>,
    pub fingerprint: Option<String>,
    pub fs_mode: String,
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEventInfo {
    pub at: i64,
    pub level: String,
    pub text: String,
}

// --- apps_api (moved verbatim from apps/cockpit/src-tauri/src/apps_cmd.rs) ---

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub desc: String,
    pub perm: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccessInfo {
    pub agent_id: String,
    pub allowed: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub initial: String,
    pub color: String,
    pub desc: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub scope: String,
    pub scope_gateways: Vec<String>,
    pub status: String,
    pub status_detail: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub auth_kind: String,
    pub auth_detail: Option<String>,
    pub tools: Vec<ToolInfo>,
    pub agent_access: Vec<AgentAccessInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AddAppInput {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    pub kind: Option<String>,
    /// stdio | http
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// KEY=VALUE pairs.
    pub env: Vec<String>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub color: Option<String>,
}

// --- native_api (moved verbatim from apps/cockpit/src-tauri/src/native_cmd.rs) ---

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub mode: String,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}
