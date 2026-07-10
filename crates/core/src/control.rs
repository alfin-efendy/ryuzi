use crate::approval::ApprovalHub;
use crate::attachments::{AttachmentFetcher, UreqFetcher};
use crate::domain::{
    ApprovalResponse, CoreEvent, Message, PermMode, Project, Session, ToolPolicyRow,
};
use crate::harness::HarnessSession;
use crate::plugins::Registries;
use crate::store::Store;
use crate::telemetry::{NoopTelemetry, Telemetry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

mod attachments;
mod lifecycle;
mod provisioning;
#[cfg(test)]
mod tests;

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

pub struct ControlPlane {
    store: Arc<Store>,
    /// Extension registries (harness/gateway/connector). `Project.harness` is
    /// resolved against `registries.harness`.
    registries: Registries,
    events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub — handed to each `SessionCtx` so the ACP permission
    /// bridge can route tool-permission prompts back to the UI.
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
    /// One-way in-memory latch: set once a plugin/skill install, update, or
    /// uninstall has mutated on-disk state that the already-constructed
    /// `registries` above cannot pick up without a process restart. Reset
    /// only by restarting the daemon (deliberately not persisted — the
    /// underlying `Registries` snapshot is also rebuilt from scratch on
    /// every startup, so a stale `true` surviving a restart would be
    /// meaningless).
    plugins_restart_required: std::sync::atomic::AtomicBool,
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

        // One-time: create ledger rows for any skill packs installed before
        // the ledger existed. Best-effort — a backfill failure must not
        // block startup (mirrors the plugin path's warn-and-continue
        // discipline). Runs here (rather than in `new`/`new_with_telemetry`)
        // so every real construction path — daemon, Cockpit, and any future
        // caller — gets it exactly once, while `store` is still an
        // `Arc<Store>` we can borrow before it's moved into the struct below.
        // A no-op under the crate's own `#[cfg(test)]` build so the ~56
        // in-crate `control::tests` stay hermetic instead of reading the
        // operator's real `$HOME/.config/ryuzi/skills` via
        // `InstallRoots::for_user()`; the two Task 4 unit tests exercise
        // `backfill_install_records_in` directly, so coverage is preserved.
        Self::backfill_install_ledger(&store).await;

        // Best-effort: remove any staging/backup leftovers a prior process
        // crashed mid-install/update without cleaning up (see
        // `skills_install::sweep_stale_install_leftovers`). Same
        // warn-and-continue discipline and the same reason for being
        // compiled out under the crate's own `#[cfg(test)]` build as the
        // backfill above — it also touches `InstallRoots::for_user()`.
        Self::sweep_stale_install_leftovers();

        Arc::new(ControlPlane {
            store,
            registries,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            running: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashMap::new()),
            telemetry,
            attachment_fetcher,
            draining: std::sync::atomic::AtomicBool::new(false),
            active_turns: std::sync::atomic::AtomicUsize::new(0),
            plugins_restart_required: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Best-effort one-time install-ledger backfill (see the call site in
    /// `new_full`). In a real (non-`test`) build this reads the operator's
    /// installed packs via `InstallRoots::for_user()` and warns-and-continues
    /// on any error. Compiled out entirely under the crate's own unit tests so
    /// they never touch the real `$HOME`.
    #[cfg(not(test))]
    async fn backfill_install_ledger(store: &Arc<Store>) {
        if let Err(e) = crate::skills_install::backfill_install_records(store).await {
            tracing::warn!("plugin install-ledger backfill failed: {e}");
        }
    }

    #[cfg(test)]
    async fn backfill_install_ledger(_store: &Arc<Store>) {}

    /// Best-effort one-time crash-leftover sweep (see the call site in
    /// `new_full`). In a real (non-`test`) build this removes stale staging/
    /// backup directories under the operator's `InstallRoots::for_user()` and
    /// warns-and-continues on any error. Compiled out entirely under the
    /// crate's own unit tests so they never touch the real `$HOME`.
    #[cfg(not(test))]
    fn sweep_stale_install_leftovers() {
        if let Err(e) = crate::skills_install::sweep_stale_install_leftovers() {
            tracing::warn!("plugin install-leftover sweep failed: {e}");
        }
    }

    #[cfg(test)]
    fn sweep_stale_install_leftovers() {}

    /// Shared handle to the persistence layer — used by daemon wiring,
    /// the domain modules (scheduler/mcp/providers/gateways), and the Tauri
    /// command layer. Returns a borrow; callers that need ownership clone.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
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

    /// Public event injection — used by `UpdateManager`'s notify path to
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

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.store.list_projects().await
    }

    pub async fn list_sessions(&self, project_id: Option<&str>) -> anyhow::Result<Vec<Session>> {
        self.store.list_sessions(project_id).await
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

    /// Persist (or update) a tool policy for `(project_id, tool)`.
    pub async fn set_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        self.store.set_tool_policy(project_id, tool, decision).await
    }

    /// All persisted tool policies (Settings → Permissions).
    pub async fn list_tool_policies(&self) -> anyhow::Result<Vec<ToolPolicyRow>> {
        self.store.list_tool_policies().await
    }

    /// Remove a persisted tool policy.
    pub async fn delete_tool_policy(&self, project_id: &str, tool: &str) -> anyhow::Result<()> {
        self.store.delete_tool_policy(project_id, tool).await
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
