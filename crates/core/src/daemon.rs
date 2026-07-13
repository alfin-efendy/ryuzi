//! Daemon composition root: wires `Store` → settings → telemetry → harness
//! registries → `ControlPlane` → gateways → the outbound `Router` → the
//! approval fan-out.
//!
//! [`build_daemon`] is the single entry point; [`Daemon::start`]/`stop` drive
//! the lifecycle. The approval fan-out — the piece that finally consumes
//! `approval_timeout_ms` — is kept in a standalone, unit-testable
//! `pub(crate)` function ([`handle_approval`]) separate from the broadcast
//! loop that spawns it.

use crate::agents::knowledge::AgentKnowledgeStore;
use crate::agents::learning_queue::LearningQueue;
use crate::agents::registry::AgentRegistry;
use crate::control::ControlPlane;
use crate::domain::{
    ApprovalDecision, ApprovalRequest, ApprovalResponse, CoreEvent, Principal, Surface,
};
use crate::gateway::{Gateway, GatewayFactory};
use crate::harness::native::native_plugin;
use crate::harness::HarnessFactory;
use crate::llm_router::secrets;
use crate::llm_router::server::RouterServer;
use crate::plugins::Registries;
use crate::policy;
use crate::router::Router;
use crate::settings::{csv, SettingsStore, CATALOG};
use crate::store::Store;
use crate::telemetry::{ConsoleTelemetry, Telemetry};
use futures::FutureExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};

/// Inputs to [`build_daemon`].
pub struct BuildDaemonOpts {
    /// Path to the sqlite database (created/migrated by `Store::open`).
    pub db_path: PathBuf,
    /// Canonical root for YAML agent profiles and OKF knowledge. It must be
    /// supplied independently of the SQLite location.
    pub config_root: PathBuf,
    /// Override the telemetry backend (used by tests). `None` selects
    /// Console, or OTLP behind the `otel` feature once `otel_endpoint` is set.
    pub telemetry: Option<Arc<dyn Telemetry>>,
    /// Gateway factories available to wire, keyed by the id an entry in the
    /// `enabled_gateways` setting names (e.g. `"discord"`).
    pub extra_gateway_factories: Vec<(String, Arc<dyn GatewayFactory>)>,
    /// Test seam: replace the single native harness factory with a fake.
    /// `None` (production) uses the real native runtime.
    pub harness_factory: Option<Arc<dyn HarnessFactory>>,
}

/// A fully wired daemon: control plane, shared store handle, and the
/// gateways `build_daemon` constructed. `store` is the SAME `Arc<Store>` the
/// `ControlPlane` holds internally (via `cp.store()`), so callers needing
/// direct DB access (e.g. HTTP read endpoints) share one connection pool.
pub struct Daemon {
    pub cp: Arc<ControlPlane>,
    pub store: Arc<Store>,
    pub gateways: Vec<Arc<dyn Gateway>>,
    /// The local Anthropic/OpenAI-compatible endpoint server. The daemon
    /// always constructs it (cheap — it does not bind a port until
    /// `start()`'s autostart branch, or an explicit RPC, calls
    /// `RouterServer::start`), so any consumer (e.g. the HTTP control API's
    /// `ApiState`) can share this one instance instead of standing up its
    /// own.
    pub router_server: Arc<RouterServer>,
    pub agents: Arc<AgentRegistry>,
    pub agent_knowledge: Arc<AgentKnowledgeStore>,
    pub learning_queue: Arc<LearningQueue>,
    telemetry: Arc<dyn Telemetry>,
    stopped: AtomicBool,
    /// The outbound `Router`'s broadcast-consumer task, tracked so `stop()`
    /// can abort it. Otherwise it (and the `Arc<ControlPlane>` clone its
    /// closure holds) would keep running — and keep the control plane
    /// alive — past `stop()`.
    router_handle: JoinHandle<()>,
    /// The approval fan-out's broadcast-consumer task, tracked for the same
    /// reason. Aborting it also drops its owned `JoinSet` of in-flight
    /// per-approval `handle_approval` races (see `spawn_approval_fanout`),
    /// so no race started before `stop()` survives it either.
    fanout_handle: JoinHandle<()>,
    /// The cron scheduler's background loop (`scheduler::spawn_runner`).
    /// The daemon is the single always-on engine host now, so this is the
    /// only place this loop is ever spawned — its `job_last_fired` anchor
    /// (see `scheduler::tick`) is single-host-only, and a second spawn
    /// elsewhere (e.g. Cockpit, which used to host this loop) would race
    /// the same anchor and double-fire or skip jobs. Tracked so `stop()`
    /// can abort it, same as `router_handle`/`fanout_handle`.
    scheduler_handle: JoinHandle<()>,
    /// The orch dispatcher's background loop (`orch::spawn_runner`), tracked
    /// for the same reason as `scheduler_handle`.
    orch_handle: JoinHandle<()>,
    /// The background-rail drainer's loop (`background_rail::spawn_runner`),
    /// tracked for the same reason as `scheduler_handle`/`orch_handle` — the
    /// daemon is the single always-on engine host for it too.
    rail_handle: JoinHandle<()>,
    /// The durable per-agent learning queue worker (`learning::spawn_runner`),
    /// tracked for the same reason as `rail_handle`. It claims pending queue
    /// rows and applies them to the owning agent's OKF bundle.
    learning_handle: JoinHandle<()>,
    /// The curator's weekly skill-lifecycle loop (`curator::spawn_runner`,
    /// Task 10), tracked for the same reason as `learning_handle`. Unlike
    /// every other loop here it is store-only (no harness/LLM dispatch for
    /// the deterministic sweep), so it is spawned directly off `store`
    /// rather than `cp` — see `build_daemon`'s doc.
    curator_handle: JoinHandle<()>,
}

impl Daemon {
    /// Start every gateway, then fire-and-forget `cp.reconcile()` (resumes
    /// any session a dead process left `Running`). Reconcile runs in the
    /// background so a slow/hanging resume can't block daemon startup.
    ///
    /// Partial-failure rollback: if gateway N fails to start, every gateway
    /// 0..N-1 that DID start is stopped (best-effort — errors swallowed,
    /// same as `stop()`), the router/fan-out/scheduler/orch/rail/learning/
    /// curator handles are aborted, and the daemon is marked stopped (reusing the same
    /// idempotency flag `stop()` checks) before the error is returned.
    /// Marking it stopped here means a caller's own best-effort `stop()` on
    /// a `start()` error (e.g. `daemon_cmd::build_and_start`) is a safe
    /// no-op instead of re-stopping gateway 0..N-1 a second time.
    ///
    /// This task is deliberately left UNTRACKED (unlike `router_handle` /
    /// `fanout_handle`): boot does not await or hold onto its reconcile
    /// call. A
    /// resume it kicks off is its own independent `spawn_prompt` background
    /// turn (tracked/owned by `ControlPlane`, not by `Daemon`), so there is
    /// nothing here for `stop()` to meaningfully cancel — this handle only
    /// covers the `reconcile()` scan itself, which is expected to finish
    /// quickly regardless of `Daemon`'s lifecycle.
    ///
    /// After reconcile is kicked off, endpoint autostart runs: if the
    /// persisted `endpoint_autostart` setting is `"1"`, `router_server` is
    /// started on the persisted `endpoint_port` (default 21128) — the same
    /// autostart Cockpit's setup hook used to perform. A failure here is
    /// logged and swallowed rather than propagated: a broken endpoint
    /// server must not prevent the rest of the daemon (gateways, sessions)
    /// from coming up.
    pub async fn start(&self) -> anyhow::Result<()> {
        for (idx, gw) in self.gateways.iter().enumerate() {
            if let Err(e) = gw.start().await {
                if !self.stopped.swap(true, Ordering::SeqCst) {
                    for started in &self.gateways[..idx] {
                        let _ = started.stop().await;
                    }
                    self.router_handle.abort();
                    self.fanout_handle.abort();
                    self.scheduler_handle.abort();
                    self.orch_handle.abort();
                    self.rail_handle.abort();
                    self.learning_handle.abort();
                    self.curator_handle.abort();
                }
                return Err(e);
            }
        }
        let cp = Arc::clone(&self.cp);
        tokio::spawn(async move {
            let _ = cp.reconcile().await;
        });

        // Endpoint autostart (moved from Cockpit's setup hook).
        let settings = crate::settings::SettingsStore::new(Arc::clone(&self.store));
        if settings
            .get("endpoint_autostart")
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
        {
            let port: u16 = settings
                .get("endpoint_port")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(21128);
            if let Err(e) = self.router_server.start(port).await {
                eprintln!("[ryuzi] endpoint autostart failed: {e}");
            }
        }
        Ok(())
    }

    /// Idempotent teardown: stop every gateway (errors swallowed so one
    /// failing gateway can't block the rest of the shutdown),
    /// abort the router and approval fan-out broadcast-consumer loops (which
    /// also aborts any in-flight per-approval races the fan-out spawned —
    /// see `spawn_approval_fanout`), abort the scheduler, orch, rail,
    /// learning, and curator loops, stop the endpoint server, then flush
    /// telemetry. A second call is a no-op.
    pub async fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        for gw in &self.gateways {
            let _ = gw.stop().await;
        }
        self.router_handle.abort();
        self.fanout_handle.abort();
        self.scheduler_handle.abort();
        self.orch_handle.abort();
        self.rail_handle.abort();
        self.learning_handle.abort();
        self.curator_handle.abort();
        self.router_server.stop().await;
        // Track D: gracefully stop every spawned extension subprocess. Safe
        // even when nothing was ever spawned (every test daemon, or a real
        // daemon whose entry never reached `spawn_extensions`) — see
        // `ControlPlane::shutdown_extensions`'s doc.
        self.cp.shutdown_extensions().await;
        self.telemetry.shutdown();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.router_handle.abort();
        self.fanout_handle.abort();
        self.scheduler_handle.abort();
        self.orch_handle.abort();
        self.rail_handle.abort();
        self.learning_handle.abort();
        self.curator_handle.abort();
    }
}

/// Given the persisted `otel_endpoint` setting, choose the telemetry backend.
/// A pure/unit-testable split of `build_daemon`'s telemetry-selection branch:
/// an empty/unset endpoint selects `ConsoleTelemetry` silently (the second
/// tuple element — "warned" — is `false`).
///
/// A non-empty endpoint tries the real OTLP exporter behind
/// `#[cfg(feature = "otel")]` ([`crate::telemetry::OtelTelemetry`]):
/// - feature ON + construction succeeds → the `OtelTelemetry` backend,
///   `warned = false` (no fallback happened).
/// - feature ON + construction fails (e.g. an unparseable endpoint) →
///   `ConsoleTelemetry`, `warned = true`.
/// - feature OFF → always `ConsoleTelemetry`, `warned = true` (unchanged
///   pre-Task-6 behavior — the feature doesn't exist in the build).
///
/// Either way, `warned = true` means the caller should print the
/// `[telemetry] OTel init failed — falling back to console` warning. The
/// print itself is kept out of this function (rather than done here) so the
/// selection logic is testable without capturing stderr.
pub(crate) fn select_telemetry(otel_endpoint: &str) -> (Arc<dyn Telemetry>, bool) {
    if otel_endpoint.trim().is_empty() {
        return (Arc::new(ConsoleTelemetry::new()), false);
    }
    match try_otel_telemetry(otel_endpoint) {
        Some(telemetry) => (telemetry, false),
        None => (Arc::new(ConsoleTelemetry::new()), true),
    }
}

/// Attempt to construct the real OTLP backend for `otel_endpoint`. Returns
/// `None` (never panics) when the `otel` feature is off, or when
/// construction fails — either way `select_telemetry` falls back to
/// `ConsoleTelemetry` with a warning.
#[cfg(feature = "otel")]
fn try_otel_telemetry(otel_endpoint: &str) -> Option<Arc<dyn Telemetry>> {
    crate::telemetry::OtelTelemetry::new(otel_endpoint)
        .ok()
        .map(|telemetry| Arc::new(telemetry) as Arc<dyn Telemetry>)
}

#[cfg(not(feature = "otel"))]
fn try_otel_telemetry(_otel_endpoint: &str) -> Option<Arc<dyn Telemetry>> {
    None
}

/// Build order (each stage depends on the previous one):
/// `Store::open` → settings → telemetry select → `Registries` (always
/// installs the native plugin, then `install_builtins`, then applies
/// `opts.harness_factory` if set) → `ControlPlane::new_with_telemetry`
/// → gateways (from `enabled_gateways` + `extra_gateway_factories` + the
/// provider catalog) → the outbound `Router` spawned on one `cp.subscribe()`
/// → a second, inbound-only `Router` handed to every gateway via
/// `Gateway::set_router` (Task 6 — see `router.rs`'s module doc for why two
/// instances) → the approval fan-out spawned on another `cp.subscribe()`
/// → the cron scheduler (`scheduler::spawn_runner`), orch dispatcher
/// (`orch::spawn_runner`), background-rail drainer
/// (`background_rail::spawn_runner`), learning worker
/// (`learning::spawn_runner`), and curator (`curator::spawn_runner`, Task
/// 10 — store-only, spawned off `store` directly rather than `cp` since it
/// never dispatches an LLM turn) loops, spawned here because the daemon
/// is the single always-on engine host for them (see `Daemon`'s
/// `scheduler_handle` doc) → the local endpoint server (`RouterServer::new`),
/// constructed but not started — `Daemon::start()` starts it only if
/// `endpoint_autostart`
/// is set. One `Arc<Store>` is opened once and cloned throughout — no
/// `Arc::try_unwrap` reclaiming.
///
/// Telemetry selection: an explicit `opts.telemetry` override always wins;
/// otherwise selection is delegated to [`select_telemetry`] (see its doc for
/// the empty/non-empty `otel_endpoint` behavior).
pub async fn build_daemon(opts: BuildDaemonOpts) -> anyhow::Result<Daemon> {
    let store = Arc::new(Store::open(&opts.db_path).await?);
    let persistence = crate::agents::bootstrap::initialize_agent_persistence(
        opts.config_root,
        Arc::clone(&store),
    )
    .await?;
    // One-time (idempotent) upgrade of any legacy plaintext secrets to
    // encrypted-at-rest; see `llm_router::secrets::init_and_sweep`'s doc for
    // the atomicity/idempotency/degraded-state contract.
    secrets::init_and_sweep(&store).await;

    let telemetry: Arc<dyn Telemetry> = match opts.telemetry {
        Some(t) => t,
        None => {
            let settings = SettingsStore::new(Arc::clone(&store));
            let endpoint = settings.get("otel_endpoint").await?.unwrap_or_default();
            let (telemetry, warned) = select_telemetry(&endpoint);
            if warned {
                // Task 6 slots the real OTLP exporter here, behind
                // `#[cfg(feature = "otel")]` (the feature doesn't exist yet).
                // Until then, every configured endpoint falls back to
                // console with this warning.
                eprintln!("[telemetry] OTel init failed — falling back to console");
            }
            telemetry
        }
    };

    let mut registries = Registries::new();
    registries.add_plugin(native_plugin());
    crate::plugins::install_providers(&mut registries);
    // Merge the (version-gated) remote catalog cache over the embedded
    // catalog before anything is added to the host — `Registries::add_plugin`
    // is first-registration-wins with no removal, so the winner per id must
    // already be decided (see `catalog::merged_catalog_plugins`'s doc). An
    // empty/unreadable cache degrades to the embedded catalog alone.
    let remote = store.list_remote_catalog().await.unwrap_or_default();
    for plugin in crate::plugins::catalog::merged_catalog_plugins(&remote) {
        registries.add_plugin(plugin);
    }
    crate::plugins::load_skill_pack_plugins(&mut registries);
    if let Some(factory) = opts.harness_factory {
        registries.harness = factory;
    }

    let cp =
        ControlPlane::new_with_telemetry(Arc::clone(&store), registries, Arc::clone(&telemetry))
            .await;
    cp.attach_agent_persistence(persistence.handles())?;
    let settings = SettingsStore::new(Arc::clone(&store));

    let factories: HashMap<String, Arc<dyn GatewayFactory>> =
        opts.extra_gateway_factories.into_iter().collect();
    let mut gateways: Vec<Arc<dyn Gateway>> = Vec::new();
    for id in csv(settings.get("enabled_gateways").await?.as_deref()) {
        let Some(factory) = factories.get(&id) else {
            continue; // no registered factory for this id — skip silently
        };
        let Some(descriptor) = CATALOG.gateway(&id) else {
            continue; // no catalog entry to build a config from — skip silently
        };
        let mut config = serde_json::Map::new();
        for field in descriptor.fields {
            let value = settings.get(field.key).await?.unwrap_or_default();
            config.insert(field.key.to_string(), serde_json::Value::String(value));
        }
        let gw = factory.create(&serde_json::Value::Object(config))?;
        gateways.push(gw);
    }

    // Two `Router` instances sharing the same `cp`/`store` — see `router.rs`'s
    // module doc. `router_out` drives the outbound render loop (`run`
    // consumes `self`); `router_in` is handed to every gateway via
    // `set_router` (Task 6: `DiscordGateway`'s `new(port)` + `set_router`
    // inversion — see `gateway::discord::mod`'s doc — needs a `Router` to
    // exist, but a `Router` needs the already-built gateway list, so
    // gateways are built first and given a `Router` handle right after one
    // exists; most gateways ignore it via `Gateway::set_router`'s default
    // no-op).
    let router_out = Router::new(Arc::clone(&cp), gateways.clone());
    let router_handle = tokio::spawn(router_out.run(cp.subscribe()));

    let router_in = Arc::new(Router::new(Arc::clone(&cp), gateways.clone()));
    for gw in &gateways {
        gw.set_router(Arc::clone(&router_in));
    }

    let fanout_handle =
        spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), gateways.clone());

    // The daemon is the single always-on engine host: cron scheduler and
    // orch dispatcher live HERE (moved out of Cockpit). The scheduler's
    // job_last_fired anchor is single-host-only — never spawn a second one.
    let scheduler_handle = crate::scheduler::spawn_runner(Arc::clone(&cp));
    let orch_handle = crate::orch::spawn_runner(Arc::clone(&cp));
    let rail_handle = crate::background_rail::spawn_runner(Arc::clone(&cp));
    let learning_handle = crate::learning::spawn_runner(Arc::clone(&persistence.learning));
    let curator_handle = crate::curator::spawn_runner(Arc::clone(&store));
    let router_server = Arc::new(RouterServer::new(Arc::clone(&store)));

    Ok(Daemon {
        cp,
        store,
        gateways,
        router_server,
        agents: persistence.registry,
        agent_knowledge: persistence.knowledge,
        learning_queue: persistence.learning,
        telemetry,
        stopped: AtomicBool::new(false),
        router_handle,
        fanout_handle,
        scheduler_handle,
        orch_handle,
        rail_handle,
        learning_handle,
        curator_handle,
    })
}

/// Subscribe to `cp`'s event bus and spawn one [`handle_approval`] task per
/// `ApprovalRequested` event, into a `JoinSet` owned by this loop. Runs until
/// the broadcast channel closes. Returns the loop's own `JoinHandle` so the
/// caller (`build_daemon`/`Daemon`) can `abort()` it — which drops the loop's
/// future, which drops its `JoinSet`, which (per `JoinSet`'s `Drop` impl)
/// aborts every still-running `handle_approval` race spawned into it. So
/// aborting this one handle tears down both the loop AND any in-flight
/// per-approval work, instead of leaving orphaned untracked tasks racing
/// against a control plane the caller believes is stopped.
fn spawn_approval_fanout(
    cp: Arc<ControlPlane>,
    store: Arc<Store>,
    gateways: Vec<Arc<dyn Gateway>>,
) -> JoinHandle<()> {
    let mut rx = cp.subscribe();
    tokio::spawn(async move {
        let mut inflight: JoinSet<()> = JoinSet::new();
        loop {
            match rx.recv().await {
                Ok(CoreEvent::ApprovalRequested {
                    session_pk,
                    request_id,
                    tool,
                    summary,
                    approval_kind,
                    input: _,
                    principal,
                }) => {
                    // Gateways only render binary tool prompts. Plan/Question
                    // prompts are Cockpit/CLI-only surfaces — a headless
                    // daemon/gateway session has nothing that will ever answer
                    // them. Rather than `continue` and leave the turn parked
                    // forever, spawn a timeout into the same `inflight` set
                    // that resolves the request to `Cancel` once
                    // `approval_timeout_ms` elapses, so the blocked tool call
                    // reports "no interactive surface" instead of hanging.
                    // `resolve_approval` is a harmless no-op if a real surface
                    // (Cockpit, CLI) already answered it first.
                    if approval_kind != crate::domain::ApprovalKind::Tool {
                        let cp = Arc::clone(&cp);
                        let store = Arc::clone(&store);
                        inflight.spawn(async move {
                            schedule_non_tool_approval_cancel(&cp, &store, &request_id).await;
                        });
                        continue;
                    }
                    let cp = Arc::clone(&cp);
                    let store = Arc::clone(&store);
                    let gateways = gateways.clone();
                    inflight.spawn(async move {
                        handle_approval(
                            &cp,
                            &store,
                            &gateways,
                            &session_pk,
                            &request_id,
                            &tool,
                            &summary,
                            principal,
                        )
                        .await;
                    });
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Headless fallback for Plan/Question approvals: gateways can't render
/// them, so there's no surface to answer the request in a daemon/gateway
/// session. Instead of leaving the turn parked forever, sleep for
/// `approval_timeout_ms` (same setting, same default, as [`handle_approval`])
/// then resolve the request as `Cancel` — distinct from an ordinary reject so
/// `exitplanmode`/`askuserquestion` can report "no interactive surface"
/// rather than "the user rejected this". `resolve_approval` returns `false`
/// (and does nothing) if the request was already resolved by a real surface,
/// so racing this against Cockpit/CLI is harmless.
pub(crate) async fn schedule_non_tool_approval_cancel(
    cp: &Arc<ControlPlane>,
    store: &Arc<Store>,
    request_id: &str,
) {
    let settings = SettingsStore::new(Arc::clone(store));
    let timeout_ms: u64 = settings
        .get("approval_timeout_ms")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000);
    tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
    cp.resolve_approval(
        request_id,
        ApprovalResponse {
            decision: ApprovalDecision::Cancel,
            scope: None,
            payload: None,
        },
    );
}

/// Core approval fan-out decision, callable directly (no broadcast loop
/// needed) so it's unit-testable: reads `approval_timeout_ms` /
/// `approver_role_ids` from settings and `started_by` from the session, then
/// resolves via `cp.resolve_approval_bool`.
///
/// - No surfaces bound to the session (after filtering to gateways we know
///   about) → immediate deny.
/// - Otherwise races `gw.request_approval` across every known surface via a
///   loop over `futures::future::select_all`: a per-gateway `Err` REMOVES
///   that future from the race (so one erroring gateway can never out-race a
///   slower legitimate human approval on another surface) and the remaining
///   futures keep racing; only once every future has errored does the race
///   resolve to a deny. The whole race is wrapped in `tokio::time::timeout`;
///   elapsing also denies.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_approval(
    cp: &Arc<ControlPlane>,
    store: &Arc<Store>,
    gateways: &[Arc<dyn Gateway>],
    session_pk: &str,
    request_id: &str,
    tool: &str,
    summary: &str,
    principal: Option<Principal>,
) {
    let settings = SettingsStore::new(Arc::clone(store));

    let timeout_ms: u64 = settings
        .get("approval_timeout_ms")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000);

    let approver_role_ids = policy::parse_role_ids(
        settings
            .get("approver_role_ids")
            .await
            .ok()
            .flatten()
            .as_deref(),
    );

    let started_by = store
        .get_session(session_pk)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.started_by);

    let known_surfaces: Vec<(Surface, Arc<dyn Gateway>)> = store
        .surfaces(session_pk)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|surface| {
            gateways
                .iter()
                .find(|g| g.id() == surface.gateway)
                .cloned()
                .map(|gw| (surface, gw))
        })
        .collect();

    if known_surfaces.is_empty() {
        cp.resolve_approval_bool(request_id, false);
        return;
    }

    let req = ApprovalRequest {
        request_id: request_id.to_string(),
        tool: tool.to_string(),
        summary: summary.to_string(),
        approver_role_ids,
        started_by,
        timeout_ms: Some(timeout_ms),
        principal,
    };

    let futs: Vec<_> = known_surfaces
        .into_iter()
        .map(|(surface, gw)| {
            let req = req.clone();
            async move { gw.request_approval(&surface, &req).await }.boxed()
        })
        .collect();

    // Loop over `select_all`, dropping any future that resolves `Err` from
    // the race instead of treating it as an instant deny vote — only once
    // every future has errored (the pool is empty) do we fall back to deny.
    let race = async move {
        let mut futs = futs;
        loop {
            if futs.is_empty() {
                return None;
            }
            let (result, _idx, rest) = futures::future::select_all(futs).await;
            futs = rest;
            if let Ok(decision) = result {
                return Some(decision);
            }
        }
    };

    let decision = match tokio::time::timeout(Duration::from_millis(timeout_ms), race).await {
        Ok(Some(decision)) => decision,
        Ok(None) | Err(_) => ApprovalDecision::RejectOnce,
    };

    cp.resolve_approval_bool(
        request_id,
        matches!(
            decision,
            ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NewMessage, PermMode, Project, Session, SessionKind, SessionStatus};
    use crate::gateway::MessageRef;
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::telemetry::NoopTelemetry;
    use async_trait::async_trait;
    use serial_test::serial;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    // ---------- shared test plumbing ----------

    fn temp_db_path() -> (tempfile::NamedTempFile, PathBuf) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        (f, path)
    }

    async fn test_agent_persistence(
        store: Arc<Store>,
    ) -> crate::agents::bootstrap::AgentPersistence {
        let config = tempfile::tempdir().unwrap();
        let persistence = crate::agents::bootstrap::initialize_agent_persistence(
            config.path().to_path_buf(),
            store,
        )
        .await
        .unwrap();
        std::mem::forget(config);
        persistence
    }

    fn capturing_console_telemetry() -> (Arc<Mutex<Vec<String>>>, Arc<dyn Telemetry>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = lines.clone();
        let telemetry = ConsoleTelemetry::with_sink(
            move |line: &str| captured.lock().unwrap().push(line.to_string()),
            || 1_000,
        );
        (lines, Arc::new(telemetry))
    }

    fn parse_telemetry_lines(lines: &Arc<Mutex<Vec<String>>>) -> Vec<serde_json::Value> {
        lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).expect("telemetry line must be valid JSON"))
            .collect()
    }

    async fn control_plane_with_telemetry(
        telemetry: Arc<dyn Telemetry>,
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        let (guard, path) = temp_db_path();
        let store = Store::open(&path).await.unwrap();
        let cp =
            ControlPlane::new_with_telemetry(Arc::new(store), Registries::new(), telemetry).await;
        let store = cp.store().clone();
        (cp, store, guard)
    }

    async fn seed_project(store: &Store, project_id: &str) {
        store
            .insert_project(Project {
                project_id: project_id.to_string(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(crate::paths::now_ms()),
                is_git: false,
            })
            .await
            .unwrap();
    }

    async fn seed_session(
        store: &Store,
        session_pk: &str,
        project_id: &str,
        started_by: Option<&str>,
    ) {
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: session_pk.to_string(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: Some(project_id.to_string()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: started_by.map(|s| s.to_string()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: true,
                kind: SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
    }

    /// Redirects `dirs::data_dir()`/HOME into a tempdir for the duration of a
    /// test, so worktree creation never touches the real state dir. Process
    /// global env — every test using it must be `#[serial]`.
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

    fn init_repo(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    // ---------- FakeGateway: configurable approval behavior ----------

    #[derive(Clone, Copy)]
    enum GwBehavior {
        Allow,
        SleepThenAllow(u64),
        /// Returns `Err` the instant it's called — used to prove a
        /// per-gateway error no longer wins the race outright (it must be
        /// removed from the race instead), and that all-erroring surfaces
        /// still deny.
        ErrImmediately,
    }

    struct FakeGateway {
        gid: String,
        behavior: GwBehavior,
        calls: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        last_req: Arc<Mutex<Option<ApprovalRequest>>>,
        /// When true, `start()` always fails — used to exercise
        /// `Daemon::start`'s partial-failure rollback.
        fail_start: bool,
    }

    impl FakeGateway {
        fn new(gid: &str, behavior: GwBehavior) -> Self {
            FakeGateway {
                gid: gid.to_string(),
                behavior,
                calls: Arc::new(AtomicUsize::new(0)),
                stops: Arc::new(AtomicUsize::new(0)),
                last_req: Arc::new(Mutex::new(None)),
                fail_start: false,
            }
        }

        fn new_failing_start(gid: &str) -> Self {
            FakeGateway {
                fail_start: true,
                ..FakeGateway::new(gid, GwBehavior::Allow)
            }
        }
    }

    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            &self.gid
        }
        async fn start(&self) -> anyhow::Result<()> {
            if self.fail_start {
                anyhow::bail!("start failed for gateway {}", self.gid);
            }
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(&self, _w: &str, _t: &str) -> anyhow::Result<String> {
            Ok("conv".into())
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m".into(),
            })
        }
        async fn edit_status(&self, _msg: &MessageRef, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(&self, _surface: &Surface, _chunks: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(&self, _surface: &Surface, _message: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _surface: &Surface,
            req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_req.lock().unwrap() = Some(req.clone());
            match self.behavior {
                GwBehavior::Allow => Ok(ApprovalDecision::AllowOnce),
                GwBehavior::SleepThenAllow(ms) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok(ApprovalDecision::AllowOnce)
                }
                GwBehavior::ErrImmediately => {
                    anyhow::bail!("approval request failed for gateway {}", self.gid)
                }
            }
        }
    }

    // ---------- (a) build_daemon wiring ----------

    struct CapturingGatewayFactory {
        captured: Arc<Mutex<Option<serde_json::Value>>>,
    }
    impl GatewayFactory for CapturingGatewayFactory {
        fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            *self.captured.lock().unwrap() = Some(config.clone());
            Ok(Arc::new(FakeGateway::new("discord", GwBehavior::Allow)))
        }
    }

    #[tokio::test]
    async fn build_daemon_wires_known_gateways_and_skips_unknown_ids() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings
                .set("enabled_gateways", "discord,bogus")
                .await
                .unwrap();
            settings.set("discord.token", "tok-xyz").await.unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let factory: Arc<dyn GatewayFactory> = Arc::new(CapturingGatewayFactory {
            captured: captured.clone(),
        });

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("discord".to_string(), factory)],
            harness_factory: None,
        })
        .await
        .unwrap();

        assert_eq!(
            daemon.gateways.len(),
            1,
            "only the known 'discord' id should be wired; 'bogus' must be skipped"
        );
        let cfg = captured
            .lock()
            .unwrap()
            .clone()
            .expect("factory must have been called");
        assert_eq!(cfg["discord.token"], "tok-xyz");
        assert_eq!(cfg["discord.app_id"], "");
        assert_eq!(cfg["discord.guild_id"], "");
    }

    #[tokio::test]
    async fn build_daemon_merges_remote_catalog_cache_over_the_embedded_catalog() {
        let (_guard, db_path) = temp_db_path();
        const NEW_TOML: &str = "contract=1\nid=\"acme-remote\"\nname=\"Acme Remote\"\nversion=\"1.0.0\"\n[[mcp]]\nname=\"m\"\ntransport=\"http\"\nurl=\"https://x\"";
        {
            let store = Store::open(&db_path).await.unwrap();
            store
                .upsert_remote_catalog(&[crate::store::RemoteCatalogRow {
                    id: "acme-remote".to_string(),
                    manifest_toml: NEW_TOML.to_string(),
                    version: "1.0.0".to_string(),
                    sequence: 1,
                    blocked: false,
                    blocked_reason: None,
                    fetched_at: 0,
                }])
                .await
                .unwrap();
        }

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();

        let ids: Vec<String> = daemon
            .cp
            .plugins()
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();
        assert!(
            ids.contains(&"acme-remote".to_string()),
            "remote catalog cache entry must be merged into the running host: {ids:?}"
        );
    }

    /// Track D hermeticity: `build_daemon` must never spawn a real extension
    /// subprocess, even when an enabled extension-capable plugin is present
    /// in the composed `Registries` — extension spawning happens ONLY from
    /// the daemon's real entry point (`crates/runner/src/daemon_cmd.rs`, via
    /// `ControlPlane::spawn_extensions`), mirroring
    /// `run_startup_maintenance`'s own hermeticity discipline (see that
    /// method's doc). `cargo test`'s `build_daemon` calls must stay safe to
    /// run in parallel without ever touching a real process tree.
    ///
    /// Proven two ways: (1) a marker file the fake extension's shell command
    /// would touch on spawn must remain absent; (2) the constructed
    /// `ExtensionHost` itself must report no spawned entry for the plugin.
    /// `#[serial]` because this test overrides `$HOME` (the only way to feed
    /// an extension-capable plugin into `build_daemon`'s real
    /// `load_skill_pack_plugins` composition step, since `BuildDaemonOpts`
    /// has no plugin-injection seam) — see `plugins::extension::proc`'s own
    /// `#[serial]` env-var test for the same reasoning.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn build_daemon_never_spawns_extension_subprocesses() {
        let (_guard, db_path) = temp_db_path();
        let fake_home = tempfile::tempdir().unwrap();
        let plugin_dir = fake_home.path().join(".config/ryuzi/plugins/marker-ext");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let marker = fake_home.path().join("spawned.marker");
        let manifest_toml = format!(
            "contract = 1\nid = \"marker-ext\"\nname = \"Marker Extension\"\n\n\
             [[extension]]\nname = \"marker\"\ncommand = \"sh\"\n\
             args = [\"-c\", \"touch '{}'\"]\nevents = [\"tool.before\"]\n",
            marker.display()
        );
        std::fs::write(plugin_dir.join("ryuzi-plugin.toml"), manifest_toml).unwrap();
        std::fs::write(
            plugin_dir.join(".ryuzi-skill.json"),
            r#"{"source":"https://example.test/marker","plugin_id":"marker-ext","installed_at":"2026-07-11T00:00:00.000Z"}"#,
        )
        .unwrap();

        {
            let store = Store::open(&db_path).await.unwrap();
            // `set_setting_raw` (not `SettingsStore::set`, which validates
            // the key against `PLUGIN_FIELDS` — not yet populated for
            // "marker-ext" this early) — mirrors
            // `plugins::extension::proc`'s own tests seeding a plugin's
            // enable flag.
            store
                .set_setting_raw("plugin.marker-ext.enabled", "true")
                .await
                .unwrap();
        }

        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", fake_home.path());
        let result = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await;
        match previous_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }

        let daemon = result
            .expect("build_daemon must succeed even with an extension-capable plugin present");
        assert!(
            daemon.cp.plugins().get("marker-ext").is_some(),
            "sanity: the extension-capable skill-pack plugin must have registered"
        );

        // Give any hypothetical stray spawn a moment to touch the marker
        // before asserting its absence.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !marker.exists(),
            "build_daemon must stay hermetic: it must never spawn a real extension subprocess"
        );
        assert!(
            daemon.cp.extension_host().get("marker-ext").await.is_empty(),
            "the constructed ExtensionHost must have no spawned entry until spawn_extensions() runs"
        );
    }

    // ---------- (b)/(c)/(d) handle_approval unit tests ----------

    #[tokio::test]
    async fn handle_approval_allow_resolves_true() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", Some("starter-1")).await;
        store.add_surface("discord", "chan1", "s1").await.unwrap();
        SettingsStore::new(store.clone())
            .set("approver_role_ids", "role-a, role-b")
            .await
            .unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let last_req = gw.last_req.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        let principal = Principal {
            plugin_id: "acme-connector".into(),
            plugin_name: "Acme Connector".into(),
        };
        handle_approval(
            &cp,
            &store,
            &gateways,
            "s1",
            "req-1",
            "Bash",
            "ls -la",
            Some(principal.clone()),
        )
        .await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.allow"),
            "expected an approval.allow count line, got: {parsed:?}"
        );

        let captured = last_req
            .lock()
            .unwrap()
            .clone()
            .expect("gateway must have been asked");
        assert_eq!(captured.started_by.as_deref(), Some("starter-1"));
        assert_eq!(
            captured.approver_role_ids,
            vec!["role-a".to_string(), "role-b".to_string()]
        );
        assert_eq!(captured.timeout_ms, Some(300_000)); // default
        assert_eq!(captured.tool, "Bash");
        assert_eq!(captured.summary, "ls -la");
        assert_eq!(
            captured.principal,
            Some(principal),
            "the principal handle_approval was called with must survive into the ApprovalRequest \
             handed to the gateway — the spec→event→request round trip"
        );
    }

    #[tokio::test]
    async fn handle_approval_timeout_denies() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        store.add_surface("discord", "chan1", "s1").await.unwrap();
        SettingsStore::new(store.clone())
            .set("approval_timeout_ms", "100")
            .await
            .unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::SleepThenAllow(2_000));
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        handle_approval(&cp, &store, &gateways, "s1", "req-2", "Bash", "sleep", None).await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
            "a timed-out race must deny, got: {parsed:?}"
        );
    }

    #[tokio::test]
    async fn handle_approval_no_surfaces_denies_immediately() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        // Deliberately no add_surface.

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let calls = gw.calls.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        handle_approval(&cp, &store, &gateways, "s1", "req-3", "Bash", "ls", None).await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no surfaces means the gateway must never be asked"
        );
        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
            "no surfaces must deny immediately, got: {parsed:?}"
        );
    }

    // ---------- fan-out error tolerance (MUST-FIX 1) ----------

    #[tokio::test]
    async fn handle_approval_an_erroring_gateway_does_not_out_race_a_slower_legitimate_allow() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        // Two surfaces on two DIFFERENT gateways bound to the same session,
        // so both are raced.
        store.add_surface("err-gw", "c1", "s1").await.unwrap();
        store.add_surface("allow-gw", "c2", "s1").await.unwrap();

        let err_gw = FakeGateway::new("err-gw", GwBehavior::ErrImmediately);
        let allow_gw = FakeGateway::new("allow-gw", GwBehavior::SleepThenAllow(50));
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(err_gw), Arc::new(allow_gw)];

        handle_approval(
            &cp,
            &store,
            &gateways,
            "s1",
            "req-race-1",
            "Bash",
            "ls",
            None,
        )
        .await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.allow"),
            "an instantly-erroring gateway must be removed from the race, not win it — \
             the slower legitimate allow must still resolve the approval, got: {parsed:?}"
        );
    }

    #[tokio::test]
    async fn handle_approval_denies_only_once_every_gateway_has_errored() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        store.add_surface("err-gw-1", "c1", "s1").await.unwrap();
        store.add_surface("err-gw-2", "c2", "s1").await.unwrap();

        let gw1 = FakeGateway::new("err-gw-1", GwBehavior::ErrImmediately);
        let gw2 = FakeGateway::new("err-gw-2", GwBehavior::ErrImmediately);
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw1), Arc::new(gw2)];

        handle_approval(
            &cp,
            &store,
            &gateways,
            "s1",
            "req-race-2",
            "Bash",
            "ls",
            None,
        )
        .await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
            "once every gateway in the race has errored, the approval must deny, got: {parsed:?}"
        );
    }

    // ---------- Daemon::start partial-failure rollback (MUST-FIX 2) ----------

    #[tokio::test]
    async fn daemon_start_rolls_back_started_gateways_and_aborts_handles_on_later_failure() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let cp = ControlPlane::new_with_telemetry(
            Arc::new(store),
            Registries::new(),
            Arc::new(NoopTelemetry),
        )
        .await;
        let store = cp.store().clone();

        let gw_a = FakeGateway::new("gw-a", GwBehavior::Allow);
        let stops_a = gw_a.stops.clone();
        let gw_b = FakeGateway::new_failing_start("gw-b");
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw_a), Arc::new(gw_b)];

        // Long-running "loops" standing in for the real router/fan-out/
        // scheduler/orch/rail/learning tasks, so this test can assert that a
        // failed `start()` aborts them too.
        let router_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let fanout_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let scheduler_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let orch_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let rail_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let learning_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let curator_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });

        let persistence = test_agent_persistence(store.clone()).await;
        cp.attach_agent_persistence(persistence.handles()).unwrap();
        let daemon = Daemon {
            cp,
            store: store.clone(),
            gateways,
            router_server: Arc::new(RouterServer::new(store)),
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            router_handle,
            fanout_handle,
            scheduler_handle,
            orch_handle,
            rail_handle,
            learning_handle,
            curator_handle,
        };

        let err = daemon.start().await.unwrap_err();
        assert!(
            err.to_string().contains("gw-b"),
            "the propagated error must be the failing gateway's, got: {err}"
        );

        assert_eq!(
            stops_a.load(Ordering::SeqCst),
            1,
            "gateway A (already started) must be stopped exactly once during rollback"
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            daemon.router_handle.is_finished(),
            "start()'s rollback must abort the router loop"
        );
        assert!(
            daemon.fanout_handle.is_finished(),
            "start()'s rollback must abort the fan-out loop"
        );
        assert!(
            daemon.scheduler_handle.is_finished(),
            "start()'s rollback must abort the scheduler loop"
        );
        assert!(
            daemon.orch_handle.is_finished(),
            "start()'s rollback must abort the orch loop"
        );
        assert!(
            daemon.rail_handle.is_finished(),
            "start()'s rollback must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "start()'s rollback must abort the learning loop"
        );
        assert!(
            daemon.curator_handle.is_finished(),
            "start()'s rollback must abort the curator loop"
        );

        // A later explicit stop() (as `build_and_start` performs on a start
        // failure) must be a no-op — rollback already tore everything down.
        daemon.stop().await;
        assert_eq!(
            stops_a.load(Ordering::SeqCst),
            1,
            "a later stop() after rollback must not re-invoke gateway A's stop()"
        );
    }

    // ---------- (b, integration) approval fan-out lets a blocked turn complete ----------

    struct PermFakeSession {
        store: Arc<Store>,
        events: broadcast::Sender<CoreEvent>,
        approvals: Arc<crate::approval::ApprovalHub>,
        session_pk: String,
    }

    #[async_trait]
    impl HarnessSession for PermFakeSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt.display }),
                ))
                .await;

            let request_id = "perm-req-1".to_string();
            let _ = self.events.send(CoreEvent::ApprovalRequested {
                session_pk: self.session_pk.clone(),
                request_id: request_id.clone(),
                tool: "Bash".into(),
                summary: "ls -la".into(),
                approval_kind: crate::domain::ApprovalKind::Tool,
                input: serde_json::json!({}),
                principal: None,
            });
            let rx = self.approvals.register(request_id);
            let allow = rx.await.map(|r| r.allowed()).unwrap_or(false);
            if allow {
                let _ = self
                    .store
                    .insert_message(NewMessage::block(
                        &self.session_pk,
                        "assistant",
                        "text",
                        serde_json::json!({ "text": "done" }),
                    ))
                    .await;
            }
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-1".into())
        }
    }

    struct PermFakeHarness;
    #[async_trait]
    impl Harness for PermFakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(PermFakeSession {
                store: ctx.store.clone(),
                events: ctx.events.clone(),
                approvals: ctx.approvals.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }
    struct PermFakeHarnessFactory;
    impl HarnessFactory for PermFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(PermFakeHarness))
        }
    }

    #[tokio::test]
    #[serial]
    async fn approval_fanout_allows_a_blocked_turn_to_complete_end_to_end() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let mut regs = Registries::new();
        regs.harness = Arc::new(PermFakeHarnessFactory);
        let cp =
            ControlPlane::new_with_telemetry(Arc::new(store), regs, Arc::new(NoopTelemetry)).await;
        cp.attach_test_agent_persistence().await;
        let store = cp.store().clone();

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        let mut rx = cp.subscribe();
        let session = cp
            .start_session(&project.project_id, "go", "test", &[])
            .await
            .unwrap();

        let mut saw_result = false;
        for _ in 0..200 {
            let recv = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
            match recv {
                Ok(Ok(CoreEvent::ApprovalRequested {
                    session_pk,
                    request_id,
                    tool,
                    summary,
                    ..
                })) => {
                    // Bind the surface now (we know the session_pk is live —
                    // the harness is already blocked awaiting the hub).
                    store
                        .add_surface("discord", "chan1", &session_pk)
                        .await
                        .unwrap();
                    handle_approval(
                        &cp,
                        &store,
                        &gateways,
                        &session_pk,
                        &request_id,
                        &tool,
                        &summary,
                        None,
                    )
                    .await;
                }
                Ok(Ok(CoreEvent::Result { .. })) => {
                    saw_result = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(
            saw_result,
            "expected the turn to complete (Result event) once the gateway allowed"
        );

        let msgs = store.list_messages(&session.session_pk).await.unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "done"),
            "expected the post-approval assistant row, got: {msgs:?}"
        );
    }

    // ---------- Plan/Question approvals time out to Cancel, not a hang ----------

    /// Like `PermFakeSession`, but raises a Plan-kind `ApprovalRequested`
    /// (unscoped registration — nothing else in this test resolves it) and
    /// records the decision it eventually got back as the assistant row, so
    /// the test can assert the fan-out's own timeout resolved it — not any
    /// gateway or manual `handle_approval` call.
    struct PlanFakeSession {
        store: Arc<Store>,
        events: broadcast::Sender<CoreEvent>,
        approvals: Arc<crate::approval::ApprovalHub>,
        session_pk: String,
    }

    #[async_trait]
    impl HarnessSession for PlanFakeSession {
        async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
            let request_id = "plan-req-1".to_string();
            let rx = self
                .approvals
                .register_for_session(&self.session_pk, request_id.clone());
            let _ = self.events.send(CoreEvent::ApprovalRequested {
                session_pk: self.session_pk.clone(),
                request_id: request_id.clone(),
                tool: "exitplanmode".into(),
                summary: "review the proposed plan".into(),
                approval_kind: crate::domain::ApprovalKind::Plan,
                input: serde_json::json!({ "plan": "do X" }),
                principal: None,
            });
            let decision = rx
                .await
                .map(|r| format!("{:?}", r.decision))
                .unwrap_or_else(|_| "channel-dropped".to_string());
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "assistant",
                    "text",
                    serde_json::json!({ "text": decision }),
                ))
                .await;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-plan".into())
        }
    }

    struct PlanFakeHarness;
    #[async_trait]
    impl Harness for PlanFakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(PlanFakeSession {
                store: ctx.store.clone(),
                events: ctx.events.clone(),
                approvals: ctx.approvals.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }
    struct PlanFakeHarnessFactory;
    impl HarnessFactory for PlanFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(PlanFakeHarness))
        }
    }

    #[tokio::test]
    #[serial]
    async fn approval_fanout_times_out_plan_kind_requests_to_cancel() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        // Keep the test fast: the fan-out's own timeout — not any gateway or
        // `handle_approval` call — must be what resolves this.
        SettingsStore::new(Arc::new(store))
            .set("approval_timeout_ms", "50")
            .await
            .unwrap();
        let store = Store::open(&db_path).await.unwrap();

        let mut regs = Registries::new();
        regs.harness = Arc::new(PlanFakeHarnessFactory);
        let cp =
            ControlPlane::new_with_telemetry(Arc::new(store), regs, Arc::new(NoopTelemetry)).await;
        cp.attach_test_agent_persistence().await;
        let store = cp.store().clone();

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        // Spawn the REAL fan-out loop under test (no gateways — Plan-kind
        // requests never touch them) instead of driving `handle_approval`
        // manually, so this exercises the actual skip-branch fix.
        let _fanout = spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), vec![]);

        let mut rx = cp.subscribe();
        let session = cp
            .start_session(&project.project_id, "go", "test", &[])
            .await
            .unwrap();

        let mut saw_result = false;
        for _ in 0..40 {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(CoreEvent::Result { .. })) => {
                    saw_result = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(
            saw_result,
            "the real spawn_approval_fanout must time out the parked Plan approval to Cancel \
             so the blocked turn completes instead of hanging forever"
        );

        let msgs = store.list_messages(&session.session_pk).await.unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "Cancel"),
            "expected the timed-out Plan approval to resolve with ApprovalDecision::Cancel, \
             got: {msgs:?}"
        );
    }

    // ---------- (e) Daemon::start fires reconcile ----------

    struct ResumeFakeSession {
        store: Arc<Store>,
        session_pk: String,
        prompts: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl HarnessSession for ResumeFakeSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            self.prompts.lock().unwrap().push(prompt.agent.clone());
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt.display }),
                ))
                .await;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-1".into())
        }
    }
    struct ResumeFakeHarness {
        prompts: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Harness for ResumeFakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(ResumeFakeSession {
                store: ctx.store.clone(),
                session_pk: ctx.session_pk.clone(),
                prompts: self.prompts.clone(),
            }))
        }
    }
    struct ResumeFakeHarnessFactory {
        prompts: Arc<Mutex<Vec<String>>>,
    }
    impl HarnessFactory for ResumeFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(ResumeFakeHarness {
                prompts: self.prompts.clone(),
            }))
        }
    }

    #[tokio::test]
    async fn daemon_start_fires_reconcile_and_resumes_running_sessions() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut regs = Registries::new();
        regs.harness = Arc::new(ResumeFakeHarnessFactory {
            prompts: prompts.clone(),
        });
        let cp =
            ControlPlane::new_with_telemetry(Arc::new(store), regs, Arc::new(NoopTelemetry)).await;
        let store = cp.store().clone();
        seed_project(&store, "p1").await;
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: Some("p1".into()),
                agent_session_id: Some("acp-123".into()),
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Running,
                perm_mode: PermMode::Default,
                started_by: Some("test".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: true,
                kind: SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();

        let persistence = test_agent_persistence(store.clone()).await;
        cp.attach_agent_persistence(persistence.handles()).unwrap();
        let daemon = Daemon {
            cp,
            store: store.clone(),
            gateways: vec![],
            router_server: Arc::new(RouterServer::new(store)),
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            orch_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            curator_handle: tokio::spawn(async {}),
        };

        daemon.start().await.unwrap();

        for _ in 0..400 {
            if !prompts.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            prompts.lock().unwrap().first().cloned(),
            Some(crate::control::RESUME_NUDGE.to_string()),
            "Daemon::start must fire reconcile, resuming the Running session with the nudge"
        );
    }

    // ---------- (f) Daemon::stop is idempotent ----------

    #[tokio::test]
    async fn daemon_stop_is_idempotent_and_stops_each_gateway_once() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let cp = ControlPlane::new_with_telemetry(
            Arc::new(store),
            Registries::new(),
            Arc::new(NoopTelemetry),
        )
        .await;
        let store = cp.store().clone();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let stops = gw.stops.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        // Long-running "loops" standing in for the real router/fan-out/
        // scheduler/orch/rail/learning tasks, so this test can assert
        // `stop()` actually aborts them.
        let router_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let fanout_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let scheduler_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let orch_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let rail_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let learning_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        let curator_handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });

        let persistence = test_agent_persistence(store.clone()).await;
        cp.attach_agent_persistence(persistence.handles()).unwrap();
        let daemon = Daemon {
            cp,
            store: store.clone(),
            gateways,
            router_server: Arc::new(RouterServer::new(store)),
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            router_handle,
            fanout_handle,
            scheduler_handle,
            orch_handle,
            rail_handle,
            learning_handle,
            curator_handle,
        };

        daemon.stop().await;
        daemon.stop().await;

        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "a second stop() must not re-invoke gateway.stop()"
        );
        // Give the abort a moment to actually land, then assert all seven
        // tracked loops are gone — stop() must not leave them running.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            daemon.router_handle.is_finished(),
            "stop() must abort the router loop"
        );
        assert!(
            daemon.fanout_handle.is_finished(),
            "stop() must abort the approval fan-out loop"
        );
        assert!(
            daemon.scheduler_handle.is_finished(),
            "stop() must abort the scheduler loop"
        );
        assert!(
            daemon.orch_handle.is_finished(),
            "stop() must abort the orch loop"
        );
        assert!(
            daemon.rail_handle.is_finished(),
            "stop() must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "stop() must abort the learning loop"
        );
        assert!(
            daemon.curator_handle.is_finished(),
            "stop() must abort the curator loop"
        );
    }

    // ---------- (g) build_daemon: telemetry selection (Finding 4) ----------

    #[test]
    fn select_telemetry_empty_endpoint_selects_console_without_warning() {
        let (_telemetry, warned) = select_telemetry("");
        assert!(
            !warned,
            "an unset otel_endpoint must select Console without a warning"
        );
    }

    #[test]
    fn select_telemetry_blank_endpoint_selects_console_without_warning() {
        let (_telemetry, warned) = select_telemetry("   ");
        assert!(
            !warned,
            "a blank (whitespace-only) otel_endpoint must be treated as unset"
        );
    }

    // Without the `otel` feature there's no real backend to try, so every
    // configured endpoint still falls back to Console with a warning (the
    // pre-Task-6 behavior). With the feature on, a syntactically valid
    // endpoint now selects the real Otel backend instead — see the
    // `#[cfg(feature = "otel")]` tests below.
    #[cfg(not(feature = "otel"))]
    #[test]
    fn select_telemetry_nonempty_endpoint_warns_and_still_falls_back_to_console() {
        let (telemetry, warned) = select_telemetry("http://localhost:4317");
        assert!(
            warned,
            "a configured otel_endpoint must signal the fallback warning when the `otel` feature is off"
        );
        assert_eq!(telemetry.backend_name(), "console");
    }

    #[cfg(feature = "otel")]
    #[test]
    fn select_telemetry_nonempty_endpoint_selects_otel_without_warning() {
        let (telemetry, warned) = select_telemetry("http://localhost:4317");
        assert!(
            !warned,
            "a configured otel_endpoint that constructs successfully must not warn"
        );
        assert_eq!(
            telemetry.backend_name(),
            "otel",
            "select_telemetry must choose the real Otel backend when the feature is on \
             and construction succeeds"
        );
    }

    #[cfg(feature = "otel")]
    #[test]
    fn select_telemetry_unparseable_endpoint_falls_back_to_console_with_warning() {
        // Not a valid URI at all — OtelTelemetry::new must return Err, and
        // select_telemetry must still fall back to Console + warn, exactly
        // as it does when the `otel` feature is off.
        let (telemetry, warned) = select_telemetry("not a url");
        assert!(
            warned,
            "a configured otel_endpoint that fails to construct must still warn"
        );
        assert_eq!(telemetry.backend_name(), "console");
    }

    #[tokio::test]
    async fn build_daemon_selects_console_telemetry_when_otel_endpoint_is_unset() {
        let (_guard, db_path) = temp_db_path();
        // No otel_endpoint set at all — build_daemon must fall back to
        // Console silently and still build successfully end-to-end.
        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: None,
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();
        assert!(daemon.gateways.is_empty());
        daemon.stop().await;
    }

    #[tokio::test]
    async fn build_daemon_selects_endpoint_backend_without_override() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings
                .set("otel_endpoint", "http://localhost:4317")
                .await
                .unwrap();
        }

        // With no `opts.telemetry` override and a non-empty otel_endpoint,
        // build_daemon must still build successfully end-to-end regardless
        // of which backend `select_telemetry` picks for it: without the
        // `otel` feature it falls back to Console + the "OTel init failed"
        // stderr warning; with the feature on and a valid endpoint it
        // selects the real Otel backend instead (no warning). Either way
        // this test's assertions are backend-agnostic — the exact
        // per-feature-state backend/warning behavior is unit-tested
        // directly above via `select_telemetry`, since capturing stderr
        // here would be awkward/flaky.
        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: None,
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();
        assert!(daemon.gateways.is_empty());
        daemon.stop().await;
    }

    // ---------- (h) the REAL spawned fan-out (Finding 1) ----------

    #[tokio::test]
    #[serial]
    async fn build_daemon_real_fanout_lets_a_blocked_turn_complete_end_to_end() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "discord").await.unwrap();
            settings.set("discord.token", "tok-xyz").await.unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let factory: Arc<dyn GatewayFactory> = Arc::new(CapturingGatewayFactory {
            captured: captured.clone(),
        });

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("discord".to_string(), factory)],
            harness_factory: Some(Arc::new(PermFakeHarnessFactory)),
        })
        .await
        .unwrap();

        daemon.start().await.unwrap();

        // Seed a project/session/surface directly (bypassing `start_session`'s
        // git-repo + fresh-random-session_pk machinery, which isn't needed
        // here) so the surface is bound BEFORE the turn starts — no race with
        // the real fan-out's `store.surfaces(session_pk)` lookup.
        seed_project(&daemon.store, "p1").await;
        seed_session(&daemon.store, "s1", "p1", Some("starter-1")).await;
        daemon
            .store
            .add_surface("discord", "chan1", "s1")
            .await
            .unwrap();

        let mut rx = daemon.cp.subscribe();
        // `continue_session` drives the (registered-but-never-started) "s1"
        // session through the SAME `PermFakeHarnessFactory` cold-resume path
        // `handle_approval`'s unit tests already exercise directly — except
        // here nothing in the test calls `handle_approval` itself: the
        // `ApprovalRequested` event this emits must be picked up by the REAL
        // `spawn_approval_fanout` task `build_daemon` wired above.
        daemon.cp.continue_session("s1", "go", &[]).await.unwrap();

        let mut saw_result = false;
        for _ in 0..200 {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(CoreEvent::Result { .. })) => {
                    saw_result = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(
            saw_result,
            "the REAL spawned fan-out must pick up ApprovalRequested, get an allow decision \
             from the FakeGateway, and let the blocked turn complete"
        );

        let msgs = daemon.store.list_messages("s1").await.unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "done"),
            "expected the post-approval assistant row, got: {msgs:?}"
        );

        daemon.stop().await;
    }

    // ---------- (i) Daemon::stop aborts the fan-out loop (Finding 2) ----------

    #[tokio::test]
    #[serial]
    async fn daemon_stop_aborts_fanout_so_a_later_approval_is_never_resolved() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: Some(Arc::new(PermFakeHarnessFactory)),
        })
        .await
        .unwrap();

        daemon.start().await.unwrap();
        // Stop BEFORE the session/approval even exists: this must abort the
        // real router + fan-out loops so nothing is left listening.
        daemon.stop().await;

        seed_project(&daemon.store, "p1").await;
        seed_session(&daemon.store, "s1", "p1", Some("starter-1")).await;
        daemon
            .store
            .add_surface("discord", "chan1", "s1")
            .await
            .unwrap();

        let mut rx = daemon.cp.subscribe();
        // The session/harness machinery is independent of `Daemon` (it lives
        // on `ControlPlane`), so this still runs and still emits
        // `ApprovalRequested` — proving the request really happened. What
        // must NOT happen is anyone resolving it, since `stop()` already
        // killed the only listener that would have.
        daemon.cp.continue_session("s1", "go", &[]).await.unwrap();

        let mut saw_approval_requested = false;
        let mut saw_result = false;
        for _ in 0..40 {
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(CoreEvent::ApprovalRequested { .. })) => saw_approval_requested = true,
                Ok(Ok(CoreEvent::Result { .. })) => saw_result = true,
                _ => {}
            }
        }

        assert!(
            saw_approval_requested,
            "the blocked turn must still emit ApprovalRequested regardless of Daemon::stop()"
        );
        assert!(
            !saw_result,
            "with the fan-out loop aborted by stop(), nothing must resolve the approval, so \
             the turn must never complete"
        );

        let msgs = daemon.store.list_messages("s1").await.unwrap();
        assert!(
            !msgs
                .iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "done"),
            "the post-approval assistant row must never appear since nothing resolved the \
             approval: got {msgs:?}"
        );
    }

    // ---------- (j) daemon hosts scheduler + orch + rail + learning + curator loops (Tasks 10, 9, 8, 10) ----------

    #[tokio::test]
    async fn daemon_uses_injected_config_root_not_database_parent() {
        let db_dir = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("nested/store.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: config.path().to_path_buf(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();
        let persistence = daemon.cp.agent_persistence().unwrap();
        assert!(Arc::ptr_eq(&daemon.agents, &persistence.registry));
        assert!(Arc::ptr_eq(&daemon.agent_knowledge, &persistence.knowledge));
        assert!(Arc::ptr_eq(&daemon.learning_queue, &persistence.learning));
        assert!(config.path().join("agents/index.yaml").exists());
        assert!(!db_path.parent().unwrap().join("agents").exists());
        daemon.stop().await;
    }

    #[tokio::test]
    async fn dropping_daemon_without_stop_aborts_owned_workers() {
        let (_guard, db_path) = temp_db_path();
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();
        let learning = daemon.learning_handle.abort_handle();
        let scheduler = daemon.scheduler_handle.abort_handle();
        assert!(!learning.is_finished());
        assert!(!scheduler.is_finished());

        drop(daemon);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !learning.is_finished() || !scheduler.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the daemon must cancel its owned workers");
    }

    #[tokio::test]
    async fn daemon_hosts_and_stop_aborts_scheduler_orch_rail_learning_and_curator_loops() {
        let (_guard, db_path) = temp_db_path();
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
        })
        .await
        .unwrap();

        assert!(
            !daemon.scheduler_handle.is_finished(),
            "scheduler loop must be live"
        );
        assert!(!daemon.orch_handle.is_finished(), "orch loop must be live");
        assert!(!daemon.rail_handle.is_finished(), "rail loop must be live");
        assert!(
            !daemon.learning_handle.is_finished(),
            "learning loop must be live"
        );
        assert!(
            !daemon.curator_handle.is_finished(),
            "curator loop must be live"
        );

        daemon.stop().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            daemon.scheduler_handle.is_finished(),
            "stop() must abort the scheduler loop"
        );
        assert!(
            daemon.orch_handle.is_finished(),
            "stop() must abort the orch loop"
        );
        assert!(
            daemon.rail_handle.is_finished(),
            "stop() must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "stop() must abort the learning loop"
        );
        assert!(
            daemon.curator_handle.is_finished(),
            "stop() must abort the curator loop"
        );
    }
}
