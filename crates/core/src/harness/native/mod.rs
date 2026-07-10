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
pub mod context;
pub mod context_manager;
pub mod cost;
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
/// tool wrappers for their tools. Servers connect CONCURRENTLY (`join_all` —
/// each stdio handshake is independent), so total startup latency is the
/// slowest server, not the sum. Failures are logged and skipped; `join_all`
/// preserves input order, so tool order stays deterministic.
async fn connect_mcp_tools(
    mcp_servers: &[crate::domain::McpServerSpec],
) -> Vec<Arc<dyn tools::Tool>> {
    let connections = futures::future::join_all(mcp_servers.iter().map(|spec| async move {
        if !matches!(spec.transport, crate::domain::McpTransport::Stdio { .. }) {
            return None; // HTTP MCP transport is not yet executed natively
        }
        match mcp_client::McpConnection::connect_stdio(spec).await {
            Ok(conn) => Some(Arc::new(conn)),
            Err(e) => {
                tracing::warn!("native: MCP server `{}` unavailable: {e}", spec.name);
                None
            }
        }
    }))
    .await;
    let mut extra: Vec<Arc<dyn tools::Tool>> = Vec::new();
    for conn in connections.into_iter().flatten() {
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
        // route/model when a stale project pins a target no connection
        // actually serves anymore.
        let model = resolve_native_model(&ctx.store, ctx.model).await;
        let meta =
            crate::llm_router::model_meta::resolve(&ctx.store, model.as_deref().unwrap_or(""))
                .await;
        crate::llm_router::model_meta::spawn_refresh();
        // Discover agents + slash commands from the worktree (and global config).
        let agents = Arc::new(agents::AgentRegistry::load(&ctx.work_dir));
        let commands = Arc::new(commands::CommandRegistry::load(&ctx.work_dir));
        let agent = agents.default_agent();
        // Connect MCP servers and expose their tools; the wrapping Arcs keep the
        // connections alive for the session's lifetime.
        let mcp_tools = connect_mcp_tools(&ctx.mcp_servers).await;
        let tools = Arc::new(tools::ToolRegistry::with_extra(mcp_tools));
        // Persistent memory is unconditional: a chat (project-less) session
        // still gets GLOBAL memory, while a project session gets global +
        // project scope. `at_default(None)` sets the global path and leaves
        // the project path unset — global memory works, project-scope ops
        // error cleanly — so previously skipping `MemoryStore` entirely for
        // `project_id: None` needlessly denied chat sessions memory. Tool-
        // policy lookups (below, via `RunnerDeps::project_id`) stay
        // project-scoped and off without a project — chat sessions have no
        // project to scope a `tool_policies` row to.
        let project_id = ctx.project_id.clone();
        let memory_store = Some(Arc::new(memory::MemoryStore::at_default(
            project_id.as_deref(),
        )));
        Ok(Box::new(NativeSession {
            session_pk: ctx.session_pk.clone(),
            deps: runner::RunnerDeps {
                session_pk: ctx.session_pk,
                work_dir: ctx.work_dir,
                extra_skill_dirs: ctx.extra_skill_dirs,
                model,
                effort: ctx.effort,
                meta,
                perm_mode: Arc::new(std::sync::Mutex::new(ctx.perm_mode)),
                project_id,
                perm_overrides: Arc::new(std::sync::Mutex::new(Default::default())),
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
            turn_lock: tokio::sync::Mutex::new(()),
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
    /// Serializes turns: two concurrent `send_prompt`s (double-send, gateway +
    /// UI race) must never interleave their `provider_turns` appends, or the
    /// ledger's user/assistant alternation — and its tool_use/tool_result
    /// pairing — breaks durably.
    turn_lock: tokio::sync::Mutex<()>,
}

#[async_trait]
impl HarnessSession for NativeSession {
    async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
        // One turn at a time per session. A queued second prompt simply waits;
        // `cancel()` trips only the CURRENT turn's token (the queued turn gets
        // a fresh one when it starts).
        let _turn = self.turn_lock.lock().await;
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
            name: "Ryuzi".to_string(),
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
            project_id: None,
            kind: crate::domain::SessionKind::Chat,
            agent: None,
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
        assert_eq!(plugin.manifest.name, "Ryuzi");
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
            .send_prompt(TurnPrompt::text("hi", "hi"))
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

    /// Redirect `dirs::home_dir()`/`dirs::data_dir()` into a tempdir for the
    /// duration of a test — `MemoryStore::at_default` resolves under
    /// `~/.config/ryuzi/memory`, so a test that exercises it for real must
    /// not touch the developer's actual home directory. Process-global env,
    /// so every test using this needs `#[serial]` (mirrors
    /// `control::tests::StateDirGuard`).
    struct StateDirGuard {
        _dir: tempfile::TempDir,
    }
    impl StateDirGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("HOME", dir.path());
            StateDirGuard { _dir: dir }
        }
    }

    /// The actual wiring bug this task fixes: a chat (project-less) session
    /// previously skipped `MemoryStore` construction entirely (`project_id:
    /// None` short-circuited it in `NativeHarness::start_session`), so a
    /// fact saved by one chat session was invisible to the next. Seed the
    /// GLOBAL memory file `at_default(None)` resolves to, start a session
    /// through the real `Harness` trait with `ctx.project_id: None` (as
    /// `ctx_for` now sets), and confirm the seeded entry reaches the first
    /// request's system prompt exactly like `memory_snapshot_reaches_
    /// primary_system_but_not_subagents` proves it does for a project
    /// session in `runner.rs`.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_session_without_a_project_still_gets_global_memory() {
        use runner::testutil::{message_delta, message_stop, text_delta, RecordingLlm};
        let _guard = StateDirGuard::new();
        memory::MemoryStore::at_default(None)
            .add(
                memory::MemoryScope::Global,
                "the deploy key lives in 1Password",
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());

        let llm = Arc::new(RecordingLlm::new(vec![vec![
            text_delta("ok"),
            message_delta("end_turn"),
            message_stop(),
        ]]));
        struct OneShotFactory(Arc<RecordingLlm>);
        impl llm::LlmStreamFactory for OneShotFactory {
            fn create(&self, _store: Arc<Store>) -> Arc<dyn llm::LlmStream> {
                self.0.clone()
            }
        }
        let plugin = native_plugin_with_llm_factory(Arc::new(OneShotFactory(llm.clone())));
        let harness = plugin.harness.unwrap().create().unwrap();
        // ctx_for's SessionCtx carries project_id: None — the chat-session shape.
        let session = harness
            .start_session(ctx_for(store.clone(), dir.path().to_path_buf()).await)
            .await
            .unwrap();
        session
            .send_prompt(TurnPrompt::text("hi", "hi"))
            .await
            .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        let system = bodies[0]["system"].as_str().unwrap_or_default();
        assert!(
            system.contains("the deploy key lives in 1Password"),
            "{system}"
        );
        assert!(system.contains("# Persistent memory (global)"), "{system}");
    }

    #[tokio::test]
    async fn concurrent_prompts_on_one_session_are_serialized() {
        use runner::testutil::{message_delta, message_stop, text_delta};
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Holds each provider stream open ~100ms and records how many
        /// streams were ever active at once: >1 means two turns interleaved
        /// their provider calls (and therefore their ledger appends).
        struct OverlapLlm {
            active: Arc<AtomicUsize>,
            max_seen: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl llm::LlmStream for OverlapLlm {
            async fn stream(
                &self,
                _body: serde_json::Value,
            ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<AnthropicEvent>>>
            {
                let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_seen.fetch_max(n, Ordering::SeqCst);
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                let active = self.active.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = tx.send(Ok(text_delta("ok"))).await;
                    let _ = tx.send(Ok(message_delta("end_turn"))).await;
                    // Mark the stream finished BEFORE the terminal event: a
                    // serialized follow-up turn can only start after
                    // message_stop is consumed, so it never counts as overlap.
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ = tx.send(Ok(message_stop())).await;
                });
                Ok(rx)
            }
        }

        struct SharedFactory(Arc<OverlapLlm>);
        impl llm::LlmStreamFactory for SharedFactory {
            fn create(&self, _store: Arc<Store>) -> Arc<dyn llm::LlmStream> {
                self.0.clone()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let overlap = Arc::new(OverlapLlm {
            active: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
        });
        let plugin = native_plugin_with_llm_factory(Arc::new(SharedFactory(overlap.clone())));
        let harness = plugin.harness.unwrap().create().unwrap();
        let session = harness
            .start_session(ctx_for(store.clone(), dir.path().to_path_buf()).await)
            .await
            .unwrap();

        // Two prompts land on the SAME session at the same time (double-send,
        // UI + gateway race, boot-nudge racing a user prompt).
        let (ra, rb) = tokio::join!(
            session.send_prompt(TurnPrompt::text("one", "one")),
            session.send_prompt(TurnPrompt::text("two", "two")),
        );
        ra.unwrap();
        rb.unwrap();

        assert_eq!(
            overlap.max_seen.load(Ordering::SeqCst),
            1,
            "turns must not interleave provider calls"
        );
        // The durable ledger alternates cleanly: two complete turns in a row.
        let turns = store.list_provider_turns("sess").await.unwrap();
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
    }

    fn conn_for_resolution_tests(
        id: &str,
        provider: &str,
        model: &str,
    ) -> crate::llm_router::connections::ConnectionRow {
        use crate::llm_router::connections::{ConnectionData, ConnectionRow};
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

    #[tokio::test]
    async fn native_model_resolution_serves_a_configured_codex_model_directly() {
        // Codex (openai-oauth) is drivable on the native path now (via
        // `codex_stream`), so a project pinned to it resolves directly
        // instead of falling back to the default route.
        use crate::llm_router::connections;
        use crate::llm_router::routes::{
            self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget,
        };

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        connections::add_connection(
            &store,
            conn_for_resolution_tests("chatgpt", "openai-oauth", "gpt-5.2-codex"),
        )
        .await
        .unwrap();
        connections::add_connection(
            &store,
            conn_for_resolution_tests("claude", "anthropic", "claude-sonnet-4-5"),
        )
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
                    provider: "anthropic".into(),
                    model: "claude-sonnet-4-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_native_model(&store, Some("openai/gpt-5.2-codex".into()))
                .await
                .as_deref(),
            Some("openai/gpt-5.2-codex")
        );
    }

    #[tokio::test]
    async fn native_model_resolution_falls_back_from_an_unresolvable_model() {
        // A configured model that no enabled connection actually serves
        // (stale project config, renamed/removed connection, ...) still
        // falls back to the default native model.
        use crate::llm_router::connections;
        use crate::llm_router::routes::{
            self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget,
        };

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        connections::add_connection(
            &store,
            conn_for_resolution_tests("chatgpt", "openai-oauth", "gpt-5.2-codex"),
        )
        .await
        .unwrap();
        connections::add_connection(
            &store,
            conn_for_resolution_tests("claude", "anthropic", "claude-sonnet-4-5"),
        )
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
                    provider: "anthropic".into(),
                    model: "claude-sonnet-4-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_native_model(&store, Some("openai/gpt-9-does-not-exist".into()))
                .await
                .as_deref(),
            Some("fable")
        );
    }

    #[tokio::test]
    async fn connect_mcp_tools_skips_http_and_unreachable_servers() {
        use crate::domain::{McpServerSpec, McpTransport};
        // One HTTP spec (not executed natively) + two stdio specs whose
        // commands don't exist (spawn fails fast, no real process). The
        // joined connect must complete and yield no tools — failures are
        // logged and skipped, never propagated.
        let specs = vec![
            McpServerSpec {
                name: "http-server".into(),
                transport: McpTransport::Http {
                    url: "http://localhost:1/mcp".into(),
                    headers: vec![],
                },
            },
            McpServerSpec {
                name: "ghost-a".into(),
                transport: McpTransport::Stdio {
                    command: "ryuzi-definitely-not-a-real-binary-a".into(),
                    args: vec![],
                    env: vec![],
                },
            },
            McpServerSpec {
                name: "ghost-b".into(),
                transport: McpTransport::Stdio {
                    command: "ryuzi-definitely-not-a-real-binary-b".into(),
                    args: vec![],
                    env: vec![],
                },
            },
        ];
        let tools = connect_mcp_tools(&specs).await;
        assert!(tools.is_empty());
    }
}
