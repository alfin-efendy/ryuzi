//! Shared request/response shapes for the RPC command families under
//! `api/`. Populated by each command family as it moves its DTOs and
//! private helpers out of the src-tauri `commands.rs` module (see the Move
//! Recipe) — bindings-stable, so every serde/specta attribute here must stay
//! byte-identical to the source it was moved from.

use crate::domain::SessionGitOptions;
use crate::harness::native::commands::{ProjectCommandInput, ProjectCommandRead};
use crate::llm_router::model_effort::{
    EffectiveEffortSource, SelectableModelInfo, StoredEffortStatus,
};
use crate::llm_router::quota::ProviderQuotaCapability;
use crate::llm_router::secrets::KeychainStatus;
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
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context: Option<ChatContextArg>,
    #[serde(default)]
    pub attachments: Vec<String>,
    /// None => engine default (worktree ON, new engine-named branch from HEAD).
    pub git: Option<GitOptions>,
    /// Initial permission mode for a legacy/composer session.
    pub perm_mode: Option<crate::domain::PermMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMention {
    pub agent_id: String,
    pub label_snapshot: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TurnInput {
    pub text: String,
    #[serde(default)]
    pub mentions: Vec<AgentMention>,
    pub context: Option<ChatContextArg>,
    #[serde(default)]
    pub attachments: Vec<String>,
    pub git: Option<GitOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessageInfo {
    pub id: String,
    pub text: String,
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
    pub gateway: String,
    pub enabled: bool,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
    pub next_run_ms: Option<i64>,
    pub history: Vec<RunInfo>,
    /// Model id this job's session starts with, overriding the project/agent
    /// default. `None` when the job uses ordinary model resolution. Not yet
    /// editable from the scheduler panel — set programmatically today (e.g.
    /// by a future `app_jobs` tool); surfaced here so a later job editor can
    /// read and round-trip it without another DTO change.
    #[serde(default)]
    pub model_override: Option<String>,
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
    pub gateway: String,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
    /// See `JobInfo::model_override`.
    #[serde(default)]
    pub model_override: Option<String>,
}

// --- automation_api (Hook persistence contract; RPC wiring follows in Task 5) ---

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationAgentRunActionInput {
    pub project_id: String,
    pub branch: String,
    pub gateway_id: String,
    pub prompt: String,
    pub agent_id: Option<String>,
    pub model_override: Option<String>,
    pub subtask: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationWebhookHeaderInput {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationWebhookOutboundActionInput {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: Vec<AutomationWebhookHeaderInput>,
    pub payload_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "config", deny_unknown_fields)]
pub enum AutomationActionInput {
    #[serde(rename = "agent.run")]
    AgentRun(AutomationAgentRunActionInput),
    #[serde(rename = "webhook.outbound")]
    WebhookOutbound(AutomationWebhookOutboundActionInput),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationHookInput {
    pub name: String,
    pub trigger_kind: crate::automation::TriggerKind,
    pub action: AutomationActionInput,
    #[serde(default = "automation_enabled_by_default")]
    pub enabled: bool,
}

const fn automation_enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationHookInfo {
    pub id: String,
    pub name: String,
    pub trigger_kind: crate::automation::TriggerKind,
    pub action_kind: crate::automation::ActionKind,
    pub enabled: bool,
    pub inbound_path: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationHookRunInfo {
    pub id: String,
    pub hook_id: String,
    pub status: String,
    pub session_pk: Option<String>,
    pub error: Option<String>,
    pub attempt_count: i64,
    pub last_http_status: Option<i64>,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub attempts: Vec<AutomationHookAttemptInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationHookAttemptInfo {
    pub run_id: String,
    pub ordinal: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub http_status: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationWebhookHeaderInfo {
    pub name: String,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationWebhookOutboundActionInfo {
    pub url: String,
    pub method: String,
    pub headers: Vec<AutomationWebhookHeaderInfo>,
    pub payload_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "config")]
pub enum AutomationActionInfo {
    #[serde(rename = "agent.run")]
    AgentRun(AutomationAgentRunActionInput),
    #[serde(rename = "webhook.outbound")]
    WebhookOutbound(AutomationWebhookOutboundActionInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AutomationHookDetail {
    pub hook: AutomationHookInfo,
    pub action: AutomationActionInfo,
    pub runs: Vec<AutomationHookRunInfo>,
}

impl From<AutomationAgentRunActionInput> for crate::automation::AgentRunAction {
    fn from(value: AutomationAgentRunActionInput) -> Self {
        Self {
            project_id: value.project_id,
            branch: value.branch,
            gateway_id: value.gateway_id,
            prompt: value.prompt,
            agent_id: value.agent_id,
            model_override: value.model_override,
            subtask: value.subtask,
        }
    }
}

impl From<AutomationWebhookHeaderInput> for crate::automation::WebhookHeader {
    fn from(value: AutomationWebhookHeaderInput) -> Self {
        Self {
            name: value.name,
            value: value.value,
        }
    }
}

impl From<AutomationWebhookOutboundActionInput> for crate::automation::WebhookOutboundAction {
    fn from(value: AutomationWebhookOutboundActionInput) -> Self {
        Self {
            url: value.url,
            method: value.method,
            headers: value.headers.into_iter().map(Into::into).collect(),
            payload_template: value.payload_template,
        }
    }
}

impl From<AutomationActionInput> for crate::automation::HookActionInput {
    fn from(value: AutomationActionInput) -> Self {
        match value {
            AutomationActionInput::AgentRun(config) => Self::AgentRun(config.into()),
            AutomationActionInput::WebhookOutbound(config) => Self::WebhookOutbound(config.into()),
        }
    }
}

impl From<AutomationHookInput> for crate::automation::HookInput {
    fn from(value: AutomationHookInput) -> Self {
        Self {
            name: value.name,
            trigger_kind: value.trigger_kind,
            action: value.action.into(),
            enabled: value.enabled,
        }
    }
}

impl From<crate::automation::WebhookHeader> for AutomationWebhookHeaderInfo {
    fn from(value: crate::automation::WebhookHeader) -> Self {
        Self {
            name: value.name,
            configured: true,
        }
    }
}

impl From<crate::automation::HookActionInput> for AutomationActionInfo {
    fn from(value: crate::automation::HookActionInput) -> Self {
        match value {
            crate::automation::HookActionInput::AgentRun(config) => {
                Self::AgentRun(AutomationAgentRunActionInput {
                    project_id: config.project_id,
                    branch: config.branch,
                    gateway_id: config.gateway_id,
                    prompt: config.prompt,
                    agent_id: config.agent_id,
                    model_override: config.model_override,
                    subtask: config.subtask,
                })
            }
            crate::automation::HookActionInput::WebhookOutbound(config) => {
                Self::WebhookOutbound(AutomationWebhookOutboundActionInfo {
                    url: config.url,
                    method: config.method,
                    headers: config.headers.into_iter().map(Into::into).collect(),
                    payload_template: config.payload_template,
                })
            }
        }
    }
}

impl From<crate::automation::HookAttemptRow> for AutomationHookAttemptInfo {
    fn from(value: crate::automation::HookAttemptRow) -> Self {
        Self {
            run_id: value.run_id,
            ordinal: value.ordinal,
            started_at: value.started_at,
            finished_at: value.finished_at,
            http_status: value.http_status,
            error: value.error,
        }
    }
}

impl From<crate::automation::HookRunRow> for AutomationHookRunInfo {
    fn from(value: crate::automation::HookRunRow) -> Self {
        Self {
            id: value.id,
            hook_id: value.hook_id,
            status: value.status,
            session_pk: value.session_pk,
            error: value.error,
            attempt_count: value.attempt_count,
            last_http_status: value.last_http_status,
            queued_at: value.queued_at,
            started_at: value.started_at,
            finished_at: value.finished_at,
            attempts: value
                .attempts
                .into_iter()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<crate::automation::HookDetail> for AutomationHookDetail {
    fn from(value: crate::automation::HookDetail) -> Self {
        let action = value.hook.action.clone().into();
        Self {
            hook: value.hook.into(),
            action,
            runs: value.runs.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<crate::automation::HookRow> for AutomationHookInfo {
    fn from(value: crate::automation::HookRow) -> Self {
        Self {
            id: value.id,
            name: value.name,
            trigger_kind: value.trigger_kind,
            action_kind: value.action_kind,
            enabled: value.enabled,
            inbound_path: value.inbound_path,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommandOriginInfo {
    Builtin,
    Global,
    Project,
}

const fn default_command_effective() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subtask: bool,
    pub origin: CommandOriginInfo,
    #[serde(default = "default_command_effective")]
    pub effective: bool,
    pub shadows_global: bool,
}

/// Editable fields for a project-owned slash command. The command name is
/// supplied separately for updates so a save cannot rename a file by accident.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectCommandMutationDto {
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub subtask: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectCommandInputDto {
    pub name: String,
    #[serde(flatten)]
    pub command: ProjectCommandMutationDto,
}

impl ProjectCommandMutationDto {
    pub fn with_name(self, name: &str) -> ProjectCommandInput {
        ProjectCommandInput {
            name: name.to_string(),
            description: self.description,
            template: self.template,
            agent: self.agent,
            model: self.model,
            subtask: self.subtask,
        }
    }
}

impl From<ProjectCommandInputDto> for ProjectCommandInput {
    fn from(value: ProjectCommandInputDto) -> Self {
        value.command.with_name(&value.name)
    }
}

/// A project command and the revision that must accompany update or delete.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommandInfo {
    pub name: String,
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: bool,
    pub revision: String,
}

impl From<ProjectCommandRead> for ProjectCommandInfo {
    fn from(value: ProjectCommandRead) -> Self {
        Self {
            name: value.name,
            description: value.description,
            template: value.template,
            agent: value.agent,
            model: value.model,
            subtask: value.subtask,
            revision: value.revision,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

// --- agent_api (moved verbatim from apps/cockpit/src-tauri/src/agent_cmd.rs) ---

// --- endpoint_api (moved verbatim from apps/cockpit/src-tauri/src/endpoint_cmd.rs) ---
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointStatusInfo {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub autostart: bool,
    pub keychain_status: KeychainStatus,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointKeyInfo {
    pub id: String,
    pub name: String,
    pub key: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub day: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageSeries {
    pub days: Vec<UsagePoint>,
    pub today_requests: i64,
    pub today_input_tokens: i64,
    pub today_output_tokens: i64,
}

// --- connections_api (moved verbatim from apps/cockpit/src-tauri/src/connections_cmd.rs) ---

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub id: String,
    pub provider: String,
    pub provider_name: String,
    pub color: String,
    pub initial: String,
    pub auth_type: String,
    pub label: String,
    pub priority: i32,
    pub enabled: bool,
    pub quota_capability: Option<ProviderQuotaCapability>,
    pub models: Vec<String>,
    /// OAuth connections only: true once refresh has failed terminally and
    /// the user needs to reconnect via the browser/paste flow again.
    pub needs_relogin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeInfo {
    pub session_pk: String,
    pub model: Option<String>,
    pub stored_effort: Option<String>,
    pub effective_effort: Option<String>,
    pub effective_effort_label: Option<String>,
    pub effective_source: EffectiveEffortSource,
    pub stored_effort_status: StoredEffortStatus,
    pub model_info: Option<SelectableModelInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    /// Legacy pass/fail, kept for existing call sites (connection-level
    /// test, toasts). Always derived: `status == "valid"`.
    pub ok: bool,
    /// Tri-state probe verdict: "valid" | "invalid" | "unknown".
    pub status: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RefreshModelsResult {
    pub connection_id: String,
    pub label: String,
    pub ok: bool,
    pub message: String,
}

/// One persisted probe verdict row for the provider Models card.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatusInfo {
    pub model: String,
    pub status: String,
    pub message: String,
    pub tested_at: i64,
}

/// One persisted probe verdict row across ALL families — hydrates the
/// app-wide model-status store consumed by every model picker.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatusEntry {
    pub family: String,
    pub model: String,
    pub status: String,
    pub message: String,
    pub tested_at: i64,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ManualStartInfo {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
}

/// Device-code flow info shown to the user while they complete the browser
/// step (Kiro): the short code to enter, the URL to visit, and the poll
/// cadence the frontend's `await_kiro_device_flow` call will honor.
// `Deserialize` (not just `Serialize`) is required: the engine serializes
// this as an RPC result, and Cockpit's `EngineClient` deserializes it back
// client-side to read `verification_uri_complete` before opening the
// system browser. A plain `//` comment (not `///`) so it isn't captured
// into the generated TS binding's doc comment.
#[derive(Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFlowInfo {
    pub flow_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

// --- plugins_api (moved verbatim from apps/cockpit/src-tauri/src/plugins_cmd.rs) ---

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    /// The exclusive capability slot this plugin's manifest claims (e.g.
    /// `"memory"`), mirroring `ryuzi_plugin_sdk::PluginManifest::slot`.
    /// `None` when the manifest declares no slot.
    pub slot: Option<String>,
    /// Whether this plugin currently WON its `slot` claim
    /// (first-registration-wins — see `crate::plugins::PluginHost::
    /// slot_owner`). Always `false` when `slot` is `None`. A plugin whose
    /// claim lost still has `slot` set (its own manifest is unaffected) but
    /// `owns_slot: false`; see `plugin_doctor`'s `"slot-conflict"` finding
    /// for the observable signal naming both the winner and the loser.
    pub owns_slot: bool,
    pub verified: bool,
    pub experimental: bool,
    pub enabled: bool,
    /// Same semantics as `PluginAuthInfo.configured` (oauth: token stored &&
    /// !reconnect_required; else a persisted `auth.setting` row or `auth.env`
    /// set). `false` when the manifest declares no `[auth]` block. On the
    /// LIST payload (not just `plugin_detail`) because the Browse grid's
    /// Install/Open split needs it — note this adds per-plugin store lookups
    /// to list assembly.
    pub configured: bool,
    /// `builtin` | `catalog` | `skill-pack`.
    pub source: String,
    /// Any of `provider` | `runtime` | `gateway` | `connector`.
    pub capabilities: Vec<String>,
    /// `integration` | `provider` | `gateway` | `skill-pack`. There is no
    /// `runtime` kind: the native agent is built-in engine behavior, not an
    /// installable/listed plugin, so it never appears in this payload.
    pub kind: String,
    /// Kind-specific "already set up" flag: integration = configured ||
    /// enabled; provider = ≥1 connection in the provider's family; gateway =
    /// all manifest settings present; skill-pack = installed on disk.
    pub installed: bool,
    /// Provider family head id (providers only) — the Models `providerDetail`
    /// navigation target. `None` for other kinds.
    pub family: Option<String>,
    /// Mirrors `crate::store::PluginInstallRecord.pinned` — `false` when the
    /// plugin has no `plugin_installs` ledger row (never installed via the
    /// tracked git-clone path, e.g. builtins/catalog integrations with no
    /// skill-pack install).
    pub pinned: bool,
    /// The ledger row's git origin (`PluginInstallRecord.source_spec`).
    /// Distinct from `source` (the stable builtin/catalog/skill-pack enum
    /// label) — the Provenance card in Cockpit renders it only when present.
    pub source_spec: Option<String>,
    pub resolved_commit: Option<String>,
    pub installed_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub trust_tier: Option<String>,
    /// `embedded` | `remote` — which catalog source won for this id.
    /// `None` for builtins and skill packs (never from either catalog).
    pub catalog_source: Option<String>,
    /// The remote catalog feed's `version` for this id, when a cached
    /// `plugin_catalog_cache` row matches. `None` when the id was never seen
    /// in a fetched feed.
    pub catalog_version: Option<String>,
    /// Set when the remote catalog's signed feed blocked (revoked) this id —
    /// mirrors `RemoteCatalogRow.blocked_reason`. `None` when not blocked.
    pub blocked_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthInfo {
    /// `none` | `api-key` | `token` | `oauth`.
    pub kind: String,
    pub setting: Option<String>,
    pub env: Option<String>,
    pub help_url: Option<String>,
    /// A persisted (non-empty) row exists for `setting`, OR `env` is set in
    /// the process environment. Never reveals the value itself.
    pub configured: bool,
    pub oauth_connect_available: bool,
    pub oauth_connect_error: Option<String>,
    pub oauth_token_stored: bool,
    pub oauth_reconnect_required: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginOauthBeginResult {
    pub state_token: String,
    pub authorize_url: String,
    pub redirect_uri: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallBeginResult {
    /// `none` | `api-key` | `token` | `oauth`.
    pub auth_kind: String,
    /// `auth.env` is declared AND set in the environment.
    pub env_var_present: bool,
    pub env_var_name: Option<String>,
    /// Endpoints + client id resolved; the browser flow started.
    pub oauth_available: bool,
    /// OAuth brokered outside Cockpit (kind=oauth, no `auth.resource`, no
    /// manifest `authorize_url` — google-workspace).
    pub oauth_external: bool,
    /// oauth, endpoints may be known, but no client id and DCR not
    /// applicable / failed.
    pub needs_client_id: bool,
    /// This call performed a successful registration.
    pub dcr_succeeded: bool,
    /// `auto` (callback server bound) | `manual` (bind failed → paste).
    pub callback_mode: String,
    pub oauth_begin: Option<PluginOauthBeginResult>,
    /// Discovery/DCR failure detail (shown on the manual client id form).
    pub dcr_error: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginFieldInfo {
    pub key: String,
    pub label: String,
    pub help: String,
    pub secret: bool,
    pub required: bool,
    /// A persisted (non-empty) row exists for `key`. Never the value itself.
    pub value_set: bool,
    /// `string` | `int` | `bool` — the value shape Cockpit renders (see
    /// `ryuzi_plugin_sdk::FieldKind`). A plain camelCase-friendly `String`
    /// mirror rather than the SDK enum itself, matching this module's
    /// existing convention (`auth_kind_label`/`mcp_transport_label`) of
    /// never crossing specta's `Type` boundary with an SDK type directly.
    pub kind: String,
    /// Non-empty makes this field an enum/choice — the value must be one of
    /// these members (see `ryuzi_plugin_sdk::SettingField::options`).
    pub options: Vec<String>,
    /// Pre-filled/effective value to show when `value_set` is `false`. Safe
    /// to return even for a `secret` field: it comes from the manifest, not
    /// a persisted credential.
    pub default: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginMcpInfo {
    pub name: String,
    /// `stdio` | `http`.
    pub transport: String,
    /// The raw manifest string (command for stdio, url for http) — no
    /// `${auth}` substitution, matching `ryuzi plugins info`'s output.
    pub command_or_url: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetail {
    pub info: PluginInfo,
    pub auth: Option<PluginAuthInfo>,
    pub settings: Vec<PluginFieldInfo>,
    pub mcp: Vec<PluginMcpInfo>,
    pub models: Vec<String>,
    pub homepage: Option<String>,
    pub publisher: String,
}

// --- Skill/plugin distribution DTOs (trust prompt, update, doctor) ---
//
// The DTOs below are thin camelCase mirrors of
// `crate::skills_install`'s `TrustPrompt`/`UpdateOutcome`/`BeginInstall` and
// `crate::plugins::doctor::DoctorFinding` — those core types derive
// `Serialize`/`Deserialize` but not specta's `Type`, so they cannot cross the
// Tauri IPC boundary directly (same rationale as `PluginInfo` mirroring
// `ryuzi_plugin_sdk::PluginManifest`). None of these add or drop any field
// relative to the core type, and `TrustPrompt` is already secret-free by
// construction (repo path, skill names, hook-script paths, byte count — no
// credential material).

/// Mirror of `crate::skills_install::TrustPrompt`. `total_bytes` stays a
/// `u64` (not narrowed to `u32`) to avoid silently truncating a large pack's
/// byte count — `export_bindings`'s `BigIntExportBehavior::Number` already
/// renders any bigint-sized field as a plain TS `number`, so there's no
/// bindings-shape cost to keeping the wider type.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrustPromptDto {
    pub token: String,
    pub source_spec: String,
    pub owner_repo: String,
    pub resolved_commit: Option<String>,
    pub skills: Vec<String>,
    pub hook_scripts: Vec<String>,
    pub total_bytes: u64,
    /// Mirrors `TrustPrompt::runs_code`: true when the staged manifest
    /// declares `[[extension]]` (code execution, Track D) — the wizard must
    /// show a distinct warning for this, not just fold it into the
    /// hook-script list.
    pub runs_code: bool,
    /// Mirrors `TrustPrompt::curated`: true when the source is one of the
    /// curated skill packs, so this prompt only exists because `runs_code`
    /// is true — the wizard uses this to avoid the misleading "this source
    /// isn't curated" framing for a curated-but-code-running install.
    pub curated: bool,
}

impl From<crate::skills_install::TrustPrompt> for TrustPromptDto {
    fn from(p: crate::skills_install::TrustPrompt) -> Self {
        TrustPromptDto {
            token: p.token,
            source_spec: p.source_spec,
            owner_repo: p.owner_repo,
            resolved_commit: p.resolved_commit,
            skills: p.skills,
            hook_scripts: p.hook_scripts,
            total_bytes: p.total_bytes,
            runs_code: p.runs_code,
            curated: p.curated,
        }
    }
}

/// Mirror of `crate::skills_install::BeginInstall`, flattened into a single
/// `{completed, trust?, plugin?}` shape the wizard can branch on without a
/// tagged-union match in TS. `trust` is set for `NeedsConfirmation`, `plugin`
/// for `Completed` — exactly one is ever `Some`.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallBegin {
    pub completed: bool,
    pub trust: Option<TrustPromptDto>,
    pub plugin: Option<crate::skills_install::InstalledSkillPack>,
}

impl From<crate::skills_install::BeginInstall> for SkillInstallBegin {
    fn from(result: crate::skills_install::BeginInstall) -> Self {
        match result {
            crate::skills_install::BeginInstall::Completed(pack) => SkillInstallBegin {
                completed: true,
                trust: None,
                plugin: Some(pack),
            },
            crate::skills_install::BeginInstall::NeedsConfirmation(prompt) => SkillInstallBegin {
                completed: false,
                trust: Some(TrustPromptDto::from(prompt)),
                plugin: None,
            },
        }
    }
}

/// Mirror of `crate::skills_install::UpdateOutcome`. Keeps the same
/// `#[serde(tag = "kind", content = "detail")]` shape so the discriminated
/// union round-trips identically to the core enum.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum UpdateOutcomeDto {
    Updated,
    AlreadyCurrent,
    SkippedPinned,
    LocalEdits,
    Failed(String),
    NeedsReack(TrustPromptDto),
}

impl From<crate::skills_install::UpdateOutcome> for UpdateOutcomeDto {
    fn from(outcome: crate::skills_install::UpdateOutcome) -> Self {
        use crate::skills_install::UpdateOutcome;
        match outcome {
            UpdateOutcome::Updated => UpdateOutcomeDto::Updated,
            UpdateOutcome::AlreadyCurrent => UpdateOutcomeDto::AlreadyCurrent,
            UpdateOutcome::SkippedPinned => UpdateOutcomeDto::SkippedPinned,
            UpdateOutcome::LocalEdits => UpdateOutcomeDto::LocalEdits,
            UpdateOutcome::Failed(message) => UpdateOutcomeDto::Failed(message),
            UpdateOutcome::NeedsReack(prompt) => {
                UpdateOutcomeDto::NeedsReack(TrustPromptDto::from(prompt))
            }
        }
    }
}

/// One pack's outcome from `update_all_plugins` —
/// `crate::skills_install::update_all_packs` returns
/// `Vec<(String, UpdateOutcome)>`; specta can't name a bare tuple usefully in
/// the generated TS, so this wraps it in a named struct.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcomeEntry {
    pub id: String,
    pub outcome: UpdateOutcomeDto,
}

/// Mirror of `crate::plugins::doctor::DoctorFinding`. Already secret-free at
/// the source (see that module's doc comment) — this DTO adds no new fields,
/// just the specta `Type` the core struct doesn't derive.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFinding {
    pub plugin_id: String,
    /// `warn` | `error`.
    pub severity: String,
    /// `reconnect-required` | `missing-binary` | `attach-failed` | `blocked` |
    /// `slot-conflict` | `not-running` | `crashed` | `restart-exhausted` |
    /// `init-failed` (the last four are Track D extension findings — DT8,
    /// see `crate::plugins::doctor::plugin_doctor`'s extension section).
    pub kind: String,
    pub message: String,
    pub suggested_action: String,
}

/// `refresh_catalog`/`catalog_status` rpc result — a thin snapshot of the
/// `catalog_feed_state` row plus counts from the cached
/// `plugin_catalog_cache` table (`crate::store::RemoteCatalogRow`). `sequence`
/// stays a `u64` for the same reason `TrustPromptDto.total_bytes` does: no
/// bindings-shape cost, since `export_bindings`'s `BigIntExportBehavior::Number`
/// already renders it as a plain TS `number`.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStatus {
    pub sequence: u64,
    pub last_fetch_at: Option<i64>,
    pub outcome: Option<String>,
    pub entries: u32,
    pub blocked: u32,
}

/// `extension_status` rpc result — one entry per extension (Track D "code
/// plugin") the daemon's `ExtensionHost` currently knows about (DT8). Mirrors
/// `plugins::extension::{ExtensionSnapshot, ExtensionStatus}` flattened into
/// a specta-able, UI-friendly shape (same rationale as `DoctorFinding`
/// mirroring `plugins::doctor::DoctorFinding`) rather than deriving `Type` on
/// the core enum directly. `crate::api::extension_status_api` builds these
/// field by field (no `From` impl) since `ExtensionStatus::Failed`'s reason
/// needs to fan out into both `status` (the canned string) and `last_error`
/// (the sanitized detail) — a single `From` conversion would need the same
/// branching anyway.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatusEntry {
    pub plugin_id: String,
    /// The manifest's `[[extension]] name` — unique within its own plugin,
    /// not globally (mirrors `ExtensionSnapshot::name`'s own namespace note).
    pub name: String,
    /// `running` | `starting` | `restarting` | `failed` | `stopped` |
    /// `not-running` (the last one has no `ExtensionStatus` counterpart — it
    /// means the plugin declares an extension and is enabled, but the host
    /// has no spawned entry for it at all, e.g. a still-pending spawn or a
    /// resolution failure prior to ever reaching `Failed`).
    pub status: String,
    /// Lifetime count of restart attempts DT4's supervisor has made for this
    /// entry. Always `0` for an entry that has never needed a restart
    /// (including the synthetic `not-running` entries, which were never
    /// spawned at all).
    pub restart_count: u32,
    /// Present only when `status == "failed"` — `ExtensionStatus::Failed`'s
    /// already-sanitized reason (`proc::sanitize_init_error`/the
    /// `restart-exhausted: ...` marker), never extension-supplied raw text.
    pub last_error: Option<String>,
    pub confirmed_events: Vec<String>,
    pub tool_count: u32,
}

impl From<crate::plugins::doctor::DoctorFinding> for DoctorFinding {
    fn from(f: crate::plugins::doctor::DoctorFinding) -> Self {
        DoctorFinding {
            plugin_id: f.plugin_id,
            severity: f.severity,
            kind: f.kind,
            message: f.message,
            suggested_action: f.suggested_action,
        }
    }
}

// --- agent_api (Plan 3: agent management RPC family for the Cockpit Agents panel) ---

/// An agent's model assignment: either a concrete provider model (with an
/// optional effort override) or a symbolic router route (`free`, ...).
/// Routes never carry an effort — `deny_unknown_fields` makes a
/// `{"kind":"route", ..., "effort": ...}` payload a decode error rather
/// than a silently dropped field.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum AgentModelInfo {
    Concrete {
        name: String,
        effort: Option<String>,
    },
    Route {
        route: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleInfo {
    pub id: String,
    pub tool: String,
    pub decision: String,
    pub command_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentValidationInfo {
    pub field: String,
    pub message: String,
}

/// One startup-recovery note surfaced to the UI (for example a quarantined
/// agent file that failed to parse and was set aside).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecoveryInfo {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummaryInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub avatar_color: String,
    pub model: AgentModelInfo,
    pub permission_mode: String,
    pub skill_count: u32,
    pub tool_count: u32,
    pub knowledge_count: u32,
    pub executable: bool,
    pub validation: Vec<AgentValidationInfo>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetailInfo {
    pub summary: AgentSummaryInfo,
    pub permission_rules: Vec<PermissionRuleInfo>,
    pub skills: Vec<String>,
    pub native_tools: Vec<String>,
    pub plugin_tools: Vec<String>,
    pub apps: Vec<String>,
    pub max_turns: u32,
    pub max_tool_rounds: u32,
    pub model_info: Option<SelectableModelInfo>,
}

/// Everything a create/update mutation may set on an agent. Server-derived
/// fields (`id`, counts, `executable`, `validation`, `is_default`) are
/// deliberately absent so the client can't submit them.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentMutationInfo {
    pub name: String,
    pub description: String,
    pub avatar_color: String,
    pub model: AgentModelInfo,
    pub permission_mode: String,
    pub permission_rules: Vec<PermissionRuleInfo>,
    pub skills: Vec<String>,
    pub native_tools: Vec<String>,
    pub plugin_tools: Vec<String>,
    pub apps: Vec<String>,
    pub max_turns: u32,
    pub max_tool_rounds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentRegistryInfo {
    pub agents: Vec<AgentSummaryInfo>,
    pub default_agent_id: String,
    pub recovery: Vec<AgentRecoveryInfo>,
    pub subagent_model: AgentModelInfo,
}

/// One knowledge concept as stored in the agent's OKF tree. `timestamp` is
/// RFC3339. `scope` is `None` for non-memory concepts and one of `global`,
/// `user`, or `project` for memory; `project_id` is non-null only for
/// project memory.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeConceptInfo {
    pub id: String,
    pub relative_path: String,
    pub concept_type: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub scope: Option<String>,
    pub project_id: Option<String>,
    pub tags: Vec<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeConceptMutationInfo {
    pub title: String,
    pub description: String,
    pub body: String,
    pub scope: String,
    pub project_id: Option<String>,
    pub tags: Vec<String>,
}

/// A knowledge file that failed OKF parsing: surfaced with its raw markdown
/// so the UI can offer repair instead of silently dropping it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InvalidKnowledgeConceptInfo {
    pub relative_path: String,
    pub error: String,
    pub raw_markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JourneyMilestoneInfo {
    pub concept_id: String,
    pub title: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillUsageInfo {
    pub skill_id: String,
    pub uses: u64,
    pub successes: u64,
    pub concept_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LearningReviewInfo {
    pub concept_id: String,
    pub title: String,
    pub description: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CuratorStateInfo {
    pub concept: Option<KnowledgeConceptInfo>,
    pub last_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CuratorHistorySnapshotInfo {
    pub snapshot_id: String,
    pub concept: KnowledgeConceptInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentLearningInfo {
    pub concepts: Vec<KnowledgeConceptInfo>,
    pub invalid: Vec<InvalidKnowledgeConceptInfo>,
    pub journey: Vec<JourneyMilestoneInfo>,
    pub skill_usage: Vec<AgentSkillUsageInfo>,
    pub reviews: Vec<LearningReviewInfo>,
    pub curator: CuratorStateInfo,
    pub curator_history: Vec<CuratorHistorySnapshotInfo>,
}

/// One row of a session's artifact listing (`artifacts_api::list_session_artifacts`):
/// either an artifact the session originated (`reference_id` and its sibling
/// reference fields are `None`) or one shared into the session via a
/// reference (all three are `Some`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactInfo {
    pub id: String,
    pub source_session_pk: String,
    pub reference_id: Option<String>,
    pub shared_from_session_pk: Option<String>,
    pub parent_reference_id: Option<String>,
    pub status: String,
    pub name: String,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub creator: String,
    pub created_at: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactFileInfo {
    pub name: String,
    pub content_type: Option<String>,
    pub data_base64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mention_and_turn_input_use_camel_case_fields() {
        let turn: TurnInput = serde_json::from_value(serde_json::json!({
            "text": "ask Ada",
            "mentions": [{
                "agentId": "ada",
                "labelSnapshot": "Ada",
                "startUtf16": 4,
                "endUtf16": 7
            }],
            "attachments": []
        }))
        .unwrap();

        assert_eq!(
            serde_json::to_value(&turn.mentions[0]).unwrap(),
            serde_json::json!({
                "agentId": "ada",
                "labelSnapshot": "Ada",
                "startUtf16": 4,
                "endUtf16": 7
            })
        );
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
    fn git_options_convert_to_session_git_options_trimming_blanks() {
        let core: SessionGitOptions = GitOptions {
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
}

#[cfg(test)]
mod agent_management_dto_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_model_union_uses_discriminated_camel_case_shape() {
        assert_eq!(
            serde_json::to_value(AgentModelInfo::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: Some("high".into()),
            })
            .unwrap(),
            json!({"kind":"concrete","name":"anthropic/claude-opus-4-8","effort":"high"})
        );
        assert_eq!(
            serde_json::to_value(AgentModelInfo::Route {
                route: "free".into()
            })
            .unwrap(),
            json!({"kind":"route","route":"free"})
        );
    }

    #[test]
    fn mutation_input_rejects_route_effort_by_construction() {
        let parsed = serde_json::from_value::<AgentMutationInfo>(json!({
            "name":"Reviewer",
            "description":"Reviews changes",
            "avatarColor":"violet",
            "model":{"kind":"route","route":"free","effort":"high"},
            "permissionMode":"ask",
            "permissionRules":[],
            "skills":[],
            "nativeTools":["read"],
            "pluginTools":[],
            "apps":[],
            "maxTurns":50,
            "maxToolRounds":100
        }));
        assert!(parsed.is_err());
    }
}
