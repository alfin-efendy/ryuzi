use crate::approval::ApprovalHub;
use crate::attachments::{AttachmentFetcher, UreqFetcher};
use crate::domain::{CoreEvent, Message, PermMode, Project, Session};
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
    /// Telemetry seam (see `crate::telemetry`) — `Noop` unless a daemon wires
    /// up `Console`/OTLP via `new_with_telemetry`.
    telemetry: Arc<dyn Telemetry>,
    /// Downloads Discord (or other gateway) message attachments for
    /// `with_attachments`. Real network I/O (`UreqFetcher`) unless a test
    /// injects a fake via `new_full`.
    attachment_fetcher: Arc<dyn AttachmentFetcher>,
    /// One-way latch set by `drain` — once true, `start_session`/
    /// `continue_session` reject new turns.
    draining: std::sync::atomic::AtomicBool,
    /// Count of in-flight turns (incremented synchronously in `spawn_prompt`
    /// before the task is spawned, decremented by `TurnGuard`'s `Drop` inside
    /// the task) — polled by `drain` to know when it's safe to stop waiting.
    active_turns: std::sync::atomic::AtomicUsize,
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
        Arc::new(ControlPlane {
            store,
            registries,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            running: Mutex::new(HashMap::new()),
            telemetry,
            attachment_fetcher,
            draining: std::sync::atomic::AtomicBool::new(false),
            active_turns: std::sync::atomic::AtomicUsize::new(0),
        })
    }

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

    pub fn resolve_approval(&self, request_id: &str, allow: bool) -> bool {
        let resolved = self.approvals.resolve(request_id, allow);
        let name = if allow {
            "approval.allow"
        } else {
            "approval.deny"
        };
        self.telemetry.count(name, vec![]);
        resolved
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
