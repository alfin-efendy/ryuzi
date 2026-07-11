use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, McpServerSpec, PermMode};
use crate::store::Store;
use async_trait::async_trait;

pub mod native;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Everything a harness needs to run one session. Built by the control plane
/// (Spec 3 wiring) and passed to `Harness::start_session`.
pub struct SessionCtx {
    pub session_pk: String,
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
    /// Extra skill directories contributed by enabled user plugins (see
    /// `crate::plugins::PluginHost::enabled_skill_dirs`), on top of the
    /// native runtime's usual worktree/global skill dirs.
    pub extra_skill_dirs: Vec<PathBuf>,
    /// Event bus for normalized session output.
    pub events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub for tool-permission requests.
    pub approvals: Arc<ApprovalHub>,
    /// Persistence handle (transcript rows, agent_session_id updates).
    pub store: Arc<Store>,
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
            work_dir: PathBuf::from("/tmp"),
            attachments_dir: None,
            perm_mode: PermMode::Default,
            model: None,
            effort: None,
            resume: None,
            mcp_servers: vec![],
            extra_skill_dirs: vec![],
            events,
            approvals: Arc::new(ApprovalHub::new()),
            store,
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
