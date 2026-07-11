//! Shared request/response shapes for the RPC command families under
//! `api/`. Populated by each command family as it moves its DTOs and
//! private helpers out of the src-tauri `commands.rs` module (see the Move
//! Recipe) — bindings-stable, so every serde/specta attribute here must stay
//! byte-identical to the source it was moved from.

use crate::domain::SessionGitOptions;
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
    pub context: Option<ChatContextArg>,
    #[serde(default)]
    pub attachments: Vec<String>,
    /// None => engine default (worktree ON, new engine-named branch from HEAD).
    pub git: Option<GitOptions>,
    /// Initial permission mode for the session being started (new-chat
    /// picker). `None` ⇒ inherit the project default.
    pub perm_mode: Option<crate::domain::PermMode>,
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

// --- agent_api (moved verbatim from apps/cockpit/src-tauri/src/agent_cmd.rs) ---

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettingsInfo {
    pub model: Option<String>,
    /// "plan" | "ask" | "edit" | "full"; None = engine default ("ask").
    pub perm_mode: Option<String>,
}

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
    pub base_url: Option<String>,
    pub models: Vec<String>,
    /// e.g. "sk-…3fk9" — full key never leaves the backend after creation.
    pub key_masked: Option<String>,
    /// OAuth connections only: true once refresh has failed terminally and
    /// the user needs to reconnect via the browser/paste flow again.
    pub needs_relogin: bool,
    /// Anthropic OAuth only: enable full Claude Code-style request cloaking.
    pub claude_cloaking: bool,
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
    /// `reconnect-required` | `missing-binary` | `attach-failed` | `blocked`.
    pub kind: String,
    pub message: String,
    pub suggested_action: String,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn chat_request_options_deserializes_model_and_ignores_legacy_runtime_id() {
        // Native-only: a legacy `runtimeId` key is accepted but ignored.
        let opts: ChatRequestOptions =
            serde_json::from_value(serde_json::json!({"runtimeId": "native", "model": "fable"}))
                .unwrap();
        assert_eq!(opts.model.as_deref(), Some("fable"));
    }

    #[test]
    fn chat_request_options_git_defaults_to_none_and_deserializes() {
        // Old payloads (no `git` key) keep parsing.
        let opts: ChatRequestOptions =
            serde_json::from_value(serde_json::json!({"model": "fable"})).unwrap();
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
