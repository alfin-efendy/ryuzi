use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, McpServerSpec, PermMode};
use crate::registry::Registry;
use crate::store::Store;
use async_trait::async_trait;

pub mod acp;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Everything a harness needs to run one session. Built by the control plane
/// (Spec 3 wiring) and passed to `Harness::start_session`.
pub struct SessionCtx {
    pub session_pk: String,
    pub work_dir: PathBuf,
    pub perm_mode: PermMode,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Agent session id to resume, if any.
    pub resume: Option<String>,
    /// MCP servers to attach (from the connector axis).
    pub mcp_servers: Vec<McpServerSpec>,
    /// Event bus for normalized session output.
    pub events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub for tool-permission requests.
    pub approvals: Arc<ApprovalHub>,
    /// Persistence handle (transcript rows, agent_session_id updates).
    pub store: Arc<Store>,
}

/// A registered agent runtime (e.g. Claude Code via ACP in Spec 3).
#[async_trait]
pub trait Harness: Send + Sync {
    /// Begin (or resume) a session; returns a live handle for its lifetime.
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>>;
}

/// A live session driven by a `Harness`. Output is emitted via `SessionCtx.events`.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    async fn send_prompt(&self, prompt: String) -> anyhow::Result<()>;
    async fn cancel(&self) -> anyhow::Result<()>;
    async fn end(&self) -> anyhow::Result<()>;
    fn agent_session_id(&self) -> Option<String>;
}

/// Builds a `Harness`. The factory instance carries host-injected config
/// (e.g. the ACP adapter path in Spec 3), so `create` takes no arguments.
pub trait HarnessFactory: Send + Sync {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>>;
}

/// name → `HarnessFactory`. Keyed by `Project.harness`.
pub type HarnessRegistry = Registry<dyn HarnessFactory>;

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSession;
    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn send_prompt(&self, _prompt: String) -> anyhow::Result<()> {
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
            perm_mode: PermMode::Default,
            model: None,
            effort: None,
            resume: None,
            mcp_servers: vec![],
            events,
            approvals: Arc::new(ApprovalHub::new()),
            store,
        }
    }

    #[test]
    fn registry_resolves_harness_factory_by_name() {
        let mut reg: HarnessRegistry = HarnessRegistry::new();
        reg.register("claude-code", Arc::new(FakeHarnessFactory));
        reg.register("codex", Arc::new(FakeHarnessFactory));
        assert!(reg.get("claude-code").is_some());
        assert!(reg.get("unknown").is_none());
        assert_eq!(
            reg.names(),
            vec!["claude-code".to_string(), "codex".to_string()]
        );
    }

    #[tokio::test]
    async fn factory_produces_a_harness_that_starts_a_working_session() {
        let factory = FakeHarnessFactory;
        let harness = factory.create().unwrap();
        let session = harness.start_session(make_ctx().await).await.unwrap();
        assert!(session.agent_session_id().is_none());
        session.send_prompt("hello".into()).await.unwrap();
    }
}
