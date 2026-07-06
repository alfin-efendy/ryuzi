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

pub mod agents;
pub mod commands;
pub mod compaction;
pub mod context;
pub mod format;
pub mod hooks;
pub mod ledger;
pub mod llm;
pub mod lsp;
pub mod mcp_client;
pub mod memory;
pub mod permission;
pub mod runner;
pub mod skills;
pub mod snapshot;
pub mod tools;

use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
use crate::plugins::{CorePlugin, PluginSource};
use async_trait::async_trait;
use ryuzi_plugin_sdk::PluginManifest;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The native runtime harness id, stored in `projects.harness`.
pub const NATIVE_ID: &str = "native";

/// The native agent runtime as a [`Harness`]. Each session runs the agentic
/// loop in-process via [`runner::run_turn`].
pub struct NativeHarness {
    /// Factory for the LLM stream. Overridable in tests to script conversations.
    llm_factory: Arc<dyn llm::LlmStreamFactory>,
}

impl NativeHarness {
    pub fn new() -> Self {
        NativeHarness {
            llm_factory: Arc::new(llm::RouterLlmStreamFactory),
        }
    }

    /// Construct with a custom LLM stream factory (used by tests).
    pub fn with_llm_factory(llm_factory: Arc<dyn llm::LlmStreamFactory>) -> Self {
        NativeHarness { llm_factory }
    }
}

impl Default for NativeHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// Connect the session's enabled MCP servers (stdio only) and build native
/// tool wrappers for their tools. Failures are logged and skipped.
async fn connect_mcp_tools(
    mcp_servers: &[crate::domain::McpServerSpec],
) -> Vec<Arc<dyn tools::Tool>> {
    let mut extra: Vec<Arc<dyn tools::Tool>> = Vec::new();
    for spec in mcp_servers {
        if !matches!(spec.transport, crate::domain::McpTransport::Stdio { .. }) {
            continue; // HTTP MCP transport is not yet executed natively
        }
        match mcp_client::McpConnection::connect_stdio(spec).await {
            Ok(conn) => {
                let conn = Arc::new(conn);
                for t in &conn.tools {
                    extra.push(Arc::new(tools::mcp::McpTool::new(
                        &conn.server_name,
                        &t.name,
                        &t.description,
                        t.input_schema.clone(),
                        conn.clone(),
                    )));
                }
            }
            Err(e) => tracing::warn!("native: MCP server `{}` unavailable: {e}", spec.name),
        }
    }
    extra
}

async fn resolve_native_model(
    store: &crate::store::Store,
    configured: Option<String>,
) -> Option<String> {
    if let Some(model) = configured.filter(|m| !m.trim().is_empty()) {
        if crate::llm_router::client::route_model_for_anthropic_messages(store, &model)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return Some(model);
        }
    }
    crate::llm_router::client::default_anthropic_messages_model(store).await
}

#[async_trait]
impl Harness for NativeHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        let llm = self.llm_factory.create(ctx.store.clone());
        // Native speaks Anthropic Messages internally; resolve configured
        // routes/models through that capability and fall back to a compatible
        // route/model when a stale project pins a Responses-only target.
        let model = resolve_native_model(&ctx.store, ctx.model).await;
        // Discover agents + slash commands from the worktree (and global config).
        let agents = Arc::new(agents::AgentRegistry::load(&ctx.work_dir));
        let commands = Arc::new(commands::CommandRegistry::load(&ctx.work_dir));
        let agent = agents.default_agent();
        // Connect MCP servers and expose their tools; the wrapping Arcs keep the
        // connections alive for the session's lifetime.
        let mcp_tools = connect_mcp_tools(&ctx.mcp_servers).await;
        let tools = Arc::new(tools::ToolRegistry::with_extra(mcp_tools));
        // Persistent memory keys off the session's project; without a session
        // row (bare tests) the feature is simply off, keeping runs hermetic.
        let memory_store = match ctx.store.get_session(&ctx.session_pk).await {
            Ok(Some(session)) => Some(Arc::new(memory::MemoryStore::at_default(Some(
                &session.project_id,
            )))),
            _ => None,
        };
        Ok(Box::new(NativeSession {
            session_pk: ctx.session_pk.clone(),
            deps: runner::RunnerDeps {
                session_pk: ctx.session_pk,
                work_dir: ctx.work_dir,
                extra_skill_dirs: ctx.extra_skill_dirs,
                model,
                perm_mode: Arc::new(std::sync::Mutex::new(ctx.perm_mode)),
                project_policy: None,
                store: ctx.store,
                events: ctx.events,
                approvals: ctx.approvals,
                llm,
                tools,
                agent,
                agents,
                commands,
                memory: memory_store,
                snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
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

    fn set_perm_mode(&self, mode: crate::domain::PermMode) {
        // Live update: the next turn's tool gate reads this fresh, so a
        // composer/project-settings permission change applies without a restart.
        self.deps.set_perm_mode(mode);
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

/// The `native` built-in plugin: harness-only, no external binary — the
/// native runtime runs the agentic loop in-process (see the module doc).
pub fn native_plugin() -> CorePlugin {
    native_plugin_with_llm_factory(Arc::new(llm::RouterLlmStreamFactory))
}

/// Construct with a custom LLM stream factory (used by tests, mirroring the
/// old `NativeIntegration::with_llm_factory` seam).
pub fn native_plugin_with_llm_factory(llm_factory: Arc<dyn llm::LlmStreamFactory>) -> CorePlugin {
    CorePlugin {
        manifest: PluginManifest {
            contract: 1,
            id: NATIVE_ID.to_string(),
            name: "Native (ryuzi)".to_string(),
            version: "0.0.0".to_string(),
            publisher: "ryuzi".to_string(),
            description: "Ryuzi's built-in agent runtime — runs the loop and tools in-process, using your configured model providers".to_string(),
            homepage: None,
            icon: Some("cpu".to_string()),
            categories: vec!["runtime".to_string()],
            verified: true,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: None,
            runtime: None,
        },
        harness: Some(Arc::new(NativeHarnessFactory::with_llm_factory(
            llm_factory,
        ))),
        gateway: None,
        connector: None,
        source: PluginSource::Builtin,
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
            extra_skill_dirs: vec![],
            events,
            approvals: Arc::new(ApprovalHub::new()),
            store,
        }
    }

    #[test]
    fn native_plugin_registers_under_native_id() {
        let mut regs = crate::plugins::Registries::new();
        regs.add_plugin(native_plugin());
        assert!(regs.harness.get("native").is_some());
        assert!(regs.gateway.get("native").is_none());
    }

    #[test]
    fn native_plugin_manifest_has_expected_identity() {
        let plugin = native_plugin();
        assert_eq!(plugin.manifest.contract, 1);
        assert_eq!(plugin.manifest.id, "native");
        assert_eq!(plugin.manifest.name, "Native (ryuzi)");
        assert_eq!(plugin.manifest.publisher, "ryuzi");
        assert!(plugin.manifest.verified);
        assert_eq!(plugin.manifest.categories, vec!["runtime".to_string()]);
        assert_eq!(plugin.manifest.icon.as_deref(), Some("cpu"));
        assert!(plugin.harness.is_some());
        assert!(plugin.gateway.is_none());
        assert!(plugin.connector.is_none());
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
        let plugin = native_plugin_with_llm_factory(factory);
        let harness = plugin.harness.unwrap().create().unwrap();
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

    #[tokio::test]
    async fn native_model_resolution_falls_back_from_responses_only_model() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
        use crate::llm_router::routes::{
            self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget,
        };

        fn conn(id: &str, provider: &str, model: &str) -> ConnectionRow {
            let is_oauth = provider.ends_with("oauth");
            ConnectionRow {
                id: id.into(),
                provider: provider.into(),
                auth_type: if is_oauth {
                    "oauth".into()
                } else {
                    "api_key".into()
                },
                label: id.into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: (!is_oauth).then(|| format!("sk-{id}")),
                    access_token: is_oauth.then(|| format!("at-{id}")),
                    models_override: Some(vec![model.into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            }
        }

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        connections::add_connection(&store, conn("chatgpt", "openai-oauth", "gpt-5.2-codex"))
            .await
            .unwrap();
        connections::add_connection(&store, conn("claude", "anthropic", "claude-sonnet-4-5"))
            .await
            .unwrap();
        routes::save_model_route(
            &store,
            ModelRouteInfo {
                id: "r1".into(),
                name: "fable".into(),
                enabled: true,
                strategy: ModelRouteStrategy::Fallback,
                targets: vec![ModelRouteTarget {
                    connection_id: "claude".into(),
                    model: "claude-sonnet-4-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_native_model(&store, Some("openai-oauth/gpt-5.2-codex".into()))
                .await
                .as_deref(),
            Some("fable")
        );
    }
}
