//! Extension runtime (Track D): supervised subprocess "code plugins" that
//! speak a JSON-RPC protocol over stdio (see [`protocol`]), following the
//! same subprocess-isolation model Ryuzi already uses for MCP servers
//! (`harness::native::mcp_client`) — hardened with `env_clear()` + an
//! explicit env allowlist and a bounded handshake timeout, and made
//! non-fatal: a failed/mismatched extension is recorded as
//! [`ExtensionStatus::Failed`], never a daemon-fatal error.
//!
//! **NEVER in-process plugin code**: every extension is a subprocess. This
//! module contains no mechanism to load or execute plugin-supplied code any
//! other way.
//!
//! **Scope of this slice (DT3):** the `extension` capability axis on
//! `CorePlugin` (`plugins::host`), the manifest → [`ExtensionSpec`] binding
//! (`DeclarativeExtension` in `plugins::declarative`, mirroring
//! `DeclarativeConnector`), and [`proc::ExtensionProc`]/[`proc::ExtensionHost`]
//! covering spawn + handshake + graceful shutdown for one subprocess. Every
//! type here is shaped so the later slices only ADD behavior:
//! - **DT4 (supervision)**: health via `extension/ping`
//!   ([`protocol::METHOD_PING`], reserved but unused here) and
//!   restart-with-backoff — add an `ExtensionStatus::Restarting` variant and
//!   a loop owned by `ExtensionHost`.
//! - **DT5 (event dispatch)**: `event/<name>` notifications
//!   ([`protocol::METHOD_EVENT_PREFIX`]) fanned out to every `ExtensionProc`
//!   whose `confirmed_events` includes the firing `HookEvent`, using the
//!   proc's already-open stdin/reader; `ExtensionSpec::timeout` is the
//!   per-event budget that dispatch enforces.
//! - **DT6 (tool provision)**: wraps `ExtensionProc::tools` (raw `Value`s
//!   from `protocol::InitializeAck::tools`) into an `ExtensionTool: Tool`
//!   dispatching `tool/call` over the same pipe, the same way `McpTool`
//!   wraps `McpConnection`.

pub mod proc;
pub mod protocol;

use std::time::Duration;

use async_trait::async_trait;

use crate::harness::native::hooks::HookEvent;
use crate::settings::SettingsStore;

pub use proc::{ExtensionHost, ExtensionProc};
pub use protocol::PROTOCOL_VERSION;

/// Per-event dispatch budget an `[[extension]]` manifest entry gets when it
/// omits `timeout_ms`. Distinct from [`proc::INIT_HANDSHAKE_TIMEOUT`] — that
/// one bounds the one-time startup handshake; this bounds a single
/// `event/<name>` round trip (DT5's concern). Matches the design doc's own
/// `[[extension]]` example (`timeout_ms = 5000`).
pub const DEFAULT_EVENT_TIMEOUT_MS: u64 = 5_000;

/// A resolved, ready-to-spawn extension: every `${...}` placeholder in
/// `command`/`args` has already been substituted (mirrors how
/// `declarative::build_spec` turns an `McpServerDef` into an
/// `McpServerSpec`), and `events` has been parsed from the manifest's raw
/// strings into typed [`HookEvent`]s (already validated against the known
/// vocabulary by `PluginManifest::validate`).
#[derive(Debug, Clone)]
pub struct ExtensionSpec {
    /// The manifest's `[[extension]] name` — unique within its own plugin's
    /// `extensions` list, NOT globally (mirrors `ExtensionDef::name`'s own
    /// namespace note in the SDK).
    pub name: String,
    /// The stdio binary to spawn (already `${...}`-resolved).
    pub command: String,
    pub args: Vec<String>,
    /// Hook events this extension subscribes to, parsed from the manifest's
    /// validated `events: Vec<String>`.
    pub events: Vec<HookEvent>,
    /// If true, the host queries this extension for tool definitions at
    /// init (DT6 wires the result into a session's tool registry).
    pub provides_tools: bool,
    /// Per-event dispatch budget (DT5). NOT the handshake timeout — see
    /// [`proc::INIT_HANDSHAKE_TIMEOUT`].
    pub timeout: Duration,
    /// Extra environment variables this specific extension is allowed to
    /// receive, beyond the minimal safe base (`proc`'s
    /// `SAFE_BASE_ENV_VARS`) — the allowlist half of the `env_clear()` +
    /// allowlist security model (see `proc`'s module doc). Always empty for
    /// every manifest-declared extension today:
    /// `ryuzi_plugin_sdk::ExtensionDef` has no `env` table (unlike
    /// `McpServerDef`), so `${auth}`/`${setting:KEY}` can only appear in
    /// `command`/`args`, never injected as an env var, for now. This field
    /// exists so a future SDK `[[extension]].env` table (or a Rust built-in
    /// implementing `ExtensionFactory` directly) has somewhere to put
    /// declared secrets without another `ExtensionSpec` shape change.
    pub env: Vec<(String, String)>,
}

/// Settings access an [`ExtensionFactory`] needs to resolve
/// `${setting:KEY}`/`${auth}` placeholders. Deliberately narrower than
/// `connector::ConnectorCtx` (no `project_id`/`work_dir`): extensions are
/// spawned once per daemon lifetime, not per session (see the design doc's
/// "Spawn on daemon start ... one long-lived process per extension, not
/// per-session"), so there is no session/project to scope them to.
#[derive(Clone)]
pub struct ExtensionCtx {
    pub settings: SettingsStore,
}

/// Something that can produce the resolved [`ExtensionSpec`]s for one
/// plugin — mirrors `connector::Connector::mcp_servers`.
/// `plugins::declarative::DeclarativeExtension` is the only implementor in
/// this slice; a Rust built-in could implement this directly the same way a
/// built-in can implement `Connector` directly.
#[async_trait]
pub trait ExtensionFactory: Send + Sync {
    async fn extensions(&self, ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>>;
}

/// One extension subprocess's lifecycle state.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionStatus {
    /// Spawned; the `extension/initialize` handshake has not resolved yet.
    Starting,
    /// Handshake succeeded — `confirmed_events`/`tools` are populated.
    Running,
    /// Spawn, handshake, or protocol negotiation failed. Carries a
    /// sanitized (secret-free) reason — see `proc`'s `sanitize_init_error`
    /// — safe to surface in `plugin_doctor`/Cockpit. Never fatal to the
    /// daemon: the rest of the plugin host keeps running.
    Failed(String),
    /// [`proc::ExtensionProc::shutdown`] completed (or the process was
    /// never started / had already failed before ever running).
    Stopped,
}
