use crate::approval::ApprovalHub;
use crate::attachments::{AttachmentFetcher, UreqFetcher};
use crate::automation::{
    AutomationEnvelope, AutomationSource, Dispatcher, HookActionInput, HookOrigin, HookRow,
    RateVerdict, TriggerKind,
};
use crate::domain::{
    ApprovalResponse, CoreEvent, Message, PermMode, Project, Session, ToolPolicyRow,
};
use crate::harness::HarnessSession;
use crate::plugins::extension::{ExtensionCtx, ExtensionHost, SHUTDOWN_GRACE};
use crate::plugins::Registries;
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::{NoopTelemetry, Telemetry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

mod app_control;
mod attachments;
mod lifecycle;
mod provisioning;
#[cfg(test)]
mod tests;

pub use lifecycle::WorkerBinding;
pub use provisioning::{ProvisionProjectRequest, ProvisionSettings};

/// Nudge prompt used when re-driving a turn interrupted by a restart.
pub const RESUME_NUDGE: &str = "Your previous turn was interrupted by a daemon restart or update. \
    Continue the task from where you left off. If it was already complete, briefly summarize what you did.";

/// `node:path`'s `basename` semantics (posix-style, trailing-slash
/// insensitive) applied to a `/`-separated string — used on git URLs, which
/// aren't OS paths, so this deliberately doesn't go through `std::path`.
/// `pub(crate)` so `router.rs`'s `on_connect` can derive the same display
/// name from a bare `git_url`.
pub(crate) fn basename_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

pub struct ControlPlaneAutomationSink(std::sync::Weak<ControlPlane>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundWebhookDispatch {
    pub hook_id: String,
    pub run_id: String,
    pub session_pk: String,
}

#[derive(Debug)]
pub enum InboundWebhookError {
    Invalid(String),
    RateLimited,
    Unavailable(String),
}

impl std::fmt::Display for InboundWebhookError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) | Self::Unavailable(error) => formatter.write_str(error),
            Self::RateLimited => formatter.write_str("automation rate limit exceeded"),
        }
    }
}

impl std::error::Error for InboundWebhookError {}

#[async_trait::async_trait]
impl crate::automation::AutomationEventSink for ControlPlaneAutomationSink {
    async fn observe_lifecycle(
        &self,
        trigger: TriggerKind,
        session_pk: String,
        data: serde_json::Value,
    ) {
        if let Some(control) = self.0.upgrade() {
            control
                .observe_native_automation(trigger, session_pk, data)
                .await;
        }
    }
}

pub struct ControlPlane {
    store: Arc<Store>,
    automation: crate::automation::Dispatcher,
    /// Extension registries (harness slot/gateway/connector/plugins).
    registries: Registries,
    events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub — handed to each `SessionCtx` so the permission
    /// gate can route tool-permission prompts back to the UI.
    approvals: Arc<ApprovalHub>,
    /// Live sessions keyed by `session_pk`. Each value is the harness session
    /// handle returned by `Harness::start_session`, used to drive prompts and to
    /// `cancel`/`end` the session.
    running: Mutex<HashMap<String, Arc<dyn HarnessSession>>>,
    /// Cancellation tokens for sessions still in background startup (git
    /// prep + harness start), keyed by `session_pk`. `stop_session` and
    /// `end_session` cancel the token; the startup task checks it between
    /// phases and aborts cleanly — so a stop that lands before the harness
    /// exists still wins.
    starting: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    /// Telemetry seam (see `crate::telemetry`) — `Noop` unless a daemon wires
    /// up `Console`/OTLP via `new_with_telemetry`.
    telemetry: Arc<dyn Telemetry>,
    /// Downloads Discord (or other gateway) message attachments for
    /// `prepare_attachments`. Real network I/O (`UreqFetcher`) unless a test
    /// injects a fake via `new_full`.
    attachment_fetcher: Arc<dyn AttachmentFetcher>,
    /// One-way latch set by `drain` — once true, `start_session`/
    /// `continue_session` reject new turns.
    draining: std::sync::atomic::AtomicBool,
    /// Count of in-flight turns (incremented synchronously in `spawn_prompt`
    /// before the task is spawned, decremented by `TurnGuard`'s `Drop` inside
    /// the task) — polled by `drain` to know when it's safe to stop waiting.
    active_turns: std::sync::atomic::AtomicUsize,
    /// Shared async-delegation capacity gate (spec §6.2) — one per daemon,
    /// handed to every session's `SessionCtx` so the `n` cap is process-wide.
    background: Arc<crate::harness::native::background::BackgroundRegistry>,
    /// One-way in-memory latch: set once a plugin/skill install, update, or
    /// uninstall has mutated on-disk state that the already-constructed
    /// `registries` above cannot pick up without a process restart. Reset
    /// only by restarting the daemon (deliberately not persisted — the
    /// underlying `Registries` snapshot is also rebuilt from scratch on
    /// every startup, so a stale `true` surviving a restart would be
    /// meaningless).
    plugins_restart_required: std::sync::atomic::AtomicBool,
    /// Builds the `LlmStream` the background review fork (Task 9) drives
    /// through. Bypasses `registries.harness` (an opaque `Harness` trait
    /// object with no way to recover its concrete `LlmStreamFactory`)
    /// because the review fork is a native-runtime-only capability that
    /// talks to `harness::native::runner::drive_review` directly, not
    /// through the generic `Harness` trait. Real
    /// (`RouterLlmStreamFactory`) in production; tests swap in a scripted
    /// stream via `set_review_llm_factory_for_test` to capture and assert on
    /// the exact request body the fork sends.
    review_llm_factory: Mutex<Arc<dyn crate::harness::native::llm::LlmStreamFactory>>,
    /// Track D's extension host — constructed empty here (no real subprocess
    /// spawn; see [`Self::spawn_extensions`]'s hermeticity doc) and shared
    /// as a single `Arc` between the daemon entry (which calls
    /// `spawn_extensions`/`shutdown_extensions`) and every session's
    /// `SessionCtx.extension_events` (threaded in
    /// `lifecycle::start_harness_session`).
    extension_host: Arc<ExtensionHost>,
    /// Shared Plan 2 persistence graph, attached exactly once by the composition
    /// root before any native session starts.
    agent_persistence: std::sync::OnceLock<crate::agents::bootstrap::AgentPersistenceHandles>,
}

impl ControlPlane {
    pub async fn new(store: Store, registries: Registries) -> Arc<ControlPlane> {
        Self::new_with_telemetry(Arc::new(store), registries, Arc::new(NoopTelemetry)).await
    }

    /// Like `new`, but with an explicit telemetry backend — used by the
    /// daemon (Console/OTLP selection) and by tests asserting on emitted
    /// spans/counts.
    ///
    /// Takes an already-shared `Arc<Store>` (rather than an owned `Store`)
    /// so callers that must read settings through a `Store` handle BEFORE
    /// `ControlPlane` exists (e.g. `build_daemon`'s telemetry/harness
    /// selection) can hold and clone the same `Arc` throughout, instead of
    /// juggling ownership back and forth with `Arc::try_unwrap`.
    pub async fn new_with_telemetry(
        store: Arc<Store>,
        registries: Registries,
        telemetry: Arc<dyn Telemetry>,
    ) -> Arc<ControlPlane> {
        Self::new_full(store, registries, telemetry, Arc::new(UreqFetcher)).await
    }

    /// Like `new_with_telemetry`, but with an explicit attachment fetcher —
    /// used by tests that inject a fake fetcher instead of hitting the
    /// network (`new`/`new_with_telemetry` delegate here with `UreqFetcher`).
    pub async fn new_full(
        store: Arc<Store>,
        registries: Registries,
        telemetry: Arc<dyn Telemetry>,
        attachment_fetcher: Arc<dyn AttachmentFetcher>,
    ) -> Arc<ControlPlane> {
        let (events, _) = broadcast::channel(1024);

        // Startup maintenance (install-ledger backfill + crash-leftover
        // sweep) deliberately does NOT run here. Both touch the operator's
        // real `$HOME/.config/ryuzi/{skills,plugins}` via
        // `InstallRoots::for_user()`, and the sweep is destructive
        // (`remove_dir_all`). This constructor is called by every crate's
        // `test_cp()` helper — including `ryuzi-cockpit`'s, where `ryuzi-core`
        // is compiled WITHOUT `test` cfg, so a `#[cfg(test)]` no-op guard
        // here would not fire and tests would delete real user files. Instead
        // the real long-running hosts call `run_startup_maintenance()`
        // explicitly after construction; no test path ever does. See that
        // method's doc comment.

        Arc::new(ControlPlane {
            store,
            automation: Dispatcher::new(),
            registries,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            running: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashMap::new()),
            telemetry,
            attachment_fetcher,
            draining: std::sync::atomic::AtomicBool::new(false),
            active_turns: std::sync::atomic::AtomicUsize::new(0),
            background: crate::harness::native::background::BackgroundRegistry::new(),
            plugins_restart_required: std::sync::atomic::AtomicBool::new(false),
            review_llm_factory: Mutex::new(Arc::new(
                crate::harness::native::llm::RouterLlmStreamFactory,
            )),
            extension_host: Arc::new(ExtensionHost::new()),
            agent_persistence: std::sync::OnceLock::new(),
        })
    }

    /// One-time, best-effort startup maintenance for the install ledger and
    /// on-disk skill/plugin trees. Call this EXACTLY ONCE, from a real
    /// long-running host, AFTER the `ControlPlane` is built — never from
    /// `new`/`new_full` (which every crate's `test_cp()` helper calls) and
    /// never from a unit test. Both steps read (and the sweep deletes) files
    /// under the operator's real `$HOME/.config/ryuzi/{skills,plugins}` via
    /// `InstallRoots::for_user()`, so running it from a constructor would let
    /// `cargo test` (in particular `ryuzi-cockpit`'s test binary, where
    /// `ryuzi-core` is a non-`test`-cfg dependency) touch and delete real
    /// user files.
    ///
    /// Steps, each best-effort (warn-and-continue — a maintenance failure
    /// must never block startup):
    /// 1. Backfill `plugin_installs` rows for any on-disk pack installed
    ///    before the ledger existed (idempotent; read + insert only).
    /// 2. Sweep `.stage-`/`.backup-`/`.tmp-` crash leftovers from an
    ///    interrupted install/update (destructive `remove_dir_all`).
    pub async fn run_startup_maintenance(&self) {
        if let Err(e) = crate::skills_install::backfill_install_records(&self.store).await {
            tracing::warn!("plugin install-ledger backfill failed: {e}");
        }
        if let Err(e) = crate::skills_install::sweep_stale_install_leftovers() {
            tracing::warn!("plugin install-leftover sweep failed: {e}");
        }
    }

    /// Track D's extension host — every spawned extension subprocess across
    /// every plugin (see [`crate::plugins::extension::ExtensionHost`]).
    /// Empty until [`Self::spawn_extensions`] is called.
    pub fn extension_host(&self) -> &Arc<ExtensionHost> {
        &self.extension_host
    }

    pub fn attach_agent_persistence(
        &self,
        persistence: crate::agents::bootstrap::AgentPersistenceHandles,
    ) -> anyhow::Result<()> {
        self.agent_persistence
            .set(persistence)
            .map_err(|_| anyhow::anyhow!("agent persistence is already attached"))
    }

    pub(crate) fn agent_persistence(
        &self,
    ) -> Option<&crate::agents::bootstrap::AgentPersistenceHandles> {
        self.agent_persistence.get()
    }

    /// Attach an isolated YAML/OKF persistence graph for crate unit tests that
    /// exercise session lifecycle paths. Production composition roots attach
    /// their shared graph explicitly; this helper prevents test constructors
    /// from silently omitting that required dependency.
    #[cfg(test)]
    pub(crate) async fn attach_test_agent_persistence(&self) {
        let config = tempfile::tempdir().expect("agent persistence tempdir");
        let persistence = crate::agents::bootstrap::initialize_agent_persistence(
            config.path().to_path_buf(),
            self.store.clone(),
        )
        .await
        .expect("initialize test agent persistence");
        self.attach_agent_persistence(persistence.handles())
            .expect("attach test agent persistence");
        std::mem::forget(config);
    }

    /// Spawn every enabled extension-capable plugin's subprocess(es) (Track
    /// D) and start their supervision. Call this EXACTLY ONCE, from a real
    /// long-running host's daemon entry, AFTER the daemon has genuinely
    /// started — never from `build_daemon`/`new_full` (every crate's
    /// `test_cp()` helper calls those) and never from a unit test, so
    /// `cargo test` stays hermetic: constructing a `ControlPlane` never
    /// spawns a real subprocess. Mirrors [`Self::run_startup_maintenance`]'s
    /// hermeticity discipline — see that method's doc.
    ///
    /// Each extension's handshake can take up to
    /// `plugins::extension::proc::INIT_HANDSHAKE_TIMEOUT` (25s), and
    /// `ExtensionHost::spawn_all` spawns them one at a time — callers MUST
    /// run this as a detached background task (`tokio::spawn`), never awaited
    /// inline on a startup-latency-sensitive path, so a slow/hanging
    /// extension can never delay "daemon: running" or consume the connect
    /// timeout budget the daemon entry races `build_daemon`/`Daemon::start`
    /// against.
    pub async fn spawn_extensions(&self) {
        let ctx = ExtensionCtx {
            settings: SettingsStore::new(self.store.clone()),
        };
        self.extension_host.spawn_all(self.plugins(), &ctx).await;
    }

    /// Gracefully stop every spawned extension subprocess (Track D). Called
    /// from [`crate::daemon::Daemon::stop`] so extension shutdown always
    /// rides along with the rest of daemon teardown. Safe to call even when
    /// nothing was ever spawned (every test `ControlPlane`, or a daemon that
    /// never reached [`Self::spawn_extensions`]) —
    /// `ExtensionHost::shutdown_all` on an empty host is an immediate no-op,
    /// not a hermeticity violation.
    pub async fn shutdown_extensions(&self) {
        self.extension_host.shutdown_all(SHUTDOWN_GRACE).await;
    }

    /// Shared handle to the persistence layer — used by daemon wiring,
    /// the domain modules (scheduler/mcp/providers/gateways), and the Tauri
    /// command layer. Returns a borrow; callers that need ownership clone.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The shared background-delegation registry (capacity + cancellation).
    pub fn background(&self) -> &Arc<crate::harness::native::background::BackgroundRegistry> {
        &self.background
    }

    /// The `LlmStreamFactory` `run_review_fork` (control/lifecycle.rs)
    /// builds its `RunnerDeps.llm` from. See the field doc for why this
    /// bypasses `registries.harness`.
    pub(crate) fn review_llm_factory(
        &self,
    ) -> Arc<dyn crate::harness::native::llm::LlmStreamFactory> {
        self.review_llm_factory.lock().unwrap().clone()
    }

    /// Test-only: override the `LlmStreamFactory` a review fork drives
    /// through, so a test can inject a scripted/recording stream and assert
    /// on the exact request the fork sends (Task 9).
    #[doc(hidden)]
    #[cfg(test)]
    pub fn set_review_llm_factory_for_test(
        &self,
        factory: Arc<dyn crate::harness::native::llm::LlmStreamFactory>,
    ) {
        *self.review_llm_factory.lock().unwrap() = factory;
    }

    /// The plugin host — every installed plugin's manifest, capabilities, and
    /// enablement state (see `plugins::host::PluginHost`). Used by `serve.rs`
    /// to expose plugins over HTTP without duplicating `Registries`' shape.
    pub fn plugins(&self) -> &crate::plugins::PluginHost {
        &self.registries.plugins
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    /// Number of turns currently in flight (see `spawn_prompt`'s `TurnGuard`).
    pub fn running_count(&self) -> usize {
        self.active_turns.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// One-way latch: stop accepting new turns, wait (100ms polls) for
    /// in-flight turns up to `timeout_ms`; turns still running at the
    /// deadline are left alone (killed by the daemon exit, resumed by
    /// `reconcile`).
    pub async fn drain(&self, timeout_ms: u64) {
        self.draining
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        while self.running_count() > 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Set the in-memory restart-required latch. Called by plugin/skill
    /// install, update, and uninstall paths (Task 9) once they mutate
    /// on-disk state that `self.registries` — built once at startup — cannot
    /// reflect until the process restarts. Idempotent and safe to call from
    /// any number of concurrent mutation paths.
    pub fn mark_plugins_restart_required(&self) {
        self.plugins_restart_required
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a plugin/skill mutation since this process started requires a
    /// restart before it takes effect. Read by the daemon's plugins routes
    /// and Cockpit surfaces to show a "restart required" indicator.
    pub fn plugins_restart_required(&self) -> bool {
        self.plugins_restart_required
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Dispatch one normalized automation event. Event sources call this narrow
    /// method directly; there is intentionally no background subscriber on the
    /// core-event bus, which would outlive short-lived control planes in tests.
    pub async fn dispatch_inbound_webhook(
        self: &Arc<Self>,
        hook: HookRow,
        envelope: AutomationEnvelope,
    ) -> Result<InboundWebhookDispatch, InboundWebhookError> {
        if hook.trigger_kind != TriggerKind::WebhookInbound || !hook.enabled {
            return Err(InboundWebhookError::Invalid(
                "inbound webhook hook is not enabled".to_string(),
            ));
        }
        let HookActionInput::AgentRun(action) = &hook.action else {
            return Err(InboundWebhookError::Invalid(
                "inbound webhook hook must use agent.run".to_string(),
            ));
        };
        if action.gateway_id != "local" {
            return Err(InboundWebhookError::Invalid(
                "agent.run target gateway is not local".to_string(),
            ));
        }
        if self
            .store
            .get_project(&action.project_id)
            .await
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?
            .is_none()
        {
            return Err(InboundWebhookError::Invalid(
                "agent.run target project no longer exists".to_string(),
            ));
        }
        if self
            .automation
            .rate_verdict(&hook.id, TriggerKind::WebhookInbound, 0)
            != RateVerdict::Accepted
        {
            return Err(InboundWebhookError::RateLimited);
        }
        let envelope = envelope.capped();
        let envelope_json = serde_json::to_value(&envelope)
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?;
        let run = crate::automation::create_run(&self.store, &hook.id, envelope_json)
            .await
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?;
        let event_json = serde_json::to_string_pretty(&run.envelope)
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?;
        let prompt = format!(
            "{}\n\n[UNTRUSTED AUTOMATION EVENT — JSON]\n{}\n[END UNTRUSTED AUTOMATION EVENT]",
            action.prompt, event_json
        );
        let git = (!action.branch.is_empty()).then(|| crate::domain::SessionGitOptions {
            use_worktree: true,
            create_branch: false,
            branch_name: None,
            base_branch: Some(action.branch.clone()),
        });
        let worker = action.agent_id.as_ref().map(|agent| WorkerBinding {
            agent: agent.clone(),
            home_session_pk: None,
        });
        let mut turn_prompt = crate::harness::TurnPrompt::text(prompt.clone(), prompt);
        turn_prompt.force_subtask = Some(action.subtask);
        let session = self
            .start_session_with_prompt_and_origin(
                &action.project_id,
                turn_prompt,
                "automation",
                &[],
                git,
                None,
                action.model_override.clone(),
                worker,
                Some(HookOrigin::new(&run.hook_id, &run.id, 1)),
            )
            .await
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?;
        crate::automation::link_run_session(&self.store, &run.id, &session.session_pk)
            .await
            .map_err(|error| InboundWebhookError::Unavailable(error.to_string()))?;
        self.emit_automation_run_changed(&run.hook_id, &run.id, "running");
        Ok(InboundWebhookDispatch {
            hook_id: run.hook_id,
            run_id: run.id,
            session_pk: session.session_pk,
        })
    }

    /// Dispatch one normalized automation event. Event sources call this narrow
    /// method directly; there is intentionally no background subscriber on the
    /// core-event bus, which would outlive short-lived control planes in tests.
    pub async fn dispatch_automation_event(
        self: &Arc<Self>,
        envelope: AutomationEnvelope,
        origin: Option<HookOrigin>,
    ) {
        let envelope = envelope.capped();
        let depth = origin.as_ref().map_or(0, |origin| origin.depth);
        let envelope_json = match serde_json::to_value(&envelope) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("automation envelope serialization failed: {error}");
                return;
            }
        };
        let hooks = match crate::automation::list_enabled_hooks(&self.store, envelope.event).await {
            Ok(hooks) => hooks,
            Err(error) => {
                tracing::warn!("automation hook lookup failed: {error}");
                return;
            }
        };

        for hook in hooks {
            let run = match crate::automation::create_run(
                &self.store,
                &hook.id,
                envelope_json.clone(),
            )
            .await
            {
                Ok(run) => run,
                Err(error) => {
                    tracing::warn!(hook_id = %hook.id, "automation run creation failed: {error}");
                    continue;
                }
            };
            let verdict = self
                .automation
                .rate_verdict(&hook.id, envelope.event, depth);
            if verdict != RateVerdict::Accepted {
                let reason = match verdict {
                    RateVerdict::HookLimited => "hook rate limit exceeded",
                    RateVerdict::EngineLimited => "engine rate limit exceeded",
                    RateVerdict::DepthLimited => "automation origin depth limit exceeded",
                    RateVerdict::Accepted => unreachable!(),
                };
                self.finish_automation_run(&run.hook_id, &run.id, "skipped", None, Some(reason))
                    .await;
                continue;
            }

            match &run.snapshot.action {
                HookActionInput::WebhookOutbound(_) => {
                    let result = crate::automation::deliver_outbound(&self.store, &run).await;
                    let (status, error) = match result {
                        Ok(()) => ("success", None),
                        Err(error) => ("failed", Some(error.to_string())),
                    };
                    self.finish_automation_run(
                        &run.hook_id,
                        &run.id,
                        status,
                        None,
                        error.as_deref(),
                    )
                    .await;
                }
                HookActionInput::AgentRun(action) => {
                    if action.gateway_id != "local" {
                        self.finish_automation_run(
                            &run.hook_id,
                            &run.id,
                            "failed",
                            None,
                            Some("agent.run target gateway is not local"),
                        )
                        .await;
                        continue;
                    }
                    if self
                        .store
                        .get_project(&action.project_id)
                        .await
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        self.finish_automation_run(
                            &run.hook_id,
                            &run.id,
                            "failed",
                            None,
                            Some("agent.run target project no longer exists"),
                        )
                        .await;
                        continue;
                    }
                    let event_json = match serde_json::to_string_pretty(&run.envelope) {
                        Ok(json) => json,
                        Err(error) => {
                            self.finish_automation_run(
                                &run.hook_id,
                                &run.id,
                                "failed",
                                None,
                                Some(&format!("could not serialize event envelope: {error}")),
                            )
                            .await;
                            continue;
                        }
                    };
                    let prompt = format!(
                        "{}\n\n[UNTRUSTED AUTOMATION EVENT — JSON]\n{}\n[END UNTRUSTED AUTOMATION EVENT]",
                        action.prompt, event_json
                    );
                    let git =
                        (!action.branch.is_empty()).then(|| crate::domain::SessionGitOptions {
                            use_worktree: true,
                            create_branch: false,
                            branch_name: None,
                            base_branch: Some(action.branch.clone()),
                        });
                    let worker =
                        action
                            .agent_id
                            .as_ref()
                            .map(|agent| crate::control::WorkerBinding {
                                agent: agent.clone(),
                                home_session_pk: None,
                            });
                    let hook_origin =
                        HookOrigin::new(&run.hook_id, &run.id, depth.saturating_add(1));
                    let mut turn_prompt = crate::harness::TurnPrompt::text(prompt.clone(), prompt);
                    turn_prompt.force_subtask = Some(action.subtask);
                    match self
                        .start_session_with_prompt_and_origin(
                            &action.project_id,
                            turn_prompt,
                            "automation",
                            &[],
                            git,
                            None,
                            action.model_override.clone(),
                            worker,
                            Some(hook_origin),
                        )
                        .await
                    {
                        Ok(session) => {
                            match crate::automation::link_run_session(
                                &self.store,
                                &run.id,
                                &session.session_pk,
                            )
                            .await
                            {
                                Ok(true) => self.emit_automation_run_changed(
                                    &run.hook_id,
                                    &run.id,
                                    "running",
                                ),
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::warn!(run_id = %run.id, "automation session link failed: {error}");
                                }
                            }
                        }
                        Err(error) => {
                            self.finish_automation_run(
                                &run.hook_id,
                                &run.id,
                                "failed",
                                None,
                                Some(&error.to_string()),
                            )
                            .await
                        }
                    }
                }
            }
        }
    }

    /// No core gateway runtime currently persists operational status; gateway
    /// config writes and router status messages are view/config state, not a status
    /// transition producer. Keep this adapter as the narrow integration boundary
    /// for the future runtime writer rather than falsely dispatching from polling.
    pub async fn observe_gateway_status_transition(
        self: &Arc<Self>,
        gateway_id: &str,
        previous_status: &str,
        status: &str,
    ) {
        self.dispatch_automation_event(
            AutomationEnvelope::new(
                TriggerKind::GatewayStatusChanged,
                chrono::Utc::now().to_rfc3339(),
                AutomationSource::new("gateway", gateway_id),
                serde_json::json!({
                    "gatewayId": gateway_id,
                    "previousStatus": previous_status,
                    "status": status,
                }),
            ),
            None,
        )
        .await;
    }

    /// Native lifecycle adapter. Hook-origin sessions never emit lifecycle
    /// automations, preventing recursive cascades while preserving existing
    /// script and extension hook behavior. `session.end` observes the persisted
    /// terminal session row, because teardown persists it before the native
    /// session dispatches this adapter.
    pub async fn observe_native_automation(
        self: &Arc<Self>,
        trigger: TriggerKind,
        session_pk: String,
        mut data: serde_json::Value,
    ) {
        let origin = match self.store.hook_origin(&session_pk).await {
            Ok(origin) => origin,
            Err(error) => {
                tracing::warn!(session_pk, "automation origin lookup failed: {error}");
                return;
            }
        };
        if origin.is_some() {
            return;
        }
        let session = match self.store.get_session(&session_pk).await {
            Ok(Some(session)) => session,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(session_pk, "automation session lookup failed: {error}");
                return;
            }
        };
        let object = data
            .as_object_mut()
            .expect("native lifecycle data is an object");
        object.insert(
            "sessionPk".into(),
            serde_json::Value::String(session_pk.clone()),
        );
        object.insert(
            "projectId".into(),
            session
                .project_id
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
        object.insert(
            "gatewayId".into(),
            serde_json::Value::String("local".into()),
        );
        object.insert(
            "agentId".into(),
            serde_json::Value::String(session.agent.unwrap_or_else(|| "native".into())),
        );
        object.insert(
            "status".into(),
            serde_json::Value::String(session.status.as_str().into()),
        );
        self.dispatch_automation_event(
            AutomationEnvelope::new(
                trigger,
                chrono::Utc::now().to_rfc3339(),
                AutomationSource::new("session", session_pk),
                data,
            ),
            None,
        )
        .await;
    }

    pub(crate) async fn finish_automation_session(
        &self,
        session_pk: &str,
        status: &str,
        error: Option<&str>,
    ) {
        let Ok(Some(origin)) = self.store.hook_origin(session_pk).await else {
            return;
        };
        self.finish_automation_run(
            &origin.hook_id,
            &origin.run_id,
            status,
            Some(session_pk),
            error,
        )
        .await;
    }

    async fn finish_automation_run(
        &self,
        hook_id: &str,
        run_id: &str,
        status: &str,
        session_pk: Option<&str>,
        error: Option<&str>,
    ) {
        let changed =
            match crate::automation::finish_run(&self.store, run_id, status, session_pk, error)
                .await
            {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::warn!(run_id, "automation run finalization failed: {error}");
                    return;
                }
            };
        if changed {
            self.emit_automation_run_changed(hook_id, run_id, status);
        }
    }

    fn emit_automation_run_changed(&self, hook_id: &str, run_id: &str, status: &str) {
        let _ = self.events.send(CoreEvent::AutomationHookRunChanged {
            hook_id: hook_id.to_string(),
            run_id: run_id.to_string(),
            status: status.to_string(),
        });
    }

    /// broadcast update-lifecycle events through the same channel as
    /// session events.
    pub fn emit(&self, e: CoreEvent) {
        let _ = self.events.send(e);
    }

    /// Broadcast a core event (used by domain modules like the scheduler).
    pub fn send_event(&self, e: CoreEvent) -> bool {
        self.events.send(e).is_ok()
    }

    /// Clone of the broadcast sender for long-running domain tasks.
    pub fn events_sender(&self) -> broadcast::Sender<CoreEvent> {
        self.events.clone()
    }

    /// Resolve a pending approval with the user's full decision. Telemetry
    /// counts by decision so allow/deny rates stay observable.
    pub fn resolve_approval(&self, request_id: &str, response: ApprovalResponse) -> bool {
        let name = if response.allowed() {
            "approval.allow"
        } else {
            "approval.deny"
        };
        let resolved = self.approvals.resolve(request_id, response);
        self.telemetry.count(name, vec![]);
        resolved
    }

    /// Binary resolve for surfaces that only know allow/deny (gateway
    /// fan-out timeout/deny paths).
    pub fn resolve_approval_bool(&self, request_id: &str, allow: bool) -> bool {
        self.resolve_approval(request_id, ApprovalResponse::once(allow))
    }

    /// Test-only: park a fake approval and return its receiver.
    #[doc(hidden)]
    #[cfg(test)]
    pub fn approvals_for_test_register(
        &self,
        request_id: &str,
    ) -> tokio::sync::oneshot::Receiver<crate::domain::ApprovalResponse> {
        self.approvals.register(request_id.to_string())
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.store.list_projects().await
    }

    pub async fn list_sessions(&self, project_id: Option<&str>) -> anyhow::Result<Vec<Session>> {
        self.store.list_sessions(project_id).await
    }

    /// Bind an existing session to a project (`app_projects`'s "attach"
    /// action, spec §9.1). Validates both rows exist, then persists the
    /// association — see [`Store::set_session_project`] for the caveat that a
    /// currently-live session keeps running under whatever `project_id` its
    /// `RunnerDeps` was already built with until it is resumed.
    pub async fn attach_project(&self, session_pk: &str, project_id: &str) -> anyhow::Result<()> {
        self.store
            .get_session(session_pk)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown session: {session_pk}"))?;
        self.store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?;
        self.store.set_session_project(session_pk, project_id).await
    }

    /// Build the curated app-control facade (spec §9.1) for a top-level
    /// interactive session: holds a `Weak<ControlPlane>` so a session's
    /// `ToolCtx` never keeps the plane alive, and tags every write it makes
    /// with `WriteOrigin::Agent` — the storage-layer negative-space guard
    /// (spec §9.3) refuses this origin on settings/policy writes, and its
    /// audit rows are tagged `agent`.
    pub fn build_app_control(
        self: &Arc<Self>,
    ) -> Arc<dyn crate::harness::native::tools::AppControl> {
        Arc::new(app_control::AppControlImpl::new(
            Arc::downgrade(self),
            crate::domain::WriteOrigin::Agent,
        ))
    }

    /// Persist per-project preferences (host-supplied model/effort/perm overrides).
    /// `None` leaves the corresponding column untouched.
    pub async fn set_project_prefs(
        &self,
        project_id: &str,
        model: Option<&str>,
        effort: Option<&str>,
        perm_mode: Option<PermMode>,
    ) -> anyhow::Result<()> {
        self.store
            .update_project_prefs(project_id, model, effort, perm_mode)
            .await
    }

    pub async fn list_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        self.store.list_messages(session_pk).await
    }

    /// Retrieve the persisted tool policy for `(project_id, tool)`, if any.
    pub async fn get_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
    ) -> anyhow::Result<Option<String>> {
        self.store.get_tool_policy(project_id, tool).await
    }

    /// Persist (or update) a tool policy for `(project_id, tool)`. Only
    /// reached from the Cockpit/gateway resolve path (a human clicked
    /// "Always allow"), so this always writes as `WriteOrigin::User`.
    pub async fn set_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        self.store
            .set_tool_policy(crate::domain::WriteOrigin::User, project_id, tool, decision)
            .await
    }

    /// All persisted tool policies (Settings → Permissions).
    pub async fn list_tool_policies(&self) -> anyhow::Result<Vec<ToolPolicyRow>> {
        self.store.list_tool_policies().await
    }

    /// Remove a persisted tool policy. Only reached from the Cockpit
    /// Settings → Permissions "revoke" action (a human), so this always
    /// writes as `WriteOrigin::User`.
    pub async fn delete_tool_policy(&self, project_id: &str, tool: &str) -> anyhow::Result<()> {
        self.store
            .delete_tool_policy(crate::domain::WriteOrigin::User, project_id, tool)
            .await
    }
}

/// `ControlPlane` satisfies `UpdateManager`'s `NotifyTarget` seam, so the
/// update manager can enumerate sessions and broadcast update-lifecycle
/// events without depending on the concrete control plane.
#[async_trait::async_trait]
impl crate::update::manager::NotifyTarget for ControlPlane {
    async fn list_sessions(&self) -> Vec<Session> {
        ControlPlane::list_sessions(self, None)
            .await
            .unwrap_or_default()
    }

    fn emit(&self, e: CoreEvent) {
        self.emit(e);
    }
}
