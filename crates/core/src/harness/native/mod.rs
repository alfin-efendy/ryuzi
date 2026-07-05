//! Native agent runtime.
//!
//! Unlike the ACP harness ([`super::acp`]), which delegates all reasoning and
//! tool execution to an external Claude Code adapter process, the native
//! runtime runs the agentic loop in-process: it calls LLMs through
//! [`crate::llm_router::client`], executes its own built-in tools
//! ([`tools`]), enforces permissions ([`permission`]), and persists a
//! provider-turn ledger ([`ledger`]) — registered under the harness id
//! `"native"` beside `"claude-code"`.
//!
//! See `docs/design/2026-07-05-native-agent-runtime-design.md`.

pub mod context;
pub mod ledger;
pub mod llm;
pub mod permission;
pub mod runner;
pub mod tools;

use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
use crate::integration::Integration;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The native runtime harness id, stored in `projects.harness`.
pub const NATIVE_ID: &str = "native";

/// The native agent runtime as a [`Harness`]. Each session runs the agentic
/// loop in-process via [`runner::run_turn`].
pub struct NativeHarness {
    /// Factory for the LLM stream. Overridable in tests to script conversations.
    llm_factory: Arc<dyn llm::LlmStreamFactory>,
    tools: Arc<tools::ToolRegistry>,
}

impl NativeHarness {
    pub fn new() -> Self {
        NativeHarness {
            llm_factory: Arc::new(llm::RouterLlmStreamFactory),
            tools: Arc::new(tools::ToolRegistry::builtin()),
        }
    }

    /// Construct with a custom LLM stream factory (used by tests).
    pub fn with_llm_factory(llm_factory: Arc<dyn llm::LlmStreamFactory>) -> Self {
        NativeHarness {
            llm_factory,
            tools: Arc::new(tools::ToolRegistry::builtin()),
        }
    }
}

impl Default for NativeHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Harness for NativeHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        let llm = self.llm_factory.create(ctx.store.clone());
        Ok(Box::new(NativeSession {
            session_pk: ctx.session_pk.clone(),
            deps: runner::RunnerDeps {
                session_pk: ctx.session_pk,
                work_dir: ctx.work_dir,
                model: ctx.model,
                perm_mode: ctx.perm_mode,
                project_policy: None,
                store: ctx.store,
                events: ctx.events,
                approvals: ctx.approvals,
                llm,
                tools: self.tools.clone(),
            },
            live_cancel: Mutex::new(None),
        }))
    }
}

/// A live native session. `send_prompt` runs one full turn to completion.
pub struct NativeSession {
    deps: runner::RunnerDeps,
    session_pk: String,
    /// The in-flight turn's cancellation token, set for the duration of
    /// `send_prompt` so `cancel`/`end` can trip it.
    live_cancel: Mutex<Option<CancellationToken>>,
}

#[async_trait]
impl HarnessSession for NativeSession {
    async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
        let cancel = CancellationToken::new();
        *self.live_cancel.lock().unwrap() = Some(cancel.clone());
        let result = runner::run_turn(&self.deps, prompt, cancel).await;
        *self.live_cancel.lock().unwrap() = None;
        result
    }

    async fn cancel(&self) -> anyhow::Result<()> {
        if let Some(tok) = self.live_cancel.lock().unwrap().as_ref() {
            tok.cancel();
        }
        Ok(())
    }

    async fn end(&self) -> anyhow::Result<()> {
        // Trip any in-flight turn; there is no external process to tear down.
        if let Some(tok) = self.live_cancel.lock().unwrap().as_ref() {
            tok.cancel();
        }
        Ok(())
    }

    fn agent_session_id(&self) -> Option<String> {
        // The native runtime owns its own history (the provider_turns ledger),
        // so the session_pk is a stable, always-present resume id.
        Some(self.session_pk.clone())
    }
}

/// Builds [`NativeHarness`] instances for the registry.
pub struct NativeHarnessFactory {
    llm_factory: Arc<dyn llm::LlmStreamFactory>,
}

impl NativeHarnessFactory {
    pub fn new() -> Self {
        NativeHarnessFactory {
            llm_factory: Arc::new(llm::RouterLlmStreamFactory),
        }
    }

    pub fn with_llm_factory(llm_factory: Arc<dyn llm::LlmStreamFactory>) -> Self {
        NativeHarnessFactory { llm_factory }
    }
}

impl Default for NativeHarnessFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessFactory for NativeHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(NativeHarness::with_llm_factory(
            self.llm_factory.clone(),
        )))
    }
}

/// The `native` integration: plugs into the harness axis only.
pub struct NativeIntegration {
    factory: Arc<NativeHarnessFactory>,
}

impl NativeIntegration {
    pub fn new() -> Self {
        NativeIntegration {
            factory: Arc::new(NativeHarnessFactory::new()),
        }
    }

    /// Construct with a custom LLM stream factory (used by tests).
    pub fn with_llm_factory(llm_factory: Arc<dyn llm::LlmStreamFactory>) -> Self {
        NativeIntegration {
            factory: Arc::new(NativeHarnessFactory::with_llm_factory(llm_factory)),
        }
    }
}

impl Default for NativeIntegration {
    fn default() -> Self {
        Self::new()
    }
}

impl Integration for NativeIntegration {
    fn id(&self) -> &str {
        NATIVE_ID
    }
    fn harness(&self) -> Option<Arc<dyn HarnessFactory>> {
        Some(self.factory.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalHub;
    use crate::domain::PermMode;
    use crate::llm_router::client::AnthropicEvent;
    use crate::store::Store;
    use tokio::sync::broadcast;

    /// A factory that hands every session the same scripted conversation.
    struct ScriptedFactory {
        turns: Vec<Vec<AnthropicEvent>>,
    }
    impl llm::LlmStreamFactory for ScriptedFactory {
        fn create(&self, _store: Arc<Store>) -> Arc<dyn llm::LlmStream> {
            Arc::new(runner::testutil::ScriptedLlm::new(self.turns.clone()))
        }
    }

    async fn ctx_for(store: Arc<Store>, work_dir: std::path::PathBuf) -> SessionCtx {
        let (events, _rx) = broadcast::channel(64);
        SessionCtx {
            session_pk: "sess".into(),
            work_dir,
            perm_mode: PermMode::BypassPermissions,
            model: Some("test/model".into()),
            effort: None,
            resume: None,
            mcp_servers: vec![],
            events,
            approvals: Arc::new(ApprovalHub::new()),
            store,
        }
    }

    #[test]
    fn native_integration_registers_under_native_id() {
        let mut regs = crate::integration::Registries::new();
        regs.install(&NativeIntegration::new());
        assert!(regs.harness.get("native").is_some());
        assert!(regs.gateway.get("native").is_none());
    }

    #[tokio::test]
    async fn session_runs_a_turn_and_exposes_stable_resume_id() {
        use runner::testutil::{message_delta, message_stop, text_delta};
        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());

        let factory = Arc::new(ScriptedFactory {
            turns: vec![vec![
                text_delta("hello from native"),
                message_delta("end_turn"),
                message_stop(),
            ]],
        });
        let integ = NativeIntegration::with_llm_factory(factory);
        let harness = integ.harness().unwrap().create().unwrap();
        let session = harness
            .start_session(ctx_for(store.clone(), dir.path().to_path_buf()).await)
            .await
            .unwrap();

        assert_eq!(session.agent_session_id().as_deref(), Some("sess"));

        session
            .send_prompt(TurnPrompt {
                agent: "hi".into(),
                display: "hi".into(),
            })
            .await
            .unwrap();

        let msgs = store.list_messages("sess").await.unwrap();
        assert!(msgs
            .iter()
            .any(|m| m.role == "assistant" && m.payload["text"] == "hello from native"));

        // cancel()/end() are safe no-ops when idle.
        session.cancel().await.unwrap();
        session.end().await.unwrap();
    }
}
