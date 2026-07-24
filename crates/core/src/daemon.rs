//! Daemon composition root: wires `Store` → settings → telemetry → harness
//! registries → `ControlPlane` → gateways → the outbound `Router` → the
//! approval fan-out.
//!
//! [`build_daemon`] is the single entry point; [`Daemon::start`]/`stop` drive
//! the lifecycle. The approval fan-out is kept in a standalone, unit-testable
//! `pub(crate)` function ([`handle_approval`]) separate from the broadcast
//! loop that spawns it.

use crate::agents::knowledge::AgentKnowledgeStore;
use crate::agents::learning_queue::LearningQueue;
use crate::agents::registry::AgentRegistry;
use crate::control::ControlPlane;
use crate::domain::{ApprovalDecision, ApprovalRequest, CoreEvent, Principal, Surface};
use crate::gateway::{Gateway, GatewayFactory, GatewayStatus};
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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex};
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
    /// Native (in-process) gateway factories available to wire, keyed by the
    /// id an entry in the `enabled_gateways` setting names. Empty in
    /// production today — Discord migrated to a WASM component bundle (wired
    /// via `build_wasm_gateways` below) — but retained as the generic
    /// injection seam a future native gateway (and the daemon tests) use.
    pub extra_gateway_factories: Vec<(String, Arc<dyn GatewayFactory>)>,
    /// Test seam: replace the single native harness factory with a fake.
    /// `None` (production) uses the real native runtime.
    pub harness_factory: Option<Arc<dyn HarnessFactory>>,
    /// Test/override seam for the first-party component-bundle bootstrap
    /// (Task 11a). `None` (production) uses the real reqwest client, the
    /// compiled-in first-party trusted key, the settings/default release base
    /// URL, and the per-user install root — see [`ComponentBootstrapConfig`].
    pub component_bootstrap: Option<ComponentBootstrapConfig>,
}

/// Inputs to the first-party component-bundle bootstrap step (Task 11a),
/// injectable via [`BuildDaemonOpts::component_bootstrap`]. `None` there
/// resolves each field to its production default: a real
/// [`crate::plugins::remote_catalog::ReqwestCatalogHttp`], the
/// [`crate::plugins::first_party_key::first_party_trusted_keys`] map (empty
/// while the placeholder key is in place → fail-closed no-op), the
/// `component_release_base_url` setting (or
/// [`crate::plugins::remote_catalog::DEFAULT_COMPONENT_RELEASE_BASE_URL`]), and
/// [`crate::plugins::bundle::installed_bundle_root`]. Tests inject a fake
/// `CatalogHttp`, a generated trusted key, and a throwaway root to exercise the
/// pipeline hermetically.
pub struct ComponentBootstrapConfig {
    pub http: Arc<dyn crate::plugins::remote_catalog::CatalogHttp>,
    pub trusted_keys: std::collections::HashMap<String, [u8; 32]>,
    pub base_url: String,
    pub root: PathBuf,
}

/// A fully wired daemon: control plane, shared store handle, and the
/// gateways `build_daemon` constructed. `store` is the SAME `Arc<Store>` the
/// `ControlPlane` holds internally (via `cp.store()`), so callers needing
/// direct DB access (e.g. HTTP read endpoints) share one connection pool.
pub struct Daemon {
    pub cp: Arc<ControlPlane>,
    pub store: Arc<Store>,
    pub gateways: Vec<Arc<dyn Gateway>>,
    /// The boot-gated inbound Router passed to production gateways. It stays
    /// closed through boot recovery so gateway events cannot mutate sessions
    /// while reconciliation takes its startup snapshot.
    router_in: Arc<Router>,
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
    gateway_status_handles: Mutex<Vec<JoinHandle<()>>>,
    stopped: AtomicBool,
    /// Serializes the complete startup lifecycle, including gateway startup,
    /// recovery, and boot admission. A terminal boot failure is observed by a
    /// waiting caller before it can acquire any gateway resources.
    lifecycle: AsyncMutex<()>,
    /// Set only after the boot admission latch is open. Under `lifecycle`, a
    /// repeated successful start is a no-op rather than a second gateway boot.
    started: AtomicBool,
    /// Completion state for boot recovery. Access occurs under `lifecycle`, so
    /// an atomic flag is sufficient and avoids a nested recovery mutex.
    prompt_claim_recovery_complete: AtomicBool,
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
    /// The background-rail drainer's loop (`background_rail::spawn_runner`),
    /// tracked for the same reason as `scheduler_handle` — the
    /// daemon is the single always-on engine host for it too.
    rail_handle: JoinHandle<()>,
    /// The durable per-agent learning queue worker (`learning::spawn_runner`),
    /// tracked for the same reason as `rail_handle`. It claims pending queue
    /// rows and applies them to the owning agent's OKF bundle.
    learning_handle: JoinHandle<()>,
    /// Periodic source-session artifact retention cleanup.
    artifact_retention_handle: JoinHandle<()>,
}

impl Daemon {
    /// Start every gateway and perform endpoint autostart before the serial,
    /// boot-only recovery latch. The latch recovers abandoned prompt claims,
    /// awaits best-effort reconciliation of prior-process `Running` sessions,
    /// then starts one pending FIFO head for every idle session. Reconcile's
    /// snapshot completes before delivery can make an idle session `Running`,
    /// preventing a second harness and `RESUME_NUDGE` for the new delivery.
    ///
    /// Gateway startup comes first because reconciliation emits resumed status
    /// events that must be routable to an already-connected gateway. A boot
    /// failure is terminal for this `Daemon`; callers must construct a new one
    /// before retrying, so stopped tasks and gateway ownership are never reused.
    ///
    /// Partial-failure rollback: if gateway N fails to start, every gateway
    /// 0..N-1 that DID start is stopped (best-effort — errors swallowed,
    /// same as `stop()`), the router/fan-out/scheduler/rail/learning
    /// handles are aborted, and the daemon is marked stopped (reusing the same
    /// idempotency flag `stop()` checks) before the error is returned.
    /// Marking it stopped here means a caller's own best-effort `stop()` on
    /// a `start()` error (e.g. `daemon_cmd::build_and_start`) is a safe
    /// no-op instead of re-stopping gateway 0..N-1 a second time.
    ///
    /// Reconcile itself is not a tracked task: it completes during the boot
    /// sequence, while each resume it starts is an independent `spawn_prompt`
    /// background turn owned by `ControlPlane`.
    ///
    /// After gateway startup, endpoint autostart runs: if the persisted
    /// `endpoint_autostart` setting is `"1"`, `router_server` is started on the
    /// persisted `endpoint_port` (default 21128) — the same autostart Cockpit's
    /// setup hook used to perform. A failure here is logged and swallowed rather
    /// than propagated: a broken endpoint server must not prevent boot
    /// recovery, reconciliation, or idle queue delivery.
    pub async fn start(&self) -> anyhow::Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.stopped.load(Ordering::SeqCst) {
            anyhow::bail!("daemon failed to boot");
        }
        if self.started.load(Ordering::SeqCst) {
            return Ok(());
        }
        for (idx, gw) in self.gateways.iter().enumerate() {
            if let Err(e) = gw.start().await {
                if !self.stopped.swap(true, Ordering::SeqCst) {
                    for started in &self.gateways[..idx] {
                        let _ = started.stop().await;
                    }
                    self.router_handle.abort();
                    self.fanout_handle.abort();
                    self.scheduler_handle.abort();
                    self.rail_handle.abort();
                    self.learning_handle.abort();
                    self.artifact_retention_handle.abort();
                }
                self.abort_gateway_status_listeners();
                return Err(e);
            }
        }

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
        if let Err(error) = self
            .recover_reconcile_and_deliver_idle_queues_on_boot()
            .await
        {
            self.fail_boot_and_rollback().await;
            for gateway in &self.gateways {
                let _ = gateway.stop().await;
            }
            return Err(error);
        }
        self.router_in.open_boot_admission();
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Mark boot admission failed and abort the background tasks exactly once.
    /// Callers stop any gateways already started by this daemon separately,
    /// because start failure knows precisely which of them acquired resources.
    async fn fail_boot_and_rollback(&self) {
        self.router_in.fail_boot_admission();
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        self.router_handle.abort();
        self.fanout_handle.abort();
        self.scheduler_handle.abort();
        self.rail_handle.abort();
        self.learning_handle.abort();
        self.artifact_retention_handle.abort();
        self.abort_gateway_status_listeners();
        self.router_server.stop().await;
    }

    /// Recover prior-process claims, reconcile prior-process Running sessions,
    /// and drain pending idle queue heads exactly once for this daemon. The
    /// startup lifecycle lock serializes all three phases: reconcile's Running-session
    /// snapshot completes before delivery can make an idle session Running.
    /// Completion is set only after recovery and delivery finish.
    async fn recover_reconcile_and_deliver_idle_queues_on_boot(&self) -> anyhow::Result<()> {
        if self.prompt_claim_recovery_complete.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.store.recover_abandoned_session_prompt_claims().await?;
        // Reconcile was previously best-effort and detached; keep an error
        // non-fatal while awaiting its scan so delivery cannot race it.
        let _ = self.cp.reconcile().await;
        self.deliver_pending_idle_session_prompts_on_boot().await?;
        self.prompt_claim_recovery_complete
            .store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Start one pending FIFO head for each idle session after recovery and
    /// reconcile have completed. The control plane's atomic Idle-to-Running
    /// claim prevents a concurrent start from duplicating a delivery.
    async fn deliver_pending_idle_session_prompts_on_boot(&self) -> anyhow::Result<()> {
        for session_pk in self.store.pending_session_prompt_session_pks().await? {
            let _ = self
                .cp
                .deliver_next_queued_session_prompt(&session_pk)
                .await;
        }
        Ok(())
    }

    /// Idempotent teardown: stop every gateway (errors swallowed so one
    /// failing gateway can't block the rest of the shutdown),
    /// abort the router and approval fan-out broadcast-consumer loops (which
    /// also aborts any in-flight per-approval races the fan-out spawned —
    /// see `spawn_approval_fanout`), abort the scheduler, rail, and
    /// learning loops, stop the endpoint server, then flush
    /// telemetry. A second call is a no-op.
    pub async fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        // Stops every gateway — native factory gateways AND each registered
        // `WasmGateway`, whose `stop()` gracefully stops its own supervisor
        // (calls the component's `stop()` and aborts the supervisor task).
        for gw in &self.gateways {
            let _ = gw.stop().await;
        }
        // Give already-enqueued listener/worker tasks one scheduling
        // opportunity to begin processing a just-published terminal status
        // transition (e.g. reach the point of persisting a `queued`
        // automation run) before they are aborted below. This only yields
        // once to the scheduler — it does not wait on network I/O or
        // outbound hook delivery — so it reduces (but does not eliminate)
        // the race between a gateway's own graceful-stop terminal status
        // publish and its listener task being aborted, without
        // reintroducing a synthesized start/stop event (rejected design:
        // the gateway is the sole event producer).
        tokio::task::yield_now().await;
        self.abort_gateway_status_listeners();
        self.router_handle.abort();
        self.fanout_handle.abort();
        self.scheduler_handle.abort();
        self.rail_handle.abort();
        self.learning_handle.abort();
        self.artifact_retention_handle.abort();
        self.router_server.stop().await;
        // Track D: gracefully stop every spawned extension subprocess. Safe
        // even when nothing was ever spawned (every test daemon, or a real
        // daemon whose entry never reached `spawn_extensions`) — see
        // `ControlPlane::shutdown_extensions`'s doc.
        self.cp.shutdown_extensions().await;
        self.telemetry.shutdown();
    }

    fn abort_gateway_status_listeners(&self) {
        for listener in self.gateway_status_handles.lock().unwrap().drain(..) {
            listener.abort();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.abort_gateway_status_listeners();
        self.router_handle.abort();
        self.fanout_handle.abort();
        self.scheduler_handle.abort();
        self.rail_handle.abort();
        self.learning_handle.abort();
        self.artifact_retention_handle.abort();
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
/// → the cron scheduler (`scheduler::spawn_runner`), background-rail drainer
/// (`background_rail::spawn_runner`), and learning worker
/// (`learning::spawn_runner`) loops, spawned here because the daemon
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
pub async fn build_daemon(mut opts: BuildDaemonOpts) -> anyhow::Result<Daemon> {
    let store = Arc::new(Store::open(&opts.db_path).await?);
    // Auto-connect the MiMo/OpenCode free tiers on first run so a fresh
    // install has runnable models (and the `free` route below has candidates)
    // without any "Add account" step. Idempotent + respects user deletion.
    crate::agents::bootstrap::ensure_free_providers_seeded(&store).await?;
    // Bootstrap the first-party component bundles (MiMo/OpenCode) on first run
    // so their signed provider bundles can land + activate without any manual
    // install. Non-propagating (warn-and-continue, `let _`) — a download
    // failure must NEVER fail the whole daemon; the retryable status is
    // persisted and surfaced via the `component_bootstrap_status` RPC. Mirrors
    // the gateway-build warn+skip pattern below, NOT the `?` on the
    // free-providers line above.
    {
        let cfg = match opts.component_bootstrap.take() {
            Some(cfg) => cfg,
            None => ComponentBootstrapConfig {
                http: Arc::new(crate::plugins::remote_catalog::ReqwestCatalogHttp::new()),
                trusted_keys: crate::plugins::first_party_key::first_party_trusted_keys(),
                base_url: store
                    .get_setting_raw("component_release_base_url")
                    .await
                    .ok()
                    .flatten()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        crate::plugins::remote_catalog::DEFAULT_COMPONENT_RELEASE_BASE_URL
                            .to_string()
                    }),
                root: crate::plugins::bundle::installed_bundle_root(),
            },
        };
        let installer = crate::plugins::bundle::ComponentBundleInstaller::new(
            cfg.root.clone(),
            store.as_ref().clone(),
        );
        let _ = crate::plugins::remote_catalog::bootstrap_first_party_components(
            &store,
            cfg.http.as_ref(),
            &cfg.trusted_keys,
            &installer,
            &cfg.base_url,
        )
        .await;
    }
    // Seed the explicit "installed providers" set once (defaults ∪ families
    // that already have a connection) so the Models list gates on it. Visibility
    // only — routing still uses enabled connections. Idempotent.
    crate::llm_router::installed::ensure_default_installed_providers(&store).await?;
    // Re-register every persisted user-defined custom provider as a leaked
    // `&'static` descriptor so the router resolves its family. Must run before
    // `ensure_default_routes` so any custom-targeted route resolves at boot.
    crate::llm_router::custom::load_and_register_all(&store).await?;
    // Default durable profiles target the `free` route, so it must exist before
    // the registry below loads: the registry validates every profile exactly
    // once, at load, and caches the verdict in `AgentSnapshot`. Creating the
    // route afterwards left a fresh install's default agent permanently stamped
    // "route `free` does not exist or is not executable" until the next restart.
    // Needs connections, which the seeding above provides; a fresh daemon with
    // none remains intentionally unconfigured.
    crate::agents::bootstrap::ensure_default_routes(&store).await?;
    // Agents last: everything they validate against (connections, the `free`
    // route) now exists.
    let persistence = crate::agents::bootstrap::initialize_agent_persistence(
        opts.config_root,
        Arc::clone(&store),
    )
    .await?;
    // Refine the `free` route in the background: probe the MiMo/OpenCode free
    // models and keep only the ones that answer, leaving the synchronous
    // first-concrete baseline in place if none do. Non-blocking; boot proceeds.
    crate::agents::free_route::spawn_free_route_rebuild(Arc::clone(&store));
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

    let cp = ControlPlane::new_with_telemetry(
        Arc::clone(&store),
        registries,
        Arc::clone(&telemetry),
        persistence.clone(),
    )
    .await;
    cp.delegation().recover_after_restart().await?;
    let settings = SettingsStore::new(Arc::clone(&store));

    let factories: HashMap<String, Arc<dyn GatewayFactory>> =
        opts.extra_gateway_factories.into_iter().collect();
    let mut gateways: Vec<Arc<dyn Gateway>> = Vec::new();
    for id in csv(settings.get("enabled_gateways").await?.as_deref()) {
        let Some(factory) = factories.get(&id) else {
            continue; // no registered factory for this id — skip silently
        };
        // Assemble the gateway's config from its CATALOG-declared settings
        // fields, if any. A native gateway with no catalog descriptor (or no
        // declared fields) builds with an empty config.
        let mut config = serde_json::Map::new();
        if let Some(descriptor) = CATALOG.gateway(&id) {
            for field in descriptor.fields {
                let value = settings.get(field.key).await?.unwrap_or_default();
                config.insert(field.key.to_string(), serde_json::Value::String(value));
            }
        }
        // A gateway that cannot be built from its settings is a configuration
        // gap, not an engine fault — skip it (like the case above) rather than
        // failing the whole daemon: a clean machine must still boot the engine
        // even when an enabled gateway is missing required credentials (this
        // is why a fatal error here once left a fresh install unable to boot —
        // the daemon exited and Cockpit's `setup()` panicked before showing
        // its window). The gateway stays enabled in settings and starts on the
        // next boot once its fields are filled in.
        let gw = match factory.create(&serde_json::Value::Object(config)) {
            Ok(gw) => gw,
            Err(e) => {
                eprintln!("[ryuzi] gateway {id} is enabled but could not start: {e} — skipping");
                continue;
            }
        };
        gateways.push(gw);
    }

    // Construct + register one `WasmGateway` per enabled, long-lived gateway
    // bundle (design §5.5), alongside any native factory gateways above.
    // Registering them into `gateways` HERE — before the status subscription,
    // the outbound/inbound `Router`, and the approval fan-out below — wires each
    // WASM gateway (the migrated Discord component and any future one) into all
    // three through the very same paths native gateways use, with NO plugin-id
    // host branch. Discovery is warn-and-skip and returns empty on a clean
    // install (nothing enabled/long-lived), so the common case adds nothing.
    // This is the sole gateway path in production now that native Discord has
    // been removed.
    for gw in crate::plugins::wasm_gateway_bridge::build_wasm_gateways(
        Arc::clone(&store),
        &settings,
        Arc::clone(&telemetry),
        &crate::plugins::bundle::installed_bundle_root(),
    )
    .await
    {
        gateways.push(gw as Arc<dyn Gateway>);
    }

    // Register the live gateways with the control plane so the revocation entry
    // points (signed-feed blocklist sweep, component-release rollback) — which
    // hold only an `Arc<ControlPlane>` — can reach a running gateway supervisor
    // by id and stop it MID-SESSION at a safe boundary. The `ControlPlane` keeps
    // only `Weak` handles, so this never keeps a gateway alive past shutdown.
    cp.register_gateways(&gateways);

    // Activate any enabled, installed provider component bundle (design §5.5 /
    // plan Task 16): register a live WASM provider transport under each router
    // provider id its manifest declares (e.g. the `mimo` bundle backs
    // `mimo-free`). Registration is a side effect into the process-wide provider
    // registry the router's `anthropic_messages_stream` already diverts to via
    // `wasm_provider(&conn.provider)`, so a routed free-tier connection resolves
    // to its component through that same generic, data-driven seam — nothing is
    // threaded into the `Router` below, and there is no plugin-id host branch.
    // Warn-and-skip discovery registers nothing on a clean install (no enabled
    // provider bundle), so the common case adds nothing.
    let registered_providers = crate::plugins::wasm_provider::discover_provider_components(
        Arc::clone(&store),
        &settings,
        Arc::clone(&telemetry),
        &crate::plugins::bundle::installed_bundle_root(),
    )
    .await;
    if !registered_providers.is_empty() {
        tracing::info!(
            providers = ?registered_providers,
            "registered {} wasm provider transport(s)",
            registered_providers.len()
        );
    }

    let gateway_status_handles = Mutex::new(subscribe_gateway_statuses(Arc::clone(&cp), &gateways));

    // Two `Router` instances sharing the same `cp`/`store` — see `router.rs`'s
    // module doc. `router_out` drives the outbound render loop (`run`
    // consumes `self`); `router_in` is handed to every gateway via
    // `set_router` (a gateway whose inbound routing needs a `Router` — e.g.
    // the WASM gateway bridge — needs a `Router` to exist, but a `Router`
    // needs the already-built gateway list, so gateways are built first and
    // given a `Router` handle right after one exists; most gateways ignore it
    // via `Gateway::set_router`'s default no-op).
    let router_out = Router::new(Arc::clone(&cp), gateways.clone());
    let router_handle = tokio::spawn(router_out.run(cp.subscribe()));

    let router_in = Arc::new(Router::new_boot_gated(Arc::clone(&cp), gateways.clone()));
    for gw in &gateways {
        gw.set_router(Arc::clone(&router_in));
    }

    let fanout_handle =
        spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), gateways.clone());

    // The daemon is the single always-on engine host for cron scheduling. The scheduler's
    // job_last_fired anchor is single-host-only — never spawn a second one.
    let scheduler_handle = crate::scheduler::spawn_runner(Arc::clone(&cp));
    let artifact_retention_handle = spawn_artifact_retention(Arc::clone(&cp), Arc::clone(&store));
    let rail_handle = crate::background_rail::spawn_runner(Arc::clone(&cp));
    let learning_handle = crate::learning::spawn_runner(Arc::clone(&persistence.learning));
    let router_server = Arc::new(RouterServer::new(Arc::clone(&store)));
    router_server.attach_control_plane(&cp);

    Ok(Daemon {
        cp,
        store,
        gateways,
        router_in,
        router_server,
        agents: persistence.registry,
        agent_knowledge: persistence.knowledge,
        learning_queue: persistence.learning,
        telemetry,
        gateway_status_handles,
        stopped: AtomicBool::new(false),
        lifecycle: AsyncMutex::new(()),
        started: AtomicBool::new(false),
        prompt_claim_recovery_complete: AtomicBool::new(false),
        router_handle,
        fanout_handle,
        scheduler_handle,
        rail_handle,
        learning_handle,
        artifact_retention_handle,
    })
}

/// Run artifact retention periodically from the sole daemon host. The first
/// tick runs immediately so a restart also resumes cleanup after a previous
/// interruption; later ticks are hourly. Failures are retryable and never
/// prevent the daemon from serving sessions.
fn spawn_artifact_retention(cp: Arc<ControlPlane>, store: Arc<Store>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let settings = SettingsStore::new(store);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        loop {
            interval.tick().await;
            let retention_days = settings
                .get("artifact_retention_days")
                .await
                .ok()
                .flatten()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(30);
            if let Err(error) = cp
                .artifacts()
                .purge_expired_archives(crate::paths::now_ms(), retention_days)
                .await
            {
                tracing::warn!("artifact retention cleanup failed: {error}");
            }
        }
    })
}

/// Per-gateway queue bound between the broadcast receiver and its sole
/// persistence worker. `send().await` deliberately applies backpressure after
/// this many transitions rather than spawning unbounded hook-delivery tasks.
const GATEWAY_STATUS_DELIVERY_CAPACITY: usize = 64;

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Subscribe to every status-capable gateway during daemon construction. The
/// initial snapshot is a non-emitting baseline; every later distinct broadcast
/// event is forwarded in receive order to one serial persistence worker.
///
/// If a receiver lags past the producer's bounded event buffer, the listener
/// warns and resynchronizes its baseline to the publisher's newest snapshot.
/// This consciously drops the unavailable intermediate transitions rather than
/// fabricating a transition, while normal bursts fit within the 128-event
/// producer buffer. The worker is owned by its listener task, so aborting the
/// listener also aborts an in-flight hook delivery and drops the queue.
fn subscribe_gateway_statuses(
    cp: Arc<ControlPlane>,
    gateways: &[Arc<dyn Gateway>],
) -> Vec<JoinHandle<()>> {
    let mut listeners = Vec::new();
    for gateway in gateways {
        let Some(mut subscription) = gateway.subscribe_status() else {
            continue;
        };
        let gateway_id = gateway.id().to_string();
        let listener_cp = Arc::clone(&cp);
        listeners.push(tokio::spawn(async move {
            let (delivery_tx, mut delivery_rx) =
                mpsc::channel::<(GatewayStatus, GatewayStatus)>(GATEWAY_STATUS_DELIVERY_CAPACITY);
            let worker_cp = Arc::clone(&listener_cp);
            let worker_gateway_id = gateway_id.clone();
            let _worker = AbortOnDrop(tokio::spawn(async move {
                while let Some((previous, status)) = delivery_rx.recv().await {
                    worker_cp
                        .observe_gateway_status_transition(
                            &worker_gateway_id,
                            previous.as_str(),
                            status.as_str(),
                        )
                        .await;
                }
            }));

            let mut previous = subscription.initial;
            loop {
                match subscription.events.recv().await {
                    Ok(status) if status != previous => {
                        let transition = (previous, status);
                        previous = status;
                        if delivery_tx.send(transition).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        previous = subscription.resync();
                        tracing::warn!(
                            gateway_id = %gateway_id,
                            skipped,
                            status = previous.as_str(),
                            "gateway status listener lagged; resynchronized baseline to latest status"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
    }
    listeners
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
                    run_id,
                    requesting_agent_id,
                    requesting_agent_name,
                    request_id,
                    tool,
                    summary,
                    approval_kind,
                    input: _,
                    principal,
                }) => {
                    if approval_kind != crate::domain::ApprovalKind::Tool {
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
                            &run_id,
                            &requesting_agent_id,
                            &requesting_agent_name,
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

/// Core approval fan-out decision, callable directly (no broadcast loop
/// needed) so it's unit-testable: reads `approver_role_ids` from settings and
/// `started_by` from the session, then resolves gateway decisions through
/// `cp.resolve_approval_bool`.
///
/// - No surfaces bound to the session (after filtering to gateways we know
///   about) → leaves the request pending for Cockpit.
/// - Otherwise races `gw.request_approval` across every known surface via a
///   loop over `futures::future::select_all`: a per-gateway `Err` REMOVES
///   that future from the race (so one erroring gateway can never out-race a
///   slower legitimate human approval on another surface) and the remaining
///   futures keep racing. If every gateway errors, the request stays pending
///   so Cockpit can still provide the explicit response.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_approval(
    cp: &Arc<ControlPlane>,
    store: &Arc<Store>,
    gateways: &[Arc<dyn Gateway>],
    session_pk: &str,
    run_id: &str,
    requesting_agent_id: &str,
    requesting_agent_name: &str,
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
        // No gateway surface can answer this — leave it pending for Cockpit
        // rather than auto-denying it.
        return;
    }

    let req = ApprovalRequest {
        run_id: run_id.to_string(),
        requesting_agent_id: requesting_agent_id.to_string(),
        requesting_agent_name: requesting_agent_name.to_string(),
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
    // the race instead of treating it as an instant deny vote. If every
    // gateway errors, leave the approval pending for another surface.
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

    // A genuine `approval_timeout_ms` elapse auto-rejects (there is no
    // reasonable default decision otherwise). But every gateway simply
    // erroring — with time still remaining — must NOT be treated the same as
    // a timeout: it must leave the request pending for another surface or
    // Cockpit, exactly like the fully-surfaceless case above.
    match tokio::time::timeout(Duration::from_millis(timeout_ms), race).await {
        Ok(Some(decision)) => {
            cp.resolve_approval_bool(
                run_id,
                request_id,
                matches!(
                    decision,
                    ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways
                ),
            );
        }
        Ok(None) => {}
        Err(_) => {
            cp.resolve_approval_bool(run_id, request_id, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        NewMessage, PermMode, Project, QueuedSessionPrompt, Session, SessionKind, SessionStatus,
    };
    use crate::gateway::{GatewayStatus, MessageRef};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::telemetry::NoopTelemetry;
    use async_trait::async_trait;
    use serial_test::serial;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::task::Poll;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ---------- shared test plumbing ----------

    async fn drain_http_request(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
        const MAX_REQUEST_BYTES: usize = 64 * 1024;

        let mut request = Vec::with_capacity(4096);
        let mut request_len = None;
        let mut buf = [0u8; 4096];
        loop {
            if request.len() == MAX_REQUEST_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP request exceeds 64 KiB test-server limit",
                ));
            }

            let read_len = (MAX_REQUEST_BYTES - request.len()).min(buf.len());
            let read = stream.read(&mut buf[..read_len]).await?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before complete HTTP request",
                ));
            }
            request.extend_from_slice(&buf[..read]);

            if request_len.is_none() {
                let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = std::str::from_utf8(&request[..header_end]).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "HTTP headers are not valid UTF-8",
                    )
                })?;
                let mut lines = headers.split("\r\n");
                if lines.next().filter(|line| !line.is_empty()).is_none() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "HTTP request line is missing",
                    ));
                }
                let mut content_length = None;
                for line in lines {
                    let (name, value) = line.split_once(':').ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "HTTP header is malformed",
                        )
                    })?;
                    if name.eq_ignore_ascii_case("content-length") {
                        if content_length.is_some() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "HTTP Content-Length appears more than once",
                            ));
                        }
                        content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "HTTP Content-Length is malformed",
                            )
                        })?);
                    }
                }
                let total_len = (header_end + 4)
                    .checked_add(content_length.unwrap_or(0))
                    .filter(|&len| len <= MAX_REQUEST_BYTES)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "HTTP request exceeds 64 KiB test-server limit",
                        )
                    })?;
                request_len = Some(total_len);
            }

            if request.len() >= request_len.expect("HTTP header end must set request length") {
                return Ok(());
            }
        }
    }

    fn temp_db_path() -> (tempfile::NamedTempFile, PathBuf) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        (f, path)
    }

    // ---------- first-party component bootstrap (Task 11a) ----------

    /// Deterministic bundle-signing key + its trusted map, for the daemon
    /// bootstrap wiring tests.
    fn bootstrap_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[13u8; 32])
    }
    fn bootstrap_trusted_keys() -> std::collections::HashMap<String, [u8; 32]> {
        std::collections::HashMap::from([(
            "first-party".to_string(),
            bootstrap_signing_key().verifying_key().to_bytes(),
        )])
    }

    /// The four artifacts (`ryuzi-plugin.toml`, `release.json`, `plugin.sig`,
    /// wasm) for one bundle, plus the wasm's URL.
    struct BootstrapArtifacts {
        manifest_toml: Vec<u8>,
        release_json: Vec<u8>,
        sig_json: Vec<u8>,
        wasm: Vec<u8>,
        component_url: String,
    }

    fn build_bootstrap_artifacts(base: &str, id: &str, version: &str) -> BootstrapArtifacts {
        use base64::Engine as _;
        use ed25519_dalek::Signer as _;
        use sha2::Digest as _;
        let wasm = format!("wasm {id} {version}").into_bytes();
        let sha = format!("{:x}", sha2::Sha256::digest(&wasm));
        let component = format!("{id}.wasm");
        let component_url = format!("{base}/{id}.wasm");
        let manifest_toml = format!(
            "id = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\nwit-api = \"^0.1.0\"\nlifecycle = \"singleton\"\ncomponent = \"{component}\"\n"
        )
        .into_bytes();
        let release_json = format!(
            "{{\"id\":\"{id}\",\"version\":\"{version}\",\"wit-api\":\"0.1.0\",\"component_url\":\"{component_url}\",\"component_sha256\":\"{sha}\"}}"
        )
        .into_bytes();
        let signature = bootstrap_signing_key().sign(&release_json);
        let sig_json = serde_json::to_vec(&serde_json::json!({
            "key_id": "first-party",
            "signature": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        }))
        .unwrap();
        BootstrapArtifacts {
            manifest_toml,
            release_json,
            sig_json,
            wasm,
            component_url,
        }
    }

    /// A `CatalogHttp` fake serving canned bodies keyed by exact URL; anything
    /// unregistered is a 404.
    struct FakeBootstrapHttp {
        routes: Mutex<std::collections::HashMap<String, (u16, Vec<u8>)>>,
    }
    impl FakeBootstrapHttp {
        fn new() -> Self {
            Self {
                routes: Mutex::new(std::collections::HashMap::new()),
            }
        }
        fn register(&self, base: &str, id: &str, a: &BootstrapArtifacts) {
            let mut routes = self.routes.lock().unwrap();
            routes.insert(
                format!("{base}/{id}.ryuzi-plugin.toml"),
                (200, a.manifest_toml.clone()),
            );
            routes.insert(
                format!("{base}/{id}.release.json"),
                (200, a.release_json.clone()),
            );
            routes.insert(
                format!("{base}/{id}.release.json.sig"),
                (200, a.sig_json.clone()),
            );
            routes.insert(a.component_url.clone(), (200, a.wasm.clone()));
        }
    }
    #[async_trait]
    impl crate::plugins::remote_catalog::CatalogHttp for FakeBootstrapHttp {
        async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
            Ok(self
                .routes
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .unwrap_or((404, vec![])))
        }
    }

    // First run must attempt both first-party bundles and, given a fake signed
    // catalog serving valid releases, land + activate BOTH — without failing
    // `build_daemon`.
    #[tokio::test]
    async fn build_daemon_bootstraps_first_party_components() {
        let (_guard, db_path) = temp_db_path();
        let base = "http://bootstrap.test/latest";
        let http = Arc::new(FakeBootstrapHttp::new());
        http.register(
            base,
            "mimo",
            &build_bootstrap_artifacts(base, "mimo", "0.1.0"),
        );
        http.register(
            base,
            "opencode",
            &build_bootstrap_artifacts(base, "opencode", "0.1.0"),
        );
        let root = tempfile::tempdir().unwrap();

        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
            component_bootstrap: Some(ComponentBootstrapConfig {
                http,
                trusted_keys: bootstrap_trusted_keys(),
                base_url: base.to_string(),
                root: root.path().to_path_buf(),
            }),
        })
        .await
        .expect("build_daemon must succeed with a successful bootstrap");

        assert_eq!(
            daemon
                .store
                .active_component_release("mimo")
                .await
                .unwrap()
                .unwrap()
                .version,
            "0.1.0"
        );
        assert!(daemon
            .store
            .active_component_release("opencode")
            .await
            .unwrap()
            .is_some());
        assert!(
            daemon
                .store
                .get_setting_raw(crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_RETRY)
                .await
                .unwrap()
                .is_none(),
            "a fully successful bootstrap records no retry status"
        );
    }

    // When BOTH downloads fail, the daemon must still build (Ok) and record a
    // retryable bootstrap status for Cockpit to surface.
    #[tokio::test]
    async fn build_daemon_survives_component_bootstrap_download_failure() {
        let (_guard, db_path) = temp_db_path();
        // A fake that 404s everything → both bundles fail.
        let http = Arc::new(FakeBootstrapHttp::new());
        let root = tempfile::tempdir().unwrap();

        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
            component_bootstrap: Some(ComponentBootstrapConfig {
                http,
                trusted_keys: bootstrap_trusted_keys(),
                base_url: "http://bootstrap.test/latest".to_string(),
                root: root.path().to_path_buf(),
            }),
        })
        .await
        .expect("a bootstrap download failure must NEVER fail build_daemon");

        assert!(daemon
            .store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_none());
        assert!(
            daemon
                .store
                .get_setting_raw(crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_RETRY)
                .await
                .unwrap()
                .is_some(),
            "both-fail must record a retryable bootstrap status"
        );
    }

    async fn test_agent_persistence(
        store: Arc<Store>,
    ) -> crate::agents::bootstrap::AgentPersistence {
        let config = tempfile::tempdir().unwrap();
        let persistence = test_agent_persistence_at(store, config.path().to_path_buf()).await;
        std::mem::forget(config);
        persistence
    }

    async fn test_agent_persistence_at(
        store: Arc<Store>,
        config_root: PathBuf,
    ) -> crate::agents::bootstrap::AgentPersistence {
        crate::agents::bootstrap::initialize_agent_persistence(config_root.clone(), store.clone())
            .await
            .unwrap();
        crate::llm_router::connections::add_connection(
            &store,
            crate::llm_router::connections::ConnectionRow {
                id: "test-anthropic".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Test Anthropic".into(),
                priority: 0,
                enabled: true,
                data: crate::llm_router::connections::ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        crate::agents::bootstrap::ensure_default_routes(&store)
            .await
            .unwrap();
        crate::agents::bootstrap::initialize_agent_persistence(config_root, store)
            .await
            .unwrap()
    }

    async fn prompt_recovery_test_daemon(store: Arc<Store>) -> Daemon {
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            Registries::new(),
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;
        let router_in = Arc::new(Router::new(Arc::clone(&cp), vec![]));
        Daemon {
            cp,
            store: store.clone(),
            gateways: vec![],
            router_in,
            router_server: Arc::new(RouterServer::new(store)),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
            gateway_status_handles: Mutex::new(Vec::new()),
        }
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
        let store = Arc::new(Store::open(&path).await.unwrap());
        let persistence = test_agent_persistence(store.clone()).await;
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            Registries::new(),
            telemetry,
            persistence,
        )
        .await;
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
                primary_agent_id: Some("ryuzi".into()),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: "ryuzi".into(),
                    name: "Ryuzi".into(),
                    avatar_color: "blue".into(),
                }),
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
                archived_at: None,
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
        /// per-gateway error no longer wins the race outright. The failed
        /// surface is removed so another surface, including Cockpit, can
        /// resolve the request.
        ErrImmediately,
    }

    struct FakeGateway {
        gid: String,
        behavior: GwBehavior,
        calls: Arc<AtomicUsize>,
        starts: Arc<AtomicUsize>,
        status_posts: Arc<AtomicUsize>,
        status_posts_before_start: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        last_req: Arc<Mutex<Option<ApprovalRequest>>>,
        /// When non-zero, `start()` pauses before recording the gateway as
        /// started so a test can expose events emitted before gateway startup.
        start_delay: Duration,
        /// Test coordination for racing concurrent `Daemon::start()` calls.
        /// `start()` signals `entered` after recording its call, then waits for
        /// `release` before returning.
        start_block: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        fail_start: bool,
        status: Option<Arc<crate::gateway::GatewayStatusPublisher>>,
    }

    impl FakeGateway {
        fn new(gid: &str, behavior: GwBehavior) -> Self {
            FakeGateway {
                gid: gid.to_string(),
                behavior,
                calls: Arc::new(AtomicUsize::new(0)),
                starts: Arc::new(AtomicUsize::new(0)),
                status_posts: Arc::new(AtomicUsize::new(0)),
                status_posts_before_start: Arc::new(AtomicUsize::new(0)),
                stops: Arc::new(AtomicUsize::new(0)),
                last_req: Arc::new(Mutex::new(None)),
                start_delay: Duration::ZERO,
                start_block: None,
                fail_start: false,
                status: None,
            }
        }

        fn new_delayed_start(gid: &str, behavior: GwBehavior, start_delay: Duration) -> Self {
            FakeGateway {
                start_delay,
                ..FakeGateway::new(gid, behavior)
            }
        }

        fn new_failing_start(gid: &str) -> Self {
            FakeGateway {
                fail_start: true,
                ..FakeGateway::new(gid, GwBehavior::Allow)
            }
        }

        fn with_status_subscription(gid: &str) -> Self {
            let status = Arc::new(crate::gateway::GatewayStatusPublisher::new(
                GatewayStatus::Offline,
            ));
            FakeGateway {
                status: Some(status),
                ..FakeGateway::new(gid, GwBehavior::Allow)
            }
        }

        fn emit_status(&self, status: GatewayStatus) {
            self.status
                .as_ref()
                .expect("status-emitting fake gateway")
                .publish(status);
        }

        fn new_blocking_start(
            gid: &str,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        ) -> Self {
            FakeGateway {
                start_block: Some((entered, release)),
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
            tokio::time::sleep(self.start_delay).await;
            let start_number = self.starts.fetch_add(1, Ordering::SeqCst);
            if start_number == 0 {
                if let Some((entered, release)) = &self.start_block {
                    entered.notify_one();
                    release.notified().await;
                }
            }
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn subscribe_status(&self) -> Option<crate::gateway::GatewayStatusSubscription> {
            self.status.as_ref().map(|publisher| publisher.subscribe())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(&self, _w: &str, _t: &str) -> anyhow::Result<String> {
            Ok("conv".into())
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            self.status_posts.fetch_add(1, Ordering::SeqCst);
            if self.starts.load(Ordering::SeqCst) == 0 {
                self.status_posts_before_start
                    .fetch_add(1, Ordering::SeqCst);
            }
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
            Ok(Arc::new(FakeGateway::new("acme-gw", GwBehavior::Allow)))
        }
    }

    /// Stands in for a real native gateway factory that cannot build from the
    /// configuration it was given — e.g. a gateway with missing required
    /// credentials, which is what a machine with the gateway enabled but
    /// unconfigured has.
    struct UnconfiguredGatewayFactory;
    impl GatewayFactory for UnconfiguredGatewayFactory {
        fn create(&self, _config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            anyhow::bail!("gateway requires credentials that are not configured");
        }
    }

    struct StaticGatewayFactory {
        gateway: Arc<FakeGateway>,
    }

    impl GatewayFactory for StaticGatewayFactory {
        fn create(&self, _config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(self.gateway.clone())
        }
    }

    async fn wait_for_gateway_runs(store: &Store, hook_id: &str, expected: usize) {
        for _ in 0..100 {
            let runs = crate::automation::list_runs(store, hook_id).await.unwrap();
            if runs.len() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let runs = crate::automation::list_runs(store, hook_id).await.unwrap();
        assert_eq!(
            runs.len(),
            expected,
            "gateway status transition did not produce the expected number of runs"
        );
    }

    /// Fresh install: the default agents ship targeting the `free` route, and
    /// `build_daemon` is what creates that route (`ensure_default_routes`, off
    /// the seeded free connections). The agent registry caches each profile's
    /// validation into `AgentSnapshot { executable, validation }` ONCE, when it
    /// loads — so if it loads before the route exists, every default agent is
    /// permanently stamped "route `free` does not exist or is not executable"
    /// for the rest of the process, even though the route appears milliseconds
    /// later. The user sees a red "Invalid" agent on first launch that fixes
    /// itself only after a restart.
    #[tokio::test]
    async fn fresh_install_default_agents_are_executable_on_the_first_boot() {
        let (_guard, db_path) = temp_db_path();

        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();

        let snapshot = daemon.agents.snapshot().await;
        assert!(!snapshot.agents.is_empty(), "fresh install seeds an agent");
        for agent in &snapshot.agents {
            assert!(
                agent.validation.is_empty(),
                "agent `{}` invalid on first boot: {:?}",
                agent.profile.id,
                agent.validation
            );
            assert!(
                agent.executable,
                "agent `{}` not executable",
                agent.profile.id
            );
        }
    }

    /// An ENABLED gateway whose factory cannot build from the current settings
    /// (e.g. missing required credentials) is a configuration gap, not an
    /// engine fault: the daemon must still boot with the gateway skipped. When
    /// this failed the build instead, the engine daemon exited, and Cockpit's
    /// `setup()` panicked on `.expect("engine daemon unreachable")` before the
    /// window was ever shown.
    #[tokio::test]
    async fn build_daemon_skips_an_enabled_gateway_its_factory_cannot_build() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "acme-gw").await.unwrap();
        }

        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![(
                "acme-gw".to_string(),
                Arc::new(UnconfiguredGatewayFactory) as Arc<dyn GatewayFactory>,
            )],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .expect("the daemon must boot even though the enabled gateway is unconfigured");

        assert!(
            daemon.gateways.is_empty(),
            "an unbuildable gateway must be skipped, not wired"
        );
    }

    #[tokio::test]
    async fn build_daemon_wires_known_gateways_and_skips_unknown_ids() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings
                .set("enabled_gateways", "acme-gw,bogus")
                .await
                .unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let factory: Arc<dyn GatewayFactory> = Arc::new(CapturingGatewayFactory {
            captured: captured.clone(),
        });
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("acme-gw".to_string(), factory)],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();

        // Only the id with a registered factory is wired; the unknown id is skipped.
        assert_eq!(daemon.gateways.len(), 1);
        // A gateway with no CATALOG-declared settings fields receives an empty config.
        let cfg = captured.lock().unwrap().clone().unwrap();
        assert_eq!(cfg, serde_json::json!({}));
    }

    #[tokio::test]
    async fn daemon_forwards_gateway_status_subscription_without_start_stop_transitions() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "acme-gw").await.unwrap();
        }

        let gateway = Arc::new(FakeGateway::with_status_subscription("acme-gw"));
        let factory: Arc<dyn GatewayFactory> = Arc::new(StaticGatewayFactory {
            gateway: gateway.clone(),
        });
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("acme-gw".to_string(), factory)],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();
        seed_project(&daemon.store, "project-1").await;
        let hook = crate::automation::create_hook(
            &daemon.store,
            crate::automation::HookInput::agent_run(
                "gateway lifecycle",
                crate::automation::TriggerKind::GatewayStatusChanged,
                "project-1",
                "",
                "local",
                "ignore",
            ),
        )
        .await
        .unwrap();

        daemon.start().await.unwrap();
        assert!(crate::automation::list_runs(&daemon.store, &hook.id)
            .await
            .unwrap()
            .is_empty());

        gateway.emit_status(GatewayStatus::Connected);
        wait_for_gateway_runs(&daemon.store, &hook.id, 1).await;
        gateway.emit_status(GatewayStatus::Connected);
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            crate::automation::list_runs(&daemon.store, &hook.id)
                .await
                .unwrap()
                .len(),
            1
        );

        gateway.emit_status(GatewayStatus::Offline);
        wait_for_gateway_runs(&daemon.store, &hook.id, 2).await;
        daemon.stop().await;
        let runs = crate::automation::list_runs(&daemon.store, &hook.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 2);
        let mut transitions: Vec<_> = runs
            .iter()
            .map(|run| {
                (
                    run.envelope["data"]["previousStatus"].as_str().unwrap(),
                    run.envelope["data"]["status"].as_str().unwrap(),
                )
            })
            .collect();
        transitions.sort_unstable();
        assert_eq!(
            transitions,
            vec![("connected", "offline"), ("offline", "connected")]
        );
    }

    #[tokio::test]
    async fn daemon_records_gateway_transitions_while_outbound_delivery_is_slow() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "acme-gw").await.unwrap();
        }

        let gateway = Arc::new(FakeGateway::with_status_subscription("acme-gw"));
        let factory: Arc<dyn GatewayFactory> = Arc::new(StaticGatewayFactory {
            gateway: gateway.clone(),
        });
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("acme-gw".to_string(), factory)],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let hook = crate::automation::create_hook(
            &daemon.store,
            crate::automation::HookInput::outbound(
                "slow gateway hook",
                crate::automation::TriggerKind::GatewayStatusChanged,
                endpoint,
                None,
            ),
        )
        .await
        .unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            let _ = release_rx.await;
            // Drain the request before responding. Writing (and this fake
            // server dropping the socket at the end of scope) before the
            // client finishes sending its POST body races the client's
            // write with the server's close, which Windows surfaces as
            // `ConnectionAborted` (RST) rather than a clean response.
            drain_http_request(&mut stream).await.unwrap();
            // `Connection: close` on every response — otherwise `reqwest`'s
            // HTTP/1.1 keep-alive would try to reuse this same socket for the
            // next status delivery, but this fake server's loop below expects
            // a brand-new `accept()` per request. Without it, the second
            // `accept()` blocks forever waiting for a connection reqwest
            // never opens, wedging the daemon's serial delivery worker.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                drain_http_request(&mut stream).await.unwrap();
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        });

        daemon.start().await.unwrap();
        gateway.emit_status(GatewayStatus::Connected);
        tokio::time::timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("the first status delivery must reach the slow outbound hook")
            .unwrap();
        gateway.emit_status(GatewayStatus::Offline);
        gateway.emit_status(GatewayStatus::Connected);
        // The daemon-owned worker intentionally delivers serially. Releasing
        // the first slow request proves later transitions remain queued
        // rather than requiring detached, untracked delivery tasks.
        let _ = release_tx.send(());

        wait_for_gateway_runs(&daemon.store, &hook.id, 3).await;
        let runs = crate::automation::list_runs(&daemon.store, &hook.id)
            .await
            .unwrap();
        let transitions: Vec<_> = runs
            .iter()
            .map(|run| {
                (
                    run.envelope["data"]["previousStatus"].as_str().unwrap(),
                    run.envelope["data"]["status"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            transitions,
            vec![
                ("offline", "connected"),
                ("connected", "offline"),
                ("offline", "connected"),
            ]
        );

        daemon.stop().await;
    }

    #[tokio::test]
    async fn daemon_start_and_stop_do_not_wait_for_slow_gateway_hook_delivery() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "acme-gw").await.unwrap();
        }

        let gateway = Arc::new(FakeGateway::with_status_subscription("acme-gw"));
        let factory: Arc<dyn GatewayFactory> = Arc::new(StaticGatewayFactory {
            gateway: gateway.clone(),
        });
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("acme-gw".to_string(), factory)],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let hook = crate::automation::create_hook(
            &daemon.store,
            crate::automation::HookInput::outbound(
                "slow gateway hook",
                crate::automation::TriggerKind::GatewayStatusChanged,
                endpoint,
                None,
            ),
        )
        .await
        .unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = listener.accept().await;
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });

        tokio::time::timeout(Duration::from_millis(250), daemon.start())
            .await
            .expect("daemon start must not wait for an unobserved gateway transition")
            .unwrap();
        gateway.emit_status(GatewayStatus::Connected);
        tokio::time::timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("the status listener must dispatch the hook")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(250), daemon.stop())
            .await
            .expect("daemon stop must not wait for outbound hook delivery");
        assert_eq!(
            crate::automation::list_runs(&daemon.store, &hook.id)
                .await
                .unwrap()
                .len(),
            1
        );
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
            component_bootstrap: None,
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
            component_bootstrap: None,
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
    async fn handle_approval_without_gateway_surface_stays_pending_until_cockpit_resolves() {
        let (cp, store, _guard) = control_plane_with_telemetry(Arc::new(NoopTelemetry)).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        let approval = cp.approvals_for_test_register("run-cockpit-only", "req-cockpit-only");

        handle_approval(
            &cp,
            &store,
            &[],
            "s1",
            "run-cockpit-only",
            "agent-cockpit-only",
            "Agent Cockpit Only",
            "req-cockpit-only",
            "Bash",
            "ls",
            None,
        )
        .await;

        let mut approval = tokio::spawn(async move { approval.await.unwrap() });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut approval)
                .await
                .is_err(),
            "a Cockpit-only approval must remain pending without a gateway surface"
        );
        assert!(cp.resolve_approval_bool("run-cockpit-only", "req-cockpit-only", true));
        assert!(approval.await.unwrap().allowed());
    }

    #[tokio::test(start_paused = true)]
    async fn approval_fanout_keeps_question_requests_pending_until_cockpit_resolves() {
        let (cp, store, _guard) = control_plane_with_telemetry(Arc::new(NoopTelemetry)).await;
        let _fanout = spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), vec![]);
        let approval = cp.approvals_for_test_register("run-question", "req-question");

        cp.emit(CoreEvent::ApprovalRequested {
            session_pk: "s1".into(),
            run_id: "run-question".into(),
            requesting_agent_id: "agent-question".into(),
            requesting_agent_name: "Agent Question".into(),
            request_id: "req-question".into(),
            tool: "askuserquestion".into(),
            summary: "Choose one".into(),
            approval_kind: crate::domain::ApprovalKind::Question,
            input: serde_json::json!({}),
            principal: None,
        });

        let mut approval = tokio::spawn(async move { approval.await.unwrap() });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut approval)
                .await
                .is_err(),
            "a Question approval must not be auto-cancelled by the fan-out"
        );
        assert!(cp.resolve_approval_bool("run-question", "req-question", true));
        assert!(approval.await.unwrap().allowed());
    }

    #[tokio::test]
    async fn handle_approval_allow_resolves_true() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", Some("starter-1")).await;
        store.add_surface("acme-gw", "chan1", "s1").await.unwrap();
        SettingsStore::new(store.clone())
            .set("approver_role_ids", "role-a, role-b")
            .await
            .unwrap();

        let gw = FakeGateway::new("acme-gw", GwBehavior::Allow);
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
            "run-1",
            "agent-1",
            "Agent 1",
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
        assert_eq!(
            captured.timeout_ms,
            Some(300_000),
            "handle_approval must forward the resolved approval_timeout_ms default to the gateway"
        );
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
    async fn handle_approval_waits_for_a_slow_gateway_decision() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        store.add_surface("acme-gw", "chan1", "s1").await.unwrap();

        let gw = FakeGateway::new("acme-gw", GwBehavior::SleepThenAllow(150));
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];
        let approval = cp.approvals_for_test_register("run-2", "req-2");

        handle_approval(
            &cp, &store, &gateways, "s1", "run-2", "agent-2", "Agent 2", "req-2", "Bash", "sleep",
            None,
        )
        .await;

        assert!(approval.await.unwrap().allowed());
        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.allow"),
            "a gateway's explicit allow must resolve after an arbitrary delay, got: {parsed:?}"
        );
    }

    #[tokio::test]
    async fn handle_approval_without_gateway_surfaces_leaves_request_pending() {
        let (cp, store, _guard) = control_plane_with_telemetry(Arc::new(NoopTelemetry)).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        // Deliberately no add_surface.

        let gw = FakeGateway::new("acme-gw", GwBehavior::Allow);
        let calls = gw.calls.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];
        let approval = cp.approvals_for_test_register("run-3", "req-3");

        handle_approval(
            &cp, &store, &gateways, "s1", "run-3", "agent-3", "Agent 3", "req-3", "Bash", "ls",
            None,
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no surfaces means the gateway must never be asked"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), approval)
                .await
                .is_err(),
            "no gateway surfaces must leave the Cockpit approval pending"
        );
        assert!(cp.resolve_approval_bool("run-3", "req-3", false));
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
            "run-race-1",
            "agent-race",
            "Agent Race",
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
    async fn handle_approval_an_unavailable_gateway_leaves_request_pending_for_cockpit() {
        let (cp, store, _guard) = control_plane_with_telemetry(Arc::new(NoopTelemetry)).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        // The persisted gateway surface can become unavailable after session
        // setup (for example, the channel was deleted or permissions changed).
        store
            .add_surface("acme-gw", "deleted-channel", "s1")
            .await
            .unwrap();

        let gw = FakeGateway::new("acme-gw", GwBehavior::ErrImmediately);
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];
        let mut approval = cp.approvals_for_test_register("run-race-2", "req-race-2");

        handle_approval(
            &cp,
            &store,
            &gateways,
            "s1",
            "run-race-2",
            "agent-race",
            "Agent Race",
            "req-race-2",
            "Bash",
            "ls",
            None,
        )
        .await;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut approval)
                .await
                .is_err(),
            "an unavailable gateway surface must leave the approval pending for Cockpit"
        );
        assert!(
            cp.resolve_approval_bool("run-race-2", "req-race-2", true),
            "Cockpit must be able to resolve an approval after the gateway is unavailable"
        );
        assert!(
            approval.await.unwrap().allowed(),
            "the explicit Cockpit approval must reach the waiting tool"
        );
    }

    // ---------- Daemon::start partial-failure rollback (MUST-FIX 2) ----------

    #[tokio::test]
    async fn daemon_start_rolls_back_started_gateways_and_aborts_handles_on_later_failure() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let persistence = test_agent_persistence(store.clone()).await;
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            Registries::new(),
            Arc::new(NoopTelemetry),
            persistence.clone(),
        )
        .await;

        let gw_a = FakeGateway::new("gw-a", GwBehavior::Allow);
        let stops_a = gw_a.stops.clone();
        let gw_b = FakeGateway::new_failing_start("gw-b");
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw_a), Arc::new(gw_b)];

        // Long-running "loops" standing in for the real router/fan-out/
        // scheduler/rail/learning tasks, so this test can assert that a
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

        let daemon = Daemon {
            router_in: Arc::new(Router::new(Arc::clone(&cp), vec![])),
            cp,
            store: store.clone(),
            gateways,
            router_server: Arc::new(RouterServer::new(store)),
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            telemetry: Arc::new(NoopTelemetry),
            gateway_status_handles: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle,
            fanout_handle,
            scheduler_handle,
            rail_handle,
            learning_handle,
            artifact_retention_handle: tokio::spawn(async {}),
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
            daemon.rail_handle.is_finished(),
            "start()'s rollback must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "start()'s rollback must abort the learning loop"
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
            let run_id = "perm-primary-run".to_string();
            let requesting_agent_id = "ryuzi".to_string();
            let requesting_agent_name = "Ryuzi".to_string();
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
                run_id: run_id.clone(),
                requesting_agent_id,
                requesting_agent_name,
                request_id: request_id.clone(),
                tool: "Bash".into(),
                summary: "ls -la".into(),
                approval_kind: crate::domain::ApprovalKind::Tool,
                input: serde_json::json!({}),
                principal: None,
            });
            let rx = self
                .approvals
                .register(crate::approval::ApprovalKey::new(run_id, request_id));
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
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let mut regs = Registries::new();
        regs.harness = Arc::new(PermFakeHarnessFactory);
        let persistence = test_agent_persistence(store.clone()).await;
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            regs,
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        let gw = FakeGateway::new("acme-gw", GwBehavior::Allow);
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
                    run_id,
                    requesting_agent_id,
                    requesting_agent_name,
                    request_id,
                    tool,
                    summary,
                    ..
                })) => {
                    // Bind the surface now (we know the session_pk is live —
                    // the harness is already blocked awaiting the hub).
                    store
                        .add_surface("acme-gw", "chan1", &session_pk)
                        .await
                        .unwrap();
                    handle_approval(
                        &cp,
                        &store,
                        &gateways,
                        &session_pk,
                        &run_id,
                        &requesting_agent_id,
                        &requesting_agent_name,
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
            let run_id = "plan-primary-run".to_string();
            let requesting_agent_id = "ryuzi".to_string();
            let requesting_agent_name = "Ryuzi".to_string();
            let request_id = "plan-req-1".to_string();
            let rx = self.approvals.register_for_session(
                &self.session_pk,
                crate::approval::ApprovalKey::new(run_id.clone(), request_id.clone()),
            );
            let _ = self.events.send(CoreEvent::ApprovalRequested {
                session_pk: self.session_pk.clone(),
                run_id,
                requesting_agent_id,
                requesting_agent_name,
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
    async fn approval_fanout_keeps_plan_kind_requests_pending_until_cockpit_resolves() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        // Keep the test fast: the fan-out's own timeout — not any gateway or
        // `handle_approval` call — must be what resolves this.
        SettingsStore::new(store.clone())
            .set("approval_timeout_ms", "50")
            .await
            .unwrap();

        let mut regs = Registries::new();
        regs.harness = Arc::new(PlanFakeHarnessFactory);
        let persistence = test_agent_persistence(store.clone()).await;
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            regs,
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();
        let _fanout = spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), vec![]);

        let mut rx = cp.subscribe();
        let session = cp
            .start_session(&project.project_id, "go", "test", &[])
            .await
            .unwrap();

        let (run_id, request_id) = loop {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(CoreEvent::ApprovalRequested {
                    run_id, request_id, ..
                })) => break (run_id, request_id),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("approval event receiver failed: {error}"),
                Err(_) => panic!("the Plan approval was not requested"),
            }
        };

        tokio::time::sleep(Duration::from_millis(100)).await;
        let msgs = store.list_messages(&session.session_pk).await.unwrap();
        assert!(
            !msgs
                .iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "Cancel"),
            "the fan-out must not auto-cancel a Plan approval"
        );

        assert!(cp.resolve_approval_bool(&run_id, &request_id, true));
        for _ in 0..10 {
            let msgs = store.list_messages(&session.session_pk).await.unwrap();
            if msgs
                .iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "AllowOnce")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the Plan approval must resolve only after Cockpit responds");
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

    struct BootQueueFakeSession {
        prompts: Arc<Mutex<Vec<String>>>,
        sent: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl HarnessSession for BootQueueFakeSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            self.prompts.lock().unwrap().push(prompt.agent);
            self.sent.notify_one();
            self.release.notified().await;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("boot-queue-agent".into())
        }
    }
    struct BootQueueFakeHarness {
        prompts: Arc<Mutex<Vec<String>>>,
        starts: Arc<AtomicUsize>,
        sent: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl Harness for BootQueueFakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(BootQueueFakeSession {
                prompts: self.prompts.clone(),
                sent: self.sent.clone(),
                release: self.release.clone(),
            }))
        }
    }
    struct BootQueueFakeHarnessFactory {
        prompts: Arc<Mutex<Vec<String>>>,
        starts: Arc<AtomicUsize>,
        sent: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl HarnessFactory for BootQueueFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(BootQueueFakeHarness {
                prompts: self.prompts.clone(),
                starts: self.starts.clone(),
                sent: self.sent.clone(),
                release: self.release.clone(),
            }))
        }
    }

    #[tokio::test]
    #[serial]
    async fn daemon_start_delivers_one_pending_idle_queue_head_after_crash_window() {
        let _state_dir = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let starts = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut regs = Registries::new();
        regs.harness = Arc::new(BootQueueFakeHarnessFactory {
            prompts: prompts.clone(),
            starts: starts.clone(),
            sent: sent.clone(),
            release: release.clone(),
        });
        let store = Arc::new(store);
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            regs,
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;
        let store = cp.store().clone();
        let primary_agent = agents.resolved_snapshot("ryuzi").await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "idle-queued".into(),
                primary_agent_id: Some(primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: primary_agent.profile.id.clone(),
                    name: primary_agent.profile.name.clone(),
                    avatar_color: primary_agent.profile.avatar.color.clone(),
                }),
                project_id: None,
                agent_session_id: Some("interrupted-agent".into()),
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: Some("test".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
                archived_at: None,
            })
            .await
            .unwrap();
        // Simulate a process crash after the durable enqueue commit, before its
        // best-effort delivery kick can run.
        for (id, created_at) in [("first", 1), ("second", 2)] {
            store
                .enqueue_session_prompt(QueuedSessionPrompt {
                    id: id.into(),
                    session_pk: "idle-queued".into(),
                    agent: id.into(),
                    display: id.into(),
                    attachments: vec![],
                    created_at,
                })
                .await
                .unwrap();
        }

        let daemon = Daemon {
            router_in: Arc::new(Router::new(Arc::clone(&cp), vec![])),
            cp,
            store: store.clone(),
            gateways: vec![],
            router_server: Arc::new(RouterServer::new(store.clone())),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
            gateway_status_handles: Mutex::new(Vec::new()),
        };

        daemon.start().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), sent.notified())
            .await
            .expect("boot must start the pending idle queue head");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(prompts.lock().unwrap().as_slice(), ["first"]);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "reconcile must not resume the boot-delivered queue session"
        );
        assert!(
            !prompts
                .lock()
                .unwrap()
                .iter()
                .any(|prompt| prompt == crate::control::RESUME_NUDGE),
            "reconcile must not send a resume nudge to the boot-delivered queue session"
        );
        assert_eq!(
            store
                .list_session_prompt_queue("idle-queued")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["second"],
            "boot must claim only the FIFO head"
        );

        daemon.start().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a repeated start must not create another harness session"
        );
        assert_eq!(
            prompts.lock().unwrap().as_slice(),
            ["first"],
            "a repeated start must not send another prompt"
        );
        release.notify_waiters();
    }

    #[tokio::test]
    async fn daemon_start_recovers_abandoned_prompt_claims_once_under_concurrency() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        store
            .enqueue_session_prompt(QueuedSessionPrompt {
                id: "queued".into(),
                session_pk: "s1".into(),
                agent: "queued".into(),
                display: "queued".into(),
                attachments: vec![],
                created_at: 1,
            })
            .await
            .unwrap();
        assert_eq!(
            store
                .claim_next_session_prompt("s1")
                .await
                .unwrap()
                .unwrap()
                .id,
            "queued"
        );

        let daemon = Arc::new(prompt_recovery_test_daemon(store.clone()).await);
        let (recovery_entered, release_recovery) =
            store.pause_next_session_prompt_claim_recovery_for_test();
        let recovery_wait = recovery_entered.notified();
        tokio::pin!(recovery_wait);
        assert!(matches!(
            futures::poll!(recovery_wait.as_mut()),
            Poll::Pending
        ));
        let first_daemon = Arc::clone(&daemon);
        let first = tokio::spawn(async move { first_daemon.start().await });
        recovery_wait.await;

        let second_daemon = Arc::clone(&daemon);
        let mut second = tokio::spawn(async move { second_daemon.start().await });
        tokio::task::yield_now().await;
        assert!(matches!(futures::poll!(&mut second), Poll::Pending));
        release_recovery.notify_one();

        first.await.unwrap().unwrap();
        assert_eq!(
            store
                .claim_next_session_prompt("s1")
                .await
                .unwrap()
                .unwrap()
                .id,
            "queued"
        );
        second.await.unwrap().unwrap();
        assert!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .is_empty(),
            "the second concurrent boot recovery must not reset the live claim"
        );
    }

    #[tokio::test]
    async fn daemon_start_holds_inbound_reply_until_boot_recovery_finishes() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut regs = Registries::new();
        regs.harness = Arc::new(ResumeFakeHarnessFactory {
            prompts: prompts.clone(),
        });
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp =
            ControlPlane::new_with_telemetry(store, regs, Arc::new(NoopTelemetry), persistence)
                .await;
        let store = cp.store().clone();
        let primary_agent = agents.resolved_snapshot("ryuzi").await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "inbound-session".into(),
                primary_agent_id: Some(primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: primary_agent.profile.id.clone(),
                    name: primary_agent.profile.name.clone(),
                    avatar_color: primary_agent.profile.avatar.color.clone(),
                }),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: Some("test".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
                archived_at: None,
            })
            .await
            .unwrap();
        store
            .add_surface("acme-gw", "inbound-conversation", "inbound-session")
            .await
            .unwrap();

        let router_in = Arc::new(Router::new_boot_gated(Arc::clone(&cp), vec![]));
        let daemon = Arc::new(Daemon {
            cp,
            store: store.clone(),
            gateways: vec![],
            router_in: Arc::clone(&router_in),
            router_server: Arc::new(RouterServer::new(store.clone())),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
            gateway_status_handles: Mutex::new(Vec::new()),
        });

        let (recovery_entered, release_recovery) =
            store.pause_next_session_prompt_claim_recovery_for_test();
        let boot_daemon = Arc::clone(&daemon);
        let boot = tokio::spawn(async move { boot_daemon.start().await });
        recovery_entered.notified().await;

        let reply_router = Arc::clone(&router_in);
        let mut reply = tokio::spawn(async move {
            reply_router
                .on_reply(
                    "acme-gw",
                    "inbound-conversation",
                    "actor",
                    "inbound prompt",
                    &[],
                )
                .await
        });
        // Yield so the spawned task reaches the admission wait before its
        // JoinHandle is polled.
        tokio::task::yield_now().await;
        assert!(
            matches!(futures::poll!(&mut reply), Poll::Pending),
            "the inbound reply must remain pending while boot recovery is paused"
        );
        assert!(
            prompts.lock().unwrap().is_empty(),
            "the inbound reply must not start a harness before boot recovery completes"
        );

        release_recovery.notify_one();
        boot.await.unwrap().unwrap();
        reply.await.unwrap().unwrap();
        for _ in 0..100 {
            if prompts.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            prompts.lock().unwrap().as_slice(),
            ["inbound prompt"],
            "the admitted reply must start exactly one harness prompt"
        );
    }

    #[tokio::test]
    async fn daemon_start_rejects_waiting_inbound_work_and_rolls_back_after_boot_failure() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut regs = Registries::new();
        regs.harness = Arc::new(ResumeFakeHarnessFactory {
            prompts: prompts.clone(),
        });
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp =
            ControlPlane::new_with_telemetry(store, regs, Arc::new(NoopTelemetry), persistence)
                .await;
        let store = cp.store().clone();
        let primary_agent = agents.resolved_snapshot("ryuzi").await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "inbound-session".into(),
                primary_agent_id: Some(primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: primary_agent.profile.id.clone(),
                    name: primary_agent.profile.name.clone(),
                    avatar_color: primary_agent.profile.avatar.color.clone(),
                }),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: Some("test".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
                archived_at: None,
            })
            .await
            .unwrap();
        store
            .add_surface("acme-gw", "inbound-conversation", "inbound-session")
            .await
            .unwrap();

        let gateway = FakeGateway::new("acme-gw", GwBehavior::Allow);
        let starts = gateway.starts.clone();
        let stops = gateway.stops.clone();
        let gateway: Arc<dyn Gateway> = Arc::new(gateway);
        let router_in = Arc::new(Router::new_boot_gated(
            Arc::clone(&cp),
            vec![Arc::clone(&gateway)],
        ));
        let daemon = Arc::new(Daemon {
            cp,
            store: store.clone(),
            gateways: vec![gateway],
            router_in: Arc::clone(&router_in),
            router_server: Arc::new(RouterServer::new(store.clone())),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
            gateway_status_handles: Mutex::new(Vec::new()),
        });

        let (recovery_entered, release_recovery) =
            store.pause_next_session_prompt_claim_recovery_for_test();
        let boot_daemon = Arc::clone(&daemon);
        let boot = tokio::spawn(async move { boot_daemon.start().await });
        recovery_entered.notified().await;

        let reply_router = Arc::clone(&router_in);
        let mut reply = tokio::spawn(async move {
            reply_router
                .on_reply(
                    "acme-gw",
                    "inbound-conversation",
                    "actor",
                    "inbound prompt",
                    &[],
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            matches!(futures::poll!(&mut reply), Poll::Pending),
            "the inbound reply must remain pending while boot recovery is paused"
        );

        store
            .with_conn(|conn| conn.execute_batch("DROP TABLE session_prompt_queue"))
            .await
            .unwrap();
        release_recovery.notify_one();

        assert!(boot.await.unwrap().is_err(), "boot recovery must fail");
        let reply_error = reply
            .await
            .expect("inbound task must not panic")
            .expect_err("failed boot must reject the inbound reply");
        assert_eq!(reply_error.to_string(), "daemon failed to boot");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "the started gateway must be stopped during boot-failure rollback"
        );
        assert!(
            prompts.lock().unwrap().is_empty(),
            "the failed boot must not admit the inbound prompt"
        );
    }

    #[tokio::test]
    async fn daemon_start_serializes_concurrent_boot_failure() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp = ControlPlane::new_with_telemetry(
            store,
            Registries::new(),
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;
        let store = cp.store().clone();
        let (start_entered, release_start) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let gateway = FakeGateway::new_blocking_start(
            "acme-gw",
            Arc::clone(&start_entered),
            Arc::clone(&release_start),
        );
        let starts = Arc::clone(&gateway.starts);
        let stops = Arc::clone(&gateway.stops);
        let gateway: Arc<dyn Gateway> = Arc::new(gateway);
        let router_in = Arc::new(Router::new_boot_gated(
            Arc::clone(&cp),
            vec![Arc::clone(&gateway)],
        ));
        let daemon = Arc::new(Daemon {
            cp,
            store: store.clone(),
            gateways: vec![gateway],
            router_in: Arc::clone(&router_in),
            router_server: Arc::new(RouterServer::new(store.clone())),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle: tokio::spawn(async {}),
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
            gateway_status_handles: Mutex::new(Vec::new()),
        });

        let (recovery_entered, release_recovery) =
            store.pause_next_session_prompt_claim_recovery_for_test();
        let first_daemon = Arc::clone(&daemon);
        let first = tokio::spawn(async move { first_daemon.start().await });
        start_entered.notified().await;
        release_start.notify_waiters();
        recovery_entered.notified().await;

        let second_daemon = Arc::clone(&daemon);
        let mut second = tokio::spawn(async move { second_daemon.start().await });
        tokio::task::yield_now().await;
        assert!(matches!(futures::poll!(&mut second), Poll::Pending));
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "the second start must wait rather than start the gateway again"
        );

        store
            .with_conn(|conn| conn.execute_batch("DROP TABLE session_prompt_queue"))
            .await
            .unwrap();
        release_recovery.notify_one();
        let first_error = first.await.unwrap().unwrap_err();
        let second_error = second.await.unwrap().unwrap_err();
        assert!(first_error.to_string().contains("session_prompt_queue"));
        assert_eq!(second_error.to_string(), "daemon failed to boot");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(stops.load(Ordering::SeqCst), 1);

        let inbound_error = router_in
            .on_reply("acme-gw", "inbound-conversation", "actor", "prompt", &[])
            .await
            .unwrap_err();
        assert_eq!(inbound_error.to_string(), "daemon failed to boot");
    }

    #[tokio::test]
    async fn daemon_start_does_not_retry_after_prompt_claim_recovery_failure() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        store
            .enqueue_session_prompt(QueuedSessionPrompt {
                id: "queued".into(),
                session_pk: "s1".into(),
                agent: "queued".into(),
                display: "queued".into(),
                attachments: vec![],
                created_at: 1,
            })
            .await
            .unwrap();
        store.claim_next_session_prompt("s1").await.unwrap();

        let daemon = prompt_recovery_test_daemon(store.clone()).await;
        store.fail_next_session_prompt_claim_recovery_for_test();
        assert!(daemon.start().await.is_err());
        assert!(!daemon.prompt_claim_recovery_complete.load(Ordering::SeqCst));

        let retry_err = daemon.start().await.unwrap_err();
        assert_eq!(retry_err.to_string(), "daemon failed to boot");
    }

    #[tokio::test]
    async fn daemon_start_fires_reconcile_and_resumes_running_sessions() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut regs = Registries::new();
        regs.harness = Arc::new(ResumeFakeHarnessFactory {
            prompts: prompts.clone(),
        });
        let persistence = test_agent_persistence(store.clone()).await;
        let agents = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let learning_queue = persistence.learning.clone();
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            regs,
            Arc::new(NoopTelemetry),
            persistence,
        )
        .await;
        seed_project(&store, "p1").await;
        let primary_agent = agents.resolved_snapshot("ryuzi").await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                primary_agent_id: Some(primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: primary_agent.profile.id.clone(),
                    name: primary_agent.profile.name.clone(),
                    avatar_color: primary_agent.profile.avatar.color.clone(),
                }),
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
                archived_at: None,
            })
            .await
            .unwrap();

        store
            .add_surface("acme-gw", "resumed-session", "s1")
            .await
            .unwrap();

        let gw = FakeGateway::new_delayed_start(
            "acme-gw",
            GwBehavior::Allow,
            Duration::from_millis(100),
        );
        let gateway_starts = gw.starts.clone();
        let status_posts = gw.status_posts.clone();
        let status_posts_before_start = gw.status_posts_before_start.clone();
        let gw: Arc<dyn Gateway> = Arc::new(gw);
        let router_handle =
            tokio::spawn(Router::new(Arc::clone(&cp), vec![Arc::clone(&gw)]).run(cp.subscribe()));

        let daemon = Daemon {
            router_in: Arc::new(Router::new(Arc::clone(&cp), vec![Arc::clone(&gw)])),
            cp,
            store: store.clone(),
            gateways: vec![gw],
            router_server: Arc::new(RouterServer::new(store.clone())),
            agents,
            agent_knowledge,
            learning_queue,
            telemetry: Arc::new(NoopTelemetry),
            gateway_status_handles: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle,
            fanout_handle: tokio::spawn(async {}),
            scheduler_handle: tokio::spawn(async {}),
            rail_handle: tokio::spawn(async {}),
            learning_handle: tokio::spawn(async {}),
            artifact_retention_handle: tokio::spawn(async {}),
        };

        daemon.start().await.unwrap();

        for _ in 0..400 {
            if !prompts.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            gateway_starts.load(Ordering::SeqCst),
            1,
            "the gateway must start before boot reconciliation"
        );
        assert_eq!(
            status_posts.load(Ordering::SeqCst),
            1,
            "reconciliation must emit the resumed status to its bound surface"
        );
        assert_eq!(
            status_posts_before_start.load(Ordering::SeqCst),
            0,
            "the resumed status must not be routed before Gateway::start"
        );
        assert_eq!(
            prompts.lock().unwrap().as_slice(),
            [crate::control::RESUME_NUDGE],
            "Daemon::start must issue exactly one resume nudge for the recovered Running session"
        );
    }

    // ---------- (f) Daemon::stop is idempotent ----------

    #[tokio::test]
    async fn daemon_stop_is_idempotent_and_stops_each_gateway_once() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let persistence = test_agent_persistence(store.clone()).await;
        let cp = ControlPlane::new_with_telemetry(
            store.clone(),
            Registries::new(),
            Arc::new(NoopTelemetry),
            persistence.clone(),
        )
        .await;

        let gw = FakeGateway::new("acme-gw", GwBehavior::Allow);
        let stops = gw.stops.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        // Long-running "loops" standing in for the real router/fan-out/
        // scheduler/rail/learning tasks, so this test can assert
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

        let daemon = Daemon {
            router_in: Arc::new(Router::new(Arc::clone(&cp), vec![])),
            cp,
            store: store.clone(),
            gateways,
            router_server: Arc::new(RouterServer::new(store)),
            agents: persistence.registry,
            agent_knowledge: persistence.knowledge,
            learning_queue: persistence.learning,
            telemetry: Arc::new(NoopTelemetry),
            gateway_status_handles: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            lifecycle: AsyncMutex::new(()),
            started: AtomicBool::new(false),
            prompt_claim_recovery_complete: AtomicBool::new(false),
            router_handle,
            fanout_handle,
            scheduler_handle,
            rail_handle,
            learning_handle,
            artifact_retention_handle: tokio::spawn(async {}),
        };

        daemon.stop().await;
        daemon.stop().await;

        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "a second stop() must not re-invoke gateway.stop()"
        );
        // Give the abort a moment to actually land, then assert all tracked
        // loops are gone — stop() must not leave them running.
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
            daemon.rail_handle.is_finished(),
            "stop() must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "stop() must abort the learning loop"
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
            component_bootstrap: None,
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
            component_bootstrap: None,
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
        let config_root = tempfile::tempdir().unwrap();
        test_agent_persistence_at(
            Arc::new(Store::open(&db_path).await.unwrap()),
            config_root.path().to_path_buf(),
        )
        .await;
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings.set("enabled_gateways", "acme-gw").await.unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let factory: Arc<dyn GatewayFactory> = Arc::new(CapturingGatewayFactory {
            captured: captured.clone(),
        });

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: config_root.path().to_path_buf(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("acme-gw".to_string(), factory)],
            harness_factory: Some(Arc::new(PermFakeHarnessFactory)),
            component_bootstrap: None,
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
            .add_surface("acme-gw", "chan1", "s1")
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
        let config_root = tempfile::tempdir().unwrap();
        test_agent_persistence_at(
            Arc::new(Store::open(&db_path).await.unwrap()),
            config_root.path().to_path_buf(),
        )
        .await;

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            config_root: config_root.path().to_path_buf(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: Some(Arc::new(PermFakeHarnessFactory)),
            component_bootstrap: None,
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
            .add_surface("acme-gw", "chan1", "s1")
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

    // ---------- (j) daemon hosts scheduler + rail + learning loops (Tasks 10, 9, 8) ----------

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
            component_bootstrap: None,
        })
        .await
        .unwrap();
        let persistence = daemon.cp.agent_persistence();
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
            component_bootstrap: None,
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
    async fn daemon_hosts_and_stop_aborts_scheduler_rail_and_learning_loops() {
        let (_guard, db_path) = temp_db_path();
        let daemon = build_daemon(BuildDaemonOpts {
            db_path,
            config_root: tempfile::tempdir().unwrap().keep(),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![],
            harness_factory: None,
            component_bootstrap: None,
        })
        .await
        .unwrap();

        assert!(
            !daemon.scheduler_handle.is_finished(),
            "scheduler loop must be live"
        );
        assert!(!daemon.rail_handle.is_finished(), "rail loop must be live");
        assert!(
            !daemon.learning_handle.is_finished(),
            "learning loop must be live"
        );

        daemon.stop().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            daemon.scheduler_handle.is_finished(),
            "stop() must abort the scheduler loop"
        );
        assert!(
            daemon.rail_handle.is_finished(),
            "stop() must abort the rail loop"
        );
        assert!(
            daemon.learning_handle.is_finished(),
            "stop() must abort the learning loop"
        );
    }
}
