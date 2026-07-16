use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PermMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
}

impl PermMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermMode::Default => "default",
            PermMode::AcceptEdits => "acceptEdits",
            PermMode::BypassPermissions => "bypassPermissions",
            PermMode::Plan => "plan",
        }
    }
    pub fn from_db(s: &str) -> PermMode {
        match s {
            "acceptEdits" => PermMode::AcceptEdits,
            "bypassPermissions" => PermMode::BypassPermissions,
            "plan" => PermMode::Plan,
            _ => PermMode::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Idle,
    Running,
    Interrupted,
    Ended,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Interrupted => "interrupted",
            SessionStatus::Ended => "ended",
        }
    }
    pub fn from_db(s: &str) -> SessionStatus {
        match s {
            "running" => SessionStatus::Running,
            "interrupted" => SessionStatus::Interrupted,
            "ended" => SessionStatus::Ended,
            _ => SessionStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub workdir: String,
    pub source: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub perm_mode: PermMode,
    pub created_at: Option<i64>,
    /// Computed at read time (`git2::Repository::open` probe on `workdir`) —
    /// NOT a DB column. Self-corrects if the user later runs `git init`.
    pub is_git: bool,
}

/// What a session represents. `Project` is the pre-Phase-2 default (bound to
/// a project workdir); `Chat`, `Worker`, and `Review` are chat-first kinds
/// added in Phase 2 — `project_id` is `None` for all three, and `Worker`/
/// `Review` additionally carry `parent_session_pk` lineage back to the chat
/// or project session that spawned them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionKind {
    Project,
    Chat,
    Worker,
    Review,
}

impl SessionKind {
    pub fn from_db(s: &str) -> Self {
        match s {
            "chat" => SessionKind::Chat,
            "worker" => SessionKind::Worker,
            "review" => SessionKind::Review,
            _ => SessionKind::Project,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionKind::Project => "project",
            SessionKind::Chat => "chat",
            SessionKind::Worker => "worker",
            SessionKind::Review => "review",
        }
    }
}

/// A durable background-rail row (spec §6.1). Producers (async delegation,
/// learning forks, and scheduled jobs) enqueue one; the daemon
/// drainer delivers it into `target_session_pk` as a new user turn while
/// that session is idle. `kind` is one of [`BackgroundKind`]'s db strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundEvent {
    pub id: String,
    pub target_session_pk: String,
    /// The primary run that dispatched a delegation outcome. `None` preserves
    /// the normal rail behavior for non-delegation producers and legacy rows.
    pub origin_run_id: Option<String>,
    pub kind: String,
    pub payload: String,
    pub created_at: i64,
    pub claimed_by: Option<String>,
    pub delivered_at: Option<i64>,
}

/// The producers that write to the background rail. Stored as a db string in
/// `background_events.kind`; not part of the IPC surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Delegation,
    Learning,
    Job,
}

impl BackgroundKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackgroundKind::Delegation => "delegation",
            BackgroundKind::Learning => "learning",
            BackgroundKind::Job => "job",
        }
    }
    pub fn from_db(s: &str) -> Self {
        match s {
            "learning" => BackgroundKind::Learning,
            "job" => BackgroundKind::Job,
            _ => BackgroundKind::Delegation,
        }
    }
}

/// Which actor initiated a write — a general-purpose provenance marker
/// carried on `ToolCtx` (Phase 4 §7) and reused by Phase 6's app-control
/// negative-space guard. Deliberately NOT skill-usage-specific: any tool or
/// subsystem that needs to know "who is asking" (a human in an interactive
/// session, an autonomous agent turn, or the strictest background
/// self-review fork) can gate on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteOrigin {
    /// An interactive human-driven session.
    #[default]
    User,
    /// An autonomous agent turn (primary or sub-agent).
    Agent,
    /// The background self-improvement review fork (Phase 4 §7.2) — the
    /// strictest origin.
    BackgroundReview,
}

impl WriteOrigin {
    /// True for every origin except an interactive user turn.
    pub fn is_autonomous(self) -> bool {
        !matches!(self, WriteOrigin::User)
    }

    /// True only for an interactive user turn — the inverse of
    /// `is_autonomous`. The protected write APIs (settings, tool policies)
    /// admit only this origin (Phase 6 §9.3 negative space).
    pub fn is_user(self) -> bool {
        matches!(self, WriteOrigin::User)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            WriteOrigin::User => "user",
            WriteOrigin::Agent => "agent",
            WriteOrigin::BackgroundReview => "background_review",
        }
    }

    /// Parse a persisted origin string. Fails CLOSED: any unrecognized value
    /// decodes to the least-privileged `Agent`, never `User`, so a corrupt or
    /// unknown persisted origin (e.g. a future variant read by an older build,
    /// or a tampered audit row) can never be read back as a privileged
    /// settings/policy write. `default()` remains `User` — that is the trusted
    /// in-code default for interactive sessions, a distinct concern from
    /// decoding untrusted persisted data.
    pub fn from_db(s: &str) -> Self {
        match s {
            "user" => WriteOrigin::User,
            "background_review" => WriteOrigin::BackgroundReview,
            _ => WriteOrigin::Agent,
        }
    }
}

/// Per-skill telemetry (Phase 4 §4/§7): use/view/patch counters and
/// lifecycle state, read by the `skill_manage` native tool (Task 6) and the
/// curator (Task 10) to decide when a skill should transition between
/// `active`, `stale`, and `archived`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SkillUsage {
    pub name: String,
    pub created_by: Option<String>,
    pub use_count: i64,
    pub view_count: i64,
    pub patch_count: i64,
    pub last_used_at: Option<i64>,
    pub last_viewed_at: Option<i64>,
    pub last_patched_at: Option<i64>,
    pub state: String,
    pub pinned: bool,
    pub archived_at: Option<i64>,
    pub created_at: Option<i64>,
}

/// One `curator_runs` row (Task 10/Task 1 migration #28): a single curator
/// sweep's bookkeeping, read back by the Cockpit Learning panel's (Task 11)
/// history view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CuratorRun {
    pub id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// `running` | `ok` | `error`.
    pub status: String,
    /// How many skills the deterministic planner transitioned this run.
    pub transitioned: i64,
    /// Whether the opt-in LLM consolidation pass ran this run.
    pub consolidated: bool,
    /// Pre-mutation tar.gz snapshot path, set only when `consolidated`.
    pub snapshot_path: Option<String>,
    pub error: Option<String>,
    pub log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_pk: String,
    /// The immutable stable ID of the agent selected when this session began.
    /// `None` with `primary_agent_snapshot == None` identifies legacy history.
    pub primary_agent_id: Option<String>,
    /// The display identity captured when the session began; never derived from
    /// the mutable registry after persistence.
    pub primary_agent_snapshot: Option<AgentIdentitySnapshot>,
    /// `None` for chat-first sessions (`kind != Project`); a project-bound
    /// session always has this set.
    pub project_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub status: SessionStatus,
    /// Per-session permission mode. Copied from the project (or the new-chat
    /// picker) at creation; changing it affects THIS session only.
    pub perm_mode: PermMode,
    pub started_by: Option<String>,
    pub created_at: Option<i64>,
    pub last_active: Option<i64>,
    pub resume_attempts: i64,
    /// True when the engine auto-generated the branch name (`ryuzi/{short}`).
    /// `end_session` deletes the branch ONLY when this is set; user-named and
    /// pre-existing branches survive teardown.
    pub branch_owned: bool,
    pub kind: SessionKind,
    /// Who is speaking in this session (chat-first; e.g. a Discord user id
    /// or `"cockpit"`). Unused for `Project` sessions.
    pub speaker: Option<String>,
    /// Which agent persona/config is driving this session. Unused for
    /// `Project` sessions.
    pub agent: Option<String>,
    /// The session this one was spawned from (`Worker`/`Review` lineage).
    pub parent_session_pk: Option<String>,
}

/// An immutable identity captured when an agent becomes the primary owner of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdentitySnapshot {
    pub id: String,
    pub name: String,
    pub avatar_color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRunKind {
    Primary,
    MainDelegate,
    Subagent,
}

impl AgentRunKind {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::MainDelegate => "main-delegate",
            Self::Subagent => "subagent",
        }
    }

    pub fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "primary" => Ok(Self::Primary),
            "main-delegate" => Ok(Self::MainDelegate),
            "subagent" => Ok(Self::Subagent),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid agent run kind `{value}`").into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AgentRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl AgentRunStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid agent run status `{value}`").into(),
            )),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub run_id: String,
    pub session_pk: String,
    pub parent_run_id: Option<String>,
    pub retry_of: Option<String>,
    pub source_tool_call_id: Option<String>,
    pub dispatch_index: Option<i64>,
    pub primary_agent_id: String,
    pub executing_agent_id: Option<String>,
    pub executing_agent_name_snapshot: String,
    pub agent_kind: AgentRunKind,
    pub task: String,
    pub status: AgentRunStatus,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub tool_count: u32,
    pub resolved_model: Option<String>,
    pub resolved_effort: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentRun {
    pub run_id: String,
    pub session_pk: String,
    pub parent_run_id: Option<String>,
    pub retry_of: Option<String>,
    pub source_tool_call_id: Option<String>,
    pub dispatch_index: Option<i64>,
    pub primary_agent_id: String,
    pub executing_agent_id: Option<String>,
    pub executing_agent_name_snapshot: String,
    pub agent_kind: AgentRunKind,
    pub task: String,
    pub status: AgentRunStatus,
    pub resolved_model: Option<String>,
    pub resolved_effort: Option<String>,
}

/// How a new session's git workspace is prepared (branch controls).
/// `Default` reproduces the legacy behavior: an isolated worktree on a fresh
/// engine-named branch cut from the repo HEAD. Not part of the IPC surface —
/// the cockpit's `GitOptions` (specta) converts into this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGitOptions {
    pub use_worktree: bool,
    pub create_branch: bool,
    /// User-typed branch name; `None` => auto `ryuzi/{short}`.
    pub branch_name: Option<String>,
    /// Branch to cut from (`create_branch`) or run on (`!create_branch`);
    /// `None` => repo HEAD / current branch (legacy behavior).
    pub base_branch: Option<String>,
}

impl Default for SessionGitOptions {
    fn default() -> Self {
        SessionGitOptions {
            use_worktree: true,
            create_branch: true,
            branch_name: None,
            base_branch: None,
        }
    }
}

/// An MCP server the native agent can use as tools (attached to a harness session).
///
/// After plugin `${auth}`/setting substitution, a resolved `McpServerSpec`'s
/// `transport` carries RESOLVED SECRETS in `Stdio::env`/`Http::headers` (API
/// keys, tokens, etc.). `Serialize` exists for internal wiring only — nothing
/// in this codebase serializes a resolved spec today, but if a future
/// feature does (session export, events, logs, or any other user-visible
/// output), it MUST redact `env`/`headers` values first. Never serialize a
/// resolved `McpServerSpec` verbatim into anything a user or client can read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSpec {
    pub name: String,
    pub transport: McpTransport,
}

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// Where a session is driven from (a gateway channel + conversation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    pub gateway: String,
    pub conversation_id: String,
}

/// Who initiated an action, across gateways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub id: String,
    pub gateway: String,
}

/// A file a user attached to a message, before it has been downloaded.
/// Not part of the specta/Tauri type export surface — this crosses gateway
/// boundaries, not the cockpit IPC boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub name: String,
    pub url: String,
    pub content_type: Option<String>,
    pub size: u64,
}

/// A persisted prompt waiting for a session to become available. Attachment
/// references remain durable inputs; turn blocks and display metadata are
/// reconstructed only when the prompt is delivered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSessionPrompt {
    pub id: String,
    pub session_pk: String,
    pub agent: String,
    pub display: String,
    pub attachments: Vec<AttachmentRef>,
    pub created_at: i64,
}

impl QueuedSessionPrompt {
    pub fn into_turn_prompt(self) -> crate::harness::TurnPrompt {
        crate::harness::TurnPrompt::text(self.agent, self.display)
    }
}

/// Identifies the plugin an approvable action originates from — attribution
/// only, so an operator can see "this MCP tool belongs to plugin X" instead
/// of guessing from a substring match between the MCP server name and a
/// manifest id. `None` (everywhere this is optional) means the action is the
/// core agent itself (a built-in tool), not a plugin. Resolved at the
/// mcp-server→plugin binding (`ControlPlane::attach_plugin_mcp_servers`),
/// never by parsing the tool/server name string.
///
/// Carries no gating semantics: this is visibility/attribution metadata for
/// the approval prompt, not an input to the permission DECISION.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub plugin_id: String,
    pub plugin_name: String,
}

/// A tool-approval request surfaced to a gateway / UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// Durable run that owns this request. Resolution must supply this value
    /// along with `request_id`, because tool-call IDs are not global.
    pub run_id: String,
    /// Stable id of the agent that requested approval.
    pub requesting_agent_id: String,
    /// Display-name snapshot of the requesting agent.
    pub requesting_agent_name: String,
    pub request_id: String,
    pub tool: String,
    pub summary: String,
    /// Role ids allowed to approve, beyond the session starter. Empty means
    /// "starter only" (see `policy::can_approve`).
    #[serde(default)]
    pub approver_role_ids: Vec<String>,
    /// Actor id that started the session, for starter-always approval.
    #[serde(default)]
    pub started_by: Option<String>,
    /// Optional approval timeout, in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Which plugin's MCP tool this approval is for, if any (see [`Principal`]).
    #[serde(default)]
    pub principal: Option<Principal>,
}

/// The user's decision on a tool-approval request from the native runtime's
/// permission gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Cancel,
}

/// What a pending approval is asking the user for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalKind {
    /// Permission to run one tool call.
    Tool,
    /// Review of an `exitplanmode` plan.
    Plan,
    /// An `askuserquestion` form.
    Question,
}

/// Where an `AllowAlways`/`RejectAlways` decision is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalScope {
    /// In-memory for the current session only.
    Session,
    /// Persisted to the project's `tool_policies` row.
    Project,
}

/// The user's full reply to an approval request. `payload` carries
/// kind-specific data: `{"mode": "acceptEdits"|"default"}` or
/// `{"feedback": "…"}` for Plan, `{"answers": {question: [labels]}}`
/// for Question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
    pub scope: Option<ApprovalScope>,
    pub payload: Option<serde_json::Value>,
}

impl ApprovalResponse {
    /// The plain binary reply (`resolve_bool`, bulk session deny, gateways).
    pub fn once(allow: bool) -> Self {
        ApprovalResponse {
            decision: if allow {
                ApprovalDecision::AllowOnce
            } else {
                ApprovalDecision::RejectOnce
            },
            scope: None,
            payload: None,
        }
    }

    /// Whether the decision grants the request.
    pub fn allowed(&self) -> bool {
        matches!(
            self.decision,
            ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways
        )
    }
}

/// One persisted "don't ask again" rule (Settings → Permissions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyRow {
    pub project_id: String,
    pub tool: String,
    pub decision: String,
}

/// One app-control audit entry, surfaced in Cockpit's Settings → Audit feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    pub id: i64,
    pub tool: String,
    pub action: String,
    pub decision: String,
    /// The initiating `WriteOrigin` as a string (`user`|`agent`|`background_review`).
    pub origin: String,
    pub session_pk: Option<String>,
    /// Unix ms.
    pub at: i64,
}

/// A persisted transcript entry, one row per native-runtime event block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub session_pk: String,
    pub seq: i64,
    pub role: String,       // user | assistant | system
    pub block_type: String, // text | thought | tool_call | plan | status | error
    pub payload: serde_json::Value,
    pub tool_call_id: Option<String>,
    pub status: Option<String>,
    pub tool_kind: Option<String>,
    pub created_at: i64,
    /// Legacy group-chat attribution retained so existing databases and event
    /// payloads remain readable. New message constructors leave it unset.
    pub speaker: Option<String>,
}

/// Input to `Store::insert_message`; `seq` and `created_at` are assigned by the store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMessage {
    pub session_pk: String,
    pub role: String,
    pub block_type: String,
    pub payload: serde_json::Value,
    pub tool_call_id: Option<String>,
    pub status: Option<String>,
    pub tool_kind: Option<String>,
    /// Legacy group-chat attribution retained for database and event
    /// compatibility. New message producers always leave it unset.
    pub speaker: Option<String>,
}

impl NewMessage {
    /// Convenience for a simple text/status/error block.
    pub fn block(
        session_pk: &str,
        role: &str,
        block_type: &str,
        payload: serde_json::Value,
    ) -> Self {
        NewMessage {
            session_pk: session_pk.to_string(),
            role: role.to_string(),
            block_type: block_type.to_string(),
            payload,
            tool_call_id: None,
            status: None,
            tool_kind: None,
            speaker: None,
        }
    }
}

/// One durable entry in the native runtime's provider-turn ledger: a single
/// Anthropic-format message (`{role, content:[...]}`) as sent to / received
/// from the model. Separate from the display-oriented [`Message`] rows; this
/// is what the native runner replays to reconstruct history on resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderTurn {
    pub session_pk: String,
    pub seq: i64,
    pub role: String, // user | assistant
    /// The Anthropic `content` array for this turn.
    pub payload: serde_json::Value,
    pub created_at: i64,
}

/// Input to `Store::insert_provider_turn`; `seq` and `created_at` are assigned
/// by the store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewProviderTurn {
    pub session_pk: String,
    pub role: String,
    pub payload: serde_json::Value,
}

impl NewProviderTurn {
    pub fn new(
        session_pk: impl Into<String>,
        role: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        NewProviderTurn {
            session_pk: session_pk.into(),
            role: role.into(),
            payload,
        }
    }
}

/// One model's accumulated billed tokens + computed dollar cost within a
/// session. Token fields are the durable truth; `usd` is derived from the
/// current price table at emit time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub usd: f64,
}

/// Public event broadcast to consumers (the Tauri layer re-emits these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CoreEvent {
    SessionCreated {
        session_pk: String,
        /// `None` for a project-less (chat-first) session.
        project_id: Option<String>,
    },
    Message {
        session_pk: String,
        seq: i64,
        role: String,
        block_type: String,
        payload: serde_json::Value,
        tool_call_id: Option<String>,
        status: Option<String>,
        tool_kind: Option<String>,
        /// Legacy group-chat attribution retained in message events for
        /// database and wire compatibility.
        speaker: Option<String>,
    },
    SessionQueueChanged {
        session_pk: String,
    },
    Result {
        session_pk: String,
    },
    ApprovalRequested {
        session_pk: String,
        /// Durable run that owns this approval. It is required for resolution.
        run_id: String,
        /// Agent profile identity and display name that originated the prompt.
        requesting_agent_id: String,
        requesting_agent_name: String,
        request_id: String,
        tool: String,
        summary: String,
        /// What the prompt is: a tool permission, a plan review, or a question
        /// form. Named `approval_kind` — `kind` would collide with the enum's
        /// serde tag.
        approval_kind: ApprovalKind,
        /// Raw kind-specific payload: the tool's input JSON (Tool), the plan
        /// markdown (Plan), or the questions spec (Question).
        input: serde_json::Value,
        /// Which plugin this approval's MCP tool belongs to, if any (see
        /// [`Principal`]). `None` for built-in tools and Plan/Question prompts.
        #[serde(default)]
        principal: Option<Principal>,
    },
    Error {
        session_pk: String,
        message: String,
    },
    /// Out-of-band announcement (e.g. "update available") rendered to every
    /// surface of a session.
    Notice {
        session_pk: String,
        text: String,
    },
    SessionEnded {
        session_pk: String,
    },
    /// A delegation run changed after its persisted status commit.
    AgentRunChanged {
        session_pk: String,
        run_id: String,
        parent_run_id: Option<String>,
        status: String,
    },
    /// A Hook run changed state (queued|running|success|failed|skipped).
    AutomationHookRunChanged {
        hook_id: String,
        run_id: String,
        status: String,
    },
    /// A scheduled job run started or finished (status: running|success|failed).
    JobRunChanged {
        job_id: String,
        run_id: String,
        status: String,
    },
    /// Per-response context usage for a native session (drives the
    /// "% context left" indicator).
    ContextUsage {
        session_pk: String,
        active_tokens: u64,
        context_window: u64,
        usable_window: u64,
        percent_left: u8,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        output_tokens: u64,
    },
    /// The native runtime compacted a session's history
    /// (trigger: pre_turn|mid_turn|manual).
    ContextCompacted {
        session_pk: String,
        trigger: String,
        before_tokens: u64,
        after_tokens: u64,
        window_number: u32,
    },
    /// A provider OAuth flow produced its authorize URL. Surfaces open it
    /// (Cockpit maps this onto the legacy OauthAuthorizeUrlMsg Tauri event).
    OauthAuthorizeUrl {
        provider: String,
        authorize_url: String,
    },
    /// Same for a plugin OAuth flow.
    PluginOauthAuthorizeUrl {
        plugin_id: String,
        authorize_url: String,
    },
    /// Per-session accumulated cost: total USD and a per-model token+dollar
    /// breakdown. Emitted alongside `ContextUsage`.
    ///
    /// Like its sibling context-telemetry variants above, this variant's own
    /// fields stay snake_case (`session_pk`, `total_usd`): the enum-level
    /// `rename_all = "camelCase"` on `CoreEvent` only renames the `kind` tag
    /// value, not each variant's field names (see `ContextUsage`'s
    /// `session_pk`). The nested `ModelCost` struct carries its own
    /// `rename_all = "camelCase"`, so its fields (e.g. `cache_read` →
    /// `cacheRead`) are camelCased independently.
    SessionCost {
        session_pk: String,
        total_usd: f64,
        models: Vec<ModelCost>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_mode_roundtrips_through_db_string() {
        for m in [
            PermMode::Default,
            PermMode::AcceptEdits,
            PermMode::BypassPermissions,
            PermMode::Plan,
        ] {
            assert_eq!(PermMode::from_db(m.as_str()), m);
        }
        assert_eq!(PermMode::from_db("nonsense"), PermMode::Default);
    }

    #[test]
    fn session_status_roundtrips_through_db_string() {
        for s in [
            SessionStatus::Idle,
            SessionStatus::Running,
            SessionStatus::Interrupted,
            SessionStatus::Ended,
        ] {
            assert_eq!(SessionStatus::from_db(s.as_str()), s);
        }
        assert_eq!(SessionStatus::from_db("nonsense"), SessionStatus::Idle);
    }

    #[test]
    fn write_origin_roundtrips_through_db_string() {
        for o in [
            WriteOrigin::User,
            WriteOrigin::Agent,
            WriteOrigin::BackgroundReview,
        ] {
            assert_eq!(WriteOrigin::from_db(o.as_str()), o);
        }
        // Fails CLOSED: an unrecognized persisted origin decodes to the
        // least-privileged `Agent`, never the privileged `User` (Phase 6 §9.3).
        assert_eq!(WriteOrigin::from_db("nonsense"), WriteOrigin::Agent);
    }

    #[test]
    fn write_origin_default_is_user_and_autonomy() {
        assert_eq!(WriteOrigin::default(), WriteOrigin::User);
        assert!(!WriteOrigin::User.is_autonomous());
        assert!(WriteOrigin::Agent.is_autonomous());
        assert!(WriteOrigin::BackgroundReview.is_autonomous());
        // `is_user` is the exact inverse of `is_autonomous`.
        assert!(WriteOrigin::User.is_user());
        assert!(!WriteOrigin::Agent.is_user());
        assert!(!WriteOrigin::BackgroundReview.is_user());
    }

    #[test]
    fn mcp_server_spec_round_trips_through_json() {
        let spec = McpServerSpec {
            name: "notion".into(),
            transport: McpTransport::Stdio {
                command: "notion-mcp".into(),
                args: vec!["--stdio".into()],
                env: vec![("TOKEN".into(), "x".into())],
            },
        };
        let j = serde_json::to_string(&spec).unwrap();
        let back: McpServerSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn core_event_serializes_with_camel_tag_and_snake_fields() {
        let e = CoreEvent::SessionCreated {
            session_pk: "s1".into(),
            project_id: Some("p1".into()),
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "sessionCreated");
        assert_eq!(j["session_pk"], "s1");
        assert_eq!(j["project_id"], "p1");

        // A chat (project-less) session serializes project_id as null.
        let e = CoreEvent::SessionCreated {
            session_pk: "s2".into(),
            project_id: None,
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["project_id"], serde_json::Value::Null);
    }

    #[test]
    fn session_git_options_default_matches_legacy_behavior() {
        // Legacy behavior = isolated worktree on a fresh engine-named branch
        // cut from the repo HEAD.
        let d = SessionGitOptions::default();
        assert!(d.use_worktree);
        assert!(d.create_branch);
        assert_eq!(d.branch_name, None);
        assert_eq!(d.base_branch, None);
    }

    #[test]
    fn context_events_serialize_with_camel_kind() {
        let e = CoreEvent::ContextUsage {
            session_pk: "s1".into(),
            active_tokens: 120_000,
            context_window: 200_000,
            usable_window: 190_000,
            percent_left: 37,
            cache_read_tokens: 90_000,
            cache_creation_tokens: 4_000,
            output_tokens: 512,
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "contextUsage");
        assert_eq!(j["percent_left"], 37);

        let e = CoreEvent::ContextCompacted {
            session_pk: "s1".into(),
            trigger: "pre_turn".into(),
            before_tokens: 180_000,
            after_tokens: 31_000,
            window_number: 2,
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "contextCompacted");
        assert_eq!(j["window_number"], 2);
    }

    #[test]
    fn oauth_authorize_url_event_serializes_with_kind_tag() {
        let e = CoreEvent::OauthAuthorizeUrl {
            provider: "anthropic-oauth".into(),
            authorize_url: "https://x".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "oauthAuthorizeUrl");
        assert_eq!(v["authorize_url"], "https://x");
    }

    #[test]
    fn plugin_oauth_authorize_url_event_serializes_with_kind_tag() {
        let e = CoreEvent::PluginOauthAuthorizeUrl {
            plugin_id: "acme".into(),
            authorize_url: "https://y".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "pluginOauthAuthorizeUrl");
        assert_eq!(v["plugin_id"], "acme");
        assert_eq!(v["authorize_url"], "https://y");
    }

    #[test]
    fn session_cost_serializes_snake_variant_camel_nested() {
        let e = CoreEvent::SessionCost {
            session_pk: "s1".into(),
            total_usd: 0.1234,
            models: vec![ModelCost {
                model: "claude-sonnet-4".into(),
                input: 100,
                output: 40,
                cache_read: 20,
                cache_creation: 5,
                usd: 0.1234,
            }],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "sessionCost");
        assert_eq!(v["session_pk"], "s1");
        assert_eq!(v["total_usd"], 0.1234);
        assert_eq!(v["models"][0]["model"], "claude-sonnet-4");
        assert_eq!(v["models"][0]["cacheRead"], 20);
        assert_eq!(v["models"][0]["cacheCreation"], 5);
        assert_eq!(v["models"][0]["usd"], 0.1234);
    }
}
