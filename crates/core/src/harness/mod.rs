use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, McpServerSpec, PermMode, Principal, SessionKind};
use crate::store::Store;
use async_trait::async_trait;

pub mod native;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Everything a harness needs to run one session. Built by the control plane
/// (Spec 3 wiring) and passed to `Harness::start_session`.
pub struct SessionCtx {
    pub session_pk: String,
    /// The owning project, if any — `None` for a chat-first (project-less)
    /// session. Mirrors `Session.project_id`; harness backends key
    /// project-scoped features (e.g. the native runtime's persistent memory
    /// and tool-policy lookups) off this rather than re-querying the store.
    pub project_id: Option<String>,
    /// The session's kind (`Project`, `Chat`, `Worker`, `Review`), mirroring
    /// `Session.kind`.
    pub kind: SessionKind,
    /// Which agent persona/config is driving this session, if any. Mirrors
    /// `Session.agent`; unused for `Project` sessions today.
    pub agent: Option<String>,
    pub work_dir: PathBuf,
    /// The session's attachment folder (`…/.harness-attachments/{session_pk}`)
    /// — a second read root the native runtime's `read` tool tries when the
    /// worktree jail rejects a path. `None` when the harness doesn't
    /// materialize attachments to disk (or in bare test contexts).
    pub attachments_dir: Option<PathBuf>,
    pub perm_mode: PermMode,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Agent session id to resume, if any.
    pub resume: Option<String>,
    /// MCP servers to attach (from the connector axis).
    pub mcp_servers: Vec<McpServerSpec>,
    /// `McpServerSpec.name` → the plugin that attached that server, for
    /// every server in `mcp_servers` sourced from a connector plugin (built
    /// in `ControlPlane::attach_plugin_mcp_servers`, keyed at the same
    /// binding site the servers themselves are resolved). A DB-configured
    /// server (no plugin) simply has no entry here. The native runtime looks
    /// this up per `mcp__<server>__<tool>` tool so approvals can attribute
    /// the call to its plugin (see [`crate::domain::Principal`]).
    pub mcp_principals: HashMap<String, Principal>,
    /// Extra skill directories contributed by enabled user plugins (see
    /// `crate::plugins::PluginHost::enabled_skill_dirs`), on top of the
    /// native runtime's usual worktree/global skill dirs.
    pub extra_skill_dirs: Vec<PathBuf>,
    /// Live handle to the daemon's extension host (Track D) — every hook
    /// fire site (`harness::native::hooks::fire_hook`) dispatches to it
    /// alongside the on-disk script sink. `None` when the daemon has no
    /// extension-capable plugins spawned (the common case, and every bare
    /// test `SessionCtx`): every fire site then behaves exactly as it did
    /// before Track D existed — see `ControlPlane::start_harness_session`
    /// and `plugins::extension::ExtensionHost::is_empty`. A live handle, not
    /// config, so it is never serialized.
    pub extension_events: Option<Arc<dyn crate::plugins::extension::ExtensionEvents>>,
    /// Sibling accessor to `extension_events`, threaded from the SAME
    /// daemon-global extension host (Track D, DT6) — `None` under the exact
    /// same condition (`ExtensionHost::is_empty`), so a session with no
    /// extensions spawned builds its tool registry with zero extra work,
    /// exactly like `extension_events: None` keeps every hook fire site a
    /// true no-op. The native runtime's session start (mirroring
    /// `connect_mcp_tools`) calls `session_tools()` through this to gather
    /// every `Running`, `provides_tools` extension's tools and wrap them as
    /// native `Tool`s via `harness::native::tools::extension::ExtensionTool`.
    pub extension_tools: Option<Arc<dyn crate::plugins::extension::ExtensionTools>>,
    /// Event bus for normalized session output.
    pub events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub for tool-permission requests.
    pub approvals: Arc<ApprovalHub>,
    /// Shared async-delegation capacity gate (spec §6.2). Populated from
    /// `ControlPlane::background`; the native runner's `task` tool uses it to
    /// bound `background: true` delegations against `max_concurrent_runs`.
    pub background: Arc<crate::harness::native::background::BackgroundRegistry>,
    /// Persistence handle (transcript rows, agent_session_id updates).
    pub store: Arc<Store>,
    /// Curated app-control facade (spec §9.1), built by the control plane
    /// only for a top-level interactive session (`kind` is `Project` or
    /// `Chat`). `None` for worker/review sessions and any bare test context —
    /// the native runtime then disables the `app_*` tools on `ToolCtx.app`.
    pub app_control: Option<Arc<dyn native::tools::AppControl>>,
}

/// A registered agent runtime. `NativeHarness` (see `harness::native`) is the
/// only production implementation; the trait boundary otherwise exists so
/// tests can substitute fakes.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Begin (or resume) a session; returns a live handle for its lifetime.
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>>;
}

/// One turn's prompt, split into the agent-visible and display strings.
///
/// `prepare_attachments` may decorate the raw user text with an attachment
/// manifest before it reaches the agent — but that decorated text must never
/// be what durable history (and thus the cockpit UI) shows as "what the user
/// typed". `agent` is what's sent to the harness/agent (possibly
/// manifest-decorated); `display` is the raw text to persist as the
/// `"user"/"text"` transcript row.
#[derive(Debug, Clone, Default)]
pub struct TurnPrompt {
    /// Sent to the harness/agent — may be decorated (e.g. with an attachment
    /// manifest).
    pub agent: String,
    /// Persisted as the durable `"user"/"text"` transcript row — the raw text
    /// the user actually typed.
    pub display: String,
    /// Anthropic content blocks prepended before the text block in the user
    /// turn (today: base64 image blocks built from attachments). Consumed by
    /// the native runner.
    pub blocks: Vec<serde_json::Value>,
    /// Display metadata persisted on the user transcript row —
    /// `[{name, path, contentType, size}]` per saved attachment.
    pub attachments: Vec<serde_json::Value>,
}

impl TurnPrompt {
    /// A plain-text prompt with no attachment blocks/metadata.
    pub fn text(agent: impl Into<String>, display: impl Into<String>) -> Self {
        TurnPrompt {
            agent: agent.into(),
            display: display.into(),
            ..Default::default()
        }
    }
}

/// A live session driven by a `Harness`. Output is emitted via `SessionCtx.events`.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()>;
    async fn cancel(&self) -> anyhow::Result<()>;
    async fn end(&self) -> anyhow::Result<()>;
    fn agent_session_id(&self) -> Option<String>;

    /// Update the live permission mode for subsequent turns. Default no-op;
    /// the native session overrides this (see its `RunnerDeps`).
    fn set_perm_mode(&self, _mode: PermMode) {}

    /// Buffer a message sent while a turn is already running, for injection
    /// into that turn's next tool-result batch (Task B3). Default no-op: only
    /// the in-process native runtime has an in-flight loop to steer — the ACP
    /// harness delegates the whole turn to an external agent process with no
    /// such mid-turn channel, so a steer call on it is silently dropped.
    fn steer(&self, _text: String) {}
}

/// Builds a `Harness`. The factory instance carries host-injected config,
/// so `create` takes no arguments.
pub trait HarnessFactory: Send + Sync {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSession;
    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            None
        }
    }

    struct FakeHarness;
    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession))
        }
    }

    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    async fn make_ctx() -> SessionCtx {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let (events, _rx) = broadcast::channel(16);
        SessionCtx {
            session_pk: "s1".into(),
            project_id: None,
            kind: SessionKind::Chat,
            agent: None,
            work_dir: PathBuf::from("/tmp"),
            attachments_dir: None,
            perm_mode: PermMode::Default,
            model: None,
            effort: None,
            resume: None,
            mcp_servers: vec![],
            mcp_principals: HashMap::new(),
            extra_skill_dirs: vec![],
            extension_events: None,
            extension_tools: None,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            background: crate::harness::native::background::BackgroundRegistry::new(),
            store,
            app_control: None,
        }
    }

    #[tokio::test]
    async fn factory_produces_a_harness_that_starts_a_working_session() {
        let factory = FakeHarnessFactory;
        let harness = factory.create().unwrap();
        let session = harness.start_session(make_ctx().await).await.unwrap();
        assert!(session.agent_session_id().is_none());
        session
            .send_prompt(TurnPrompt::text("hello", "hello"))
            .await
            .unwrap();
    }
}
