//! Supervise a long-lived WASM component that exports `ryuzi:gateway/gateway`.
//!
//! # Scope (greenfield lifecycle only)
//! This is NOT the full 11-method Rust [`crate::gateway::Gateway`] trait — that
//! reconciliation (mapping a component gateway onto session routing, surfaces,
//! approvals) is Task 14/Discord and out of scope here. [`WasmGatewaySupervisor`]
//! owns exactly the lifecycle of one enabled long-lived bundle: it `start`s the
//! component, then a background task periodically calls `poll-inbound` +
//! `health-check`, serves `deliver-outbound` on demand, exposes an observable
//! [`GatewaySnapshot`], restarts the component with CAPPED backoff after a trap,
//! and `stop`s it gracefully. Inbound events are surfaced as status only — they
//! are deliberately NOT wired into session routing yet (Task 14).
//!
//! # Isolation
//! Each supervised component owns its own epoch-isolated engine (see
//! [`crate::plugins::runtime::ComponentRuntime::compile`]), and every export
//! call runs under the fuel/epoch budget of
//! [`crate::plugins::runtime::ComponentInstance::call`]. So a trapping or
//! infinitely-looping `poll-inbound`/`health-check`/`deliver-outbound` is caught
//! as a `PluginRuntimeError` and turned into a bounded-backoff restart — never a
//! daemon crash, never a hung supervisor.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::plugins::capabilities::wit_bindings::exports::ryuzi::gateway::gateway as wit;
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::runtime::{CompiledComponent, ComponentInstance};
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::Telemetry;

/// Host-side gateway connection config handed to the component's `start`
/// (mirror of the WIT `gateway-config`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayConfig {
    pub account: String,
    pub endpoint: String,
}

/// Host-side inbound event pulled from the component (mirror of the WIT
/// `gateway-event`). Surfaced only as observable status for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayInboundEvent {
    pub event_type: String,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

/// Host-side outbound event handed to `deliver-outbound`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOutboundEvent {
    pub event_type: String,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

/// The result of a `deliver-outbound` call (mirror of the WIT
/// `gateway-delivery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayDelivery {
    pub accepted: bool,
    pub sequence: u64,
}

/// An observable snapshot of a supervised gateway. Cloned out of the shared
/// state on every [`WasmGatewaySupervisor::status`] read.
#[derive(Debug, Clone, Default)]
pub struct GatewaySnapshot {
    /// Whether the component last reported itself running (from `start`/
    /// `health-check`); `false` while restarting or after a graceful stop.
    pub running: bool,
    /// The most recent `health-check` detail string, if any.
    pub health: Option<String>,
    /// How many times the component has been restarted after a trap.
    pub restart_count: u32,
    /// The most recent trap/error reason, if any.
    pub last_error: Option<String>,
    /// Inbound events pulled via `poll-inbound`, capped to
    /// [`SupervisorTuning::max_inbound_buffer`] (most-recent-wins).
    pub inbound: Vec<GatewayInboundEvent>,
}

/// Timing knobs for a supervisor. Defaults are production-conservative; tests
/// override them to run fast and deterministically.
#[derive(Debug, Clone)]
pub struct SupervisorTuning {
    /// How often to `poll-inbound` + `health-check` while running.
    pub poll_interval: Duration,
    /// The first restart's backoff; doubles each consecutive rapid trap.
    pub base_backoff: Duration,
    /// The hard ceiling the doubling backoff is capped at.
    pub max_backoff: Duration,
    /// If the component served at least this long before trapping, the backoff
    /// schedule resets — so a gateway that was healthy for a while and trapped
    /// once restarts promptly, while a component that traps immediately on every
    /// restart climbs to (and stays at) `max_backoff`.
    pub reset_after: Duration,
    /// Maximum number of inbound events retained in the snapshot.
    pub max_inbound_buffer: usize,
}

impl Default for SupervisorTuning {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            reset_after: Duration::from_secs(60),
            max_inbound_buffer: 256,
        }
    }
}

/// Consecutive-restart count at which a supervised gateway's capped
/// exponential backoff has reached its ceiling under the DEFAULT tuning
/// (`base_backoff * 2^n >= max_backoff`): from here on it retries as slowly as
/// it ever will. The supervisor never permanently gives up — it keeps
/// restarting forever at the ceiling — so `plugin_doctor` defines
/// "restart-exhausted" as a gateway that is NOT running while already at (or
/// past) this steady-state slow-retry point, rather than a hard stop that does
/// not exist. (Default tuning: `500ms * 2^6 = 32s`, capped to the `30s`
/// ceiling; at `2^5 = 16s` it is still climbing — see the test below.)
pub const GATEWAY_BACKOFF_CEILING_RESTARTS: u32 = 6;

/// The capped exponential backoff for the `attempt`-th consecutive rapid trap
/// (0-based). Pure and unit-testable: `base * 2^attempt`, clamped to
/// `max_backoff`.
fn backoff_delay(tuning: &SupervisorTuning, attempt: u32) -> Duration {
    let base = tuning.base_backoff.as_millis() as u64;
    let cap = tuning.max_backoff.as_millis() as u64;
    let factor = 2u64.saturating_pow(attempt.min(16));
    Duration::from_millis(base.saturating_mul(factor).min(cap))
}

/// A command sent to the supervisor task from a [`WasmGatewaySupervisor`] handle.
enum Command {
    Deliver {
        event: GatewayOutboundEvent,
        reply: oneshot::Sender<Result<GatewayDelivery, String>>,
    },
    Stop {
        reply: oneshot::Sender<()>,
    },
}

/// A handle to one supervised long-lived gateway component. The actual work runs
/// in a background task; this struct is the control surface the daemon owns.
pub struct WasmGatewaySupervisor {
    plugin_id: String,
    commands: mpsc::Sender<Command>,
    status: Arc<Mutex<GatewaySnapshot>>,
    handle: JoinHandle<()>,
}

impl WasmGatewaySupervisor {
    /// Spawn a supervisor task for one enabled long-lived bundle. The task
    /// `start`s the component immediately and begins its serve/poll loop.
    pub fn spawn(
        plugin_id: String,
        compiled: Arc<CompiledComponent>,
        ctx: Arc<PluginCapabilityContext>,
        config: GatewayConfig,
        tuning: SupervisorTuning,
    ) -> Self {
        Self::spawn_with_inbound(plugin_id, compiled, ctx, config, tuning, None)
    }

    /// Like [`WasmGatewaySupervisor::spawn`], but every inbound event pulled via
    /// `poll-inbound` is ALSO forwarded to `inbound` (in addition to being
    /// recorded in the observable snapshot). The host gateway bridge
    /// ([`crate::plugins::wasm_gateway_bridge::WasmGateway`]) passes a sink here
    /// and drains it to resolve outbound-op / approval correlations. `None`
    /// preserves the plain lifecycle-only behaviour.
    pub fn spawn_with_inbound(
        plugin_id: String,
        compiled: Arc<CompiledComponent>,
        ctx: Arc<PluginCapabilityContext>,
        config: GatewayConfig,
        tuning: SupervisorTuning,
        inbound: Option<mpsc::UnboundedSender<GatewayInboundEvent>>,
    ) -> Self {
        let status = Arc::new(Mutex::new(GatewaySnapshot::default()));
        let (commands_tx, commands_rx) = mpsc::channel(16);
        let task_status = Arc::clone(&status);
        let task_id = plugin_id.clone();
        let handle = tokio::spawn(async move {
            supervise(
                task_id,
                compiled,
                ctx,
                config,
                tuning,
                commands_rx,
                task_status,
                inbound,
            )
            .await;
        });
        WasmGatewaySupervisor {
            plugin_id,
            commands: commands_tx,
            status,
            handle,
        }
    }

    /// The bundle id this supervisor drives.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// A cloned snapshot of the current observable status.
    pub fn status(&self) -> GatewaySnapshot {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A shared handle to the observable snapshot, so a bridge can watch status
    /// transitions (e.g. to publish a `GatewayStatus` subscription) without
    /// holding the supervisor itself.
    pub fn status_handle(&self) -> Arc<Mutex<GatewaySnapshot>> {
        Arc::clone(&self.status)
    }

    /// Hand the component an outbound event to deliver. An `Err` means the
    /// component rejected/failed it, or the supervisor is restarting/stopped —
    /// never a panic.
    pub async fn deliver_outbound(
        &self,
        event: GatewayOutboundEvent,
    ) -> Result<GatewayDelivery, String> {
        let (reply, reply_rx) = oneshot::channel();
        self.commands
            .send(Command::Deliver { event, reply })
            .await
            .map_err(|_| "gateway supervisor is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "gateway supervisor dropped the delivery".to_string())?
    }

    /// Graceful stop: ask the task to `stop()` the component and exit its loop,
    /// then abort the task to guarantee it is gone even if it was mid-restart.
    pub async fn stop(&self) {
        let (reply, reply_rx) = oneshot::channel();
        if self.commands.send(Command::Stop { reply }).await.is_ok() {
            let _ = reply_rx.await;
        }
        self.handle.abort();
    }

    /// Hard abort of the supervisor task (used by `Drop for Daemon`, mirroring
    /// `router_handle`/`fanout_handle`). Does not call the component's `stop`.
    pub fn abort(&self) {
        self.handle.abort();
    }
}

/// The supervisor task body: an outer restart loop around an inner serve loop.
// Internal task entry point wiring together the compiled component, its
// capability context, config/tuning, and the command/status/inbound channels —
// grouping them into a struct would only obscure a one-call-site function.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    plugin_id: String,
    compiled: Arc<CompiledComponent>,
    ctx: Arc<PluginCapabilityContext>,
    config: GatewayConfig,
    tuning: SupervisorTuning,
    mut commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<GatewaySnapshot>>,
    inbound: Option<mpsc::UnboundedSender<GatewayInboundEvent>>,
) {
    let mut restart_attempt: u32 = 0;
    loop {
        // (Re)instantiate a fresh, isolated instance and run the component's
        // `start`. A failure here is treated exactly like a serve-time trap:
        // record it and back off before retrying.
        let mut instance = match start_component(&compiled, &ctx, &config).await {
            Ok(instance) => {
                update_status(&status, |snapshot| {
                    snapshot.running = true;
                    snapshot.last_error = None;
                });
                instance
            }
            Err(reason) => {
                update_status(&status, |snapshot| {
                    snapshot.running = false;
                    snapshot.last_error = Some(reason.clone());
                });
                tracing::warn!(plugin = %plugin_id, "wasm gateway failed to start: {reason}");
                if backoff_or_stop(&mut commands, backoff_delay(&tuning, restart_attempt)).await {
                    return;
                }
                restart_attempt = restart_attempt.saturating_add(1);
                update_status(&status, |snapshot| snapshot.restart_count = restart_attempt);
                continue;
            }
        };

        match serve(&mut instance, &mut commands, &status, &tuning, &inbound).await {
            ServeOutcome::Stopped | ServeOutcome::ChannelClosed => return,
            ServeOutcome::Trapped { reason, uptime } => {
                update_status(&status, |snapshot| {
                    snapshot.running = false;
                    snapshot.last_error = Some(reason.clone());
                });
                tracing::warn!(plugin = %plugin_id, "wasm gateway trapped, restarting with backoff: {reason}");
                // A component that served healthily for a while before trapping
                // restarts promptly; one that traps immediately every time
                // climbs the capped backoff.
                if uptime >= tuning.reset_after {
                    restart_attempt = 0;
                }
                if backoff_or_stop(&mut commands, backoff_delay(&tuning, restart_attempt)).await {
                    return;
                }
                restart_attempt = restart_attempt.saturating_add(1);
                update_status(&status, |snapshot| snapshot.restart_count = restart_attempt);
            }
        }
    }
}

/// Why the inner serve loop returned.
enum ServeOutcome {
    /// A graceful `Stop` command was received; the component's `stop` was
    /// already called.
    Stopped,
    /// Every handle was dropped (command channel closed) — shut down.
    ChannelClosed,
    /// An export trapped/timed out; the outer loop restarts with backoff.
    Trapped { reason: String, uptime: Duration },
}

/// The inner serve loop: races the command channel against a periodic
/// poll/health tick. The first tick fires immediately, so a just-started
/// gateway surfaces its inbound events and health without waiting a full
/// interval.
async fn serve(
    instance: &mut ComponentInstance,
    commands: &mut mpsc::Receiver<Command>,
    status: &Arc<Mutex<GatewaySnapshot>>,
    tuning: &SupervisorTuning,
    inbound: &Option<mpsc::UnboundedSender<GatewayInboundEvent>>,
) -> ServeOutcome {
    let started_at = Instant::now();
    let mut poll = tokio::time::interval(tuning.poll_interval);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            // Prefer commands (stop/deliver) over the periodic tick so a stop is
            // observed promptly.
            biased;
            command = commands.recv() => match command {
                Some(Command::Stop { reply }) => {
                    let _ = call_stop(instance).await;
                    update_status(status, |snapshot| snapshot.running = false);
                    let _ = reply.send(());
                    return ServeOutcome::Stopped;
                }
                Some(Command::Deliver { event, reply }) => {
                    match call_deliver(instance, event).await {
                        Ok(delivery) => {
                            let _ = reply.send(Ok(delivery));
                            // Latency note (design §5.3): a delivered op's
                            // `op.result` (or an `approval.decision`) is queued by
                            // the component and only surfaced via `poll-inbound`.
                            // Do one immediate poll now so it reaches the bridge's
                            // `Correlation` without waiting a full poll interval.
                            match call_poll_inbound(instance).await {
                                Ok(events) => forward_inbound(status, tuning, inbound, events),
                                Err(reason) => {
                                    return ServeOutcome::Trapped {
                                        reason: format!("poll-inbound: {reason}"),
                                        uptime: started_at.elapsed(),
                                    };
                                }
                            }
                        }
                        Err(reason) => {
                            let _ = reply.send(Err(reason.clone()));
                            return ServeOutcome::Trapped {
                                reason: format!("deliver-outbound: {reason}"),
                                uptime: started_at.elapsed(),
                            };
                        }
                    }
                }
                None => return ServeOutcome::ChannelClosed,
            },
            _ = poll.tick() => {
                match call_poll_inbound(instance).await {
                    Ok(events) => forward_inbound(status, tuning, inbound, events),
                    Err(reason) => {
                        return ServeOutcome::Trapped {
                            reason: format!("poll-inbound: {reason}"),
                            uptime: started_at.elapsed(),
                        };
                    }
                }
                match call_health(instance).await {
                    Ok(state) => update_status(status, |snapshot| {
                        snapshot.running = state.running;
                        snapshot.health = Some(state.detail);
                    }),
                    Err(reason) => {
                        return ServeOutcome::Trapped {
                            reason: format!("health-check: {reason}"),
                            uptime: started_at.elapsed(),
                        };
                    }
                }
            }
        }
    }
}

/// Sleep for `delay`, but return `true` immediately if a `Stop` command (or a
/// closed channel) arrives — so a stop during backoff is prompt. `Deliver`
/// commands received while restarting are answered with an error (the component
/// isn't running) and waiting continues.
async fn backoff_or_stop(commands: &mut mpsc::Receiver<Command>, delay: Duration) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Stop { reply }) => {
                    let _ = reply.send(());
                    return true;
                }
                Some(Command::Deliver { reply, .. }) => {
                    let _ = reply.send(Err("gateway is restarting".to_string()));
                }
                None => return true,
            },
            _ = &mut sleep => return false,
        }
    }
}

fn update_status(status: &Arc<Mutex<GatewaySnapshot>>, mutate: impl FnOnce(&mut GatewaySnapshot)) {
    let mut guard = status.lock().unwrap_or_else(PoisonError::into_inner);
    mutate(&mut guard);
}

/// Forward freshly-polled inbound events to the optional bridge sink (so an
/// `op.result`/`approval.decision` reaches the bridge's `Correlation`) AND
/// record them in the observable snapshot. Cloning per-sink is cheap (small
/// events) and keeps the snapshot's status view intact for callers that only
/// read it.
fn forward_inbound(
    status: &Arc<Mutex<GatewaySnapshot>>,
    tuning: &SupervisorTuning,
    inbound: &Option<mpsc::UnboundedSender<GatewayInboundEvent>>,
    events: Vec<GatewayInboundEvent>,
) {
    if events.is_empty() {
        return;
    }
    if let Some(sink) = inbound {
        for event in &events {
            // A closed receiver (bridge dropped) just means nobody is
            // correlating anymore — the snapshot append below still runs.
            let _ = sink.send(event.clone());
        }
    }
    append_inbound(status, tuning, events);
}

fn append_inbound(
    status: &Arc<Mutex<GatewaySnapshot>>,
    tuning: &SupervisorTuning,
    events: Vec<GatewayInboundEvent>,
) {
    if events.is_empty() {
        return;
    }
    update_status(status, |snapshot| {
        snapshot.inbound.extend(events);
        // Keep only the most recent events so a chatty gateway can't grow the
        // snapshot without bound.
        if snapshot.inbound.len() > tuning.max_inbound_buffer {
            let excess = snapshot.inbound.len() - tuning.max_inbound_buffer;
            snapshot.inbound.drain(0..excess);
        }
    });
}

// ---- component export calls (each bounded by the fuel/epoch budget) ----

async fn start_component(
    compiled: &Arc<CompiledComponent>,
    ctx: &Arc<PluginCapabilityContext>,
    config: &GatewayConfig,
) -> Result<ComponentInstance, String> {
    let mut instance = compiled
        .instantiate(ctx.clone())
        .await
        .map_err(|error| error.to_string())?;
    let wit_config = wit::GatewayConfig {
        account: config.account.clone(),
        endpoint: config.endpoint.clone(),
    };
    call_start(&mut instance, wit_config).await?;
    Ok(instance)
}

async fn call_start(
    instance: &mut ComponentInstance,
    config: wit::GatewayConfig,
) -> Result<wit::GatewayState, String> {
    let result = instance
        .call(move |inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_start(&mut *store, &config)
        })
        .await
        .map_err(|error| error.to_string())?;
    result.map_err(|error| describe_gateway_error(&error))
}

async fn call_stop(instance: &mut ComponentInstance) -> Result<wit::GatewayState, String> {
    let result = instance
        .call(|inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_stop(&mut *store)
        })
        .await
        .map_err(|error| error.to_string())?;
    result.map_err(|error| describe_gateway_error(&error))
}

async fn call_deliver(
    instance: &mut ComponentInstance,
    event: GatewayOutboundEvent,
) -> Result<GatewayDelivery, String> {
    let wit_event = wit::GatewayEvent {
        event_type: event.event_type,
        payload: event.payload,
        sequence: event.sequence,
    };
    let result = instance
        .call(move |inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_deliver_outbound(&mut *store, &wit_event)
        })
        .await
        .map_err(|error| error.to_string())?;
    match result {
        Ok(delivery) => Ok(GatewayDelivery {
            accepted: delivery.accepted,
            sequence: delivery.sequence,
        }),
        Err(error) => Err(describe_gateway_error(&error)),
    }
}

async fn call_poll_inbound(
    instance: &mut ComponentInstance,
) -> Result<Vec<GatewayInboundEvent>, String> {
    let result = instance
        .call(|inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_poll_inbound(&mut *store)
        })
        .await
        .map_err(|error| error.to_string())?;
    match result {
        Ok(events) => Ok(events
            .into_iter()
            .map(|event| GatewayInboundEvent {
                event_type: event.event_type,
                payload: event.payload,
                sequence: event.sequence,
            })
            .collect()),
        Err(error) => Err(describe_gateway_error(&error)),
    }
}

async fn call_health(instance: &mut ComponentInstance) -> Result<wit::GatewayState, String> {
    let result = instance
        .call(|inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_health_check(&mut *store)
        })
        .await
        .map_err(|error| error.to_string())?;
    result.map_err(|error| describe_gateway_error(&error))
}

/// A human-readable, secret-free rendering of a WIT `gateway-error`.
fn describe_gateway_error(error: &wit::GatewayError) -> String {
    match error {
        wit::GatewayError::InvalidConfig(message) => format!("invalid gateway config: {message}"),
        wit::GatewayError::Disconnected => "gateway disconnected".to_string(),
        wit::GatewayError::Rejected => "gateway rejected the request".to_string(),
        wit::GatewayError::Failed(message) => format!("gateway failed: {message}"),
    }
}

/// One enabled, long-lived gateway component discovered off-disk and compiled,
/// but not yet supervised — the shared ingredients a [`WasmGatewaySupervisor`]
/// (or the host bridge's `WasmGateway`, Task 6) needs to `spawn`. Produced by
/// [`discover_gateway_components`].
pub(crate) struct GatewayComponent {
    pub id: String,
    pub compiled: Arc<CompiledComponent>,
    pub ctx: Arc<PluginCapabilityContext>,
    pub config: GatewayConfig,
}

/// Discover every active WASM component bundle under `root`, keep only the
/// ENABLED ones that export `ryuzi:gateway/gateway`, and compile each into the
/// ingredients a supervisor / `WasmGateway` needs — the daemon-owned analogue
/// of `control::lifecycle::build_wasm_session_providers`. Every failure mode is
/// warn-and-skip (missing root, discovery error, unavailable runtime, per-bundle
/// compile failure, enablement-lookup error), so a broken component plugin never
/// blocks daemon startup. Returns an empty vec when nothing enabled/long-lived
/// is installed, so the common case discovers nothing.
///
/// `root` is a parameter (rather than always
/// [`crate::plugins::bundle::installed_bundle_root`]) purely so the daemon-wiring
/// migration tests can point discovery at a hermetic install root; production
/// passes the real per-user root.
pub(crate) async fn discover_gateway_components(
    store: Arc<Store>,
    settings: &SettingsStore,
    telemetry: Arc<dyn Telemetry>,
    root: &std::path::Path,
) -> Vec<GatewayComponent> {
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};

    if !root.exists() {
        return Vec::new();
    }
    let bundles = match crate::plugins::bundle::load_active_bundles(root, &store).await {
        Ok(bundles) => bundles,
        Err(error) => {
            tracing::warn!("wasm gateway: discovering component bundles failed: {error}");
            return Vec::new();
        }
    };
    if bundles.is_empty() {
        return Vec::new();
    }
    let runtime = match ComponentRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!("wasm gateway: component runtime unavailable: {error}");
            return Vec::new();
        }
    };
    let mut components = Vec::new();
    for bundle in bundles {
        let id = bundle.manifest.id.clone();
        match crate::plugins::host::component_plugin_enabled(settings, &id).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                tracing::warn!(plugin = %id, "wasm gateway: enablement check failed: {error}");
                continue;
            }
        }
        // Single source of truth for the installed-bundle capability policy
        // (incl. the first-party-only `allow_self_auth` gate) — see
        // `HostPolicy::for_installed_bundle`.
        let policy = HostPolicy::for_installed_bundle(&bundle);
        let compiled = match runtime.compile(&bundle, policy) {
            Ok(compiled) => Arc::new(compiled),
            Err(error) => {
                tracing::warn!(plugin = %id, "wasm gateway: component compile failed: {error}");
                continue;
            }
        };
        // Only long-lived gateway bundles are supervised here; a connector/hooks/
        // provider-only bundle is skipped before any instantiation (IMP-2).
        if !compiled.exports_gateway() {
            continue;
        }
        let ctx = Arc::new(PluginCapabilityContext {
            plugin_id: id.clone(),
            version: bundle.manifest.version.clone(),
            settings: settings.clone(),
            store: store.clone(),
            telemetry: telemetry.clone(),
            network_allowlist: bundle
                .manifest
                .permissions
                .network
                .iter()
                .map(|entry| entry.0.clone())
                .collect(),
            oauth_profile_ids: bundle
                .manifest
                .oauth
                .iter()
                .map(|profile| profile.id.clone())
                .collect(),
            provider_ids: bundle.manifest.resolved_provider_ids(),
        });
        // The connection config the gateway `start`s with, read from the
        // plugin's own scoped settings (best-effort; empty when unset). Full
        // config wiring is Task 11/14.
        let config = GatewayConfig {
            account: settings
                .get(&format!("plugin.{id}.account"))
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            endpoint: settings
                .get(&format!("plugin.{id}.endpoint"))
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
        };
        components.push(GatewayComponent {
            id,
            compiled,
            ctx,
            config,
        });
    }
    components
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::bundle::InstalledBundle;
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{
        PluginBundleManifest, PluginLifecycle, PluginPermissions, PluginRelease,
    };
    use std::path::PathBuf;

    use crate::plugins::build_fixture_components_once as build_fixtures;

    fn gateway_artifact() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/component-gateway/target/wasm32-wasip2/release")
            .join("ryuzi_component_gateway_fixture.wasm")
    }

    async fn build_test_supervisor(
        config: GatewayConfig,
        tuning: SupervisorTuning,
        timeout: Duration,
    ) -> (WasmGatewaySupervisor, tempfile::NamedTempFile) {
        let mut policy = HostPolicy::deny_all();
        policy.limits.timeout = timeout;
        let component_path = gateway_artifact();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let ctx = Arc::new(PluginCapabilityContext {
            plugin_id: "acme-gateway".to_string(),
            version: "0.1.0".to_string(),
            settings: SettingsStore::new(store.clone()),
            store,
            telemetry: Arc::new(NoopTelemetry),
            network_allowlist: vec![],
            oauth_profile_ids: vec![],
            provider_ids: vec![],
        });
        let bundle = InstalledBundle {
            manifest: PluginBundleManifest {
                id: "acme-gateway".to_string(),
                name: "acme-gateway".to_string(),
                version: "0.1.0".to_string(),
                wit_api: "^0.1.0".to_string(),
                lifecycle: PluginLifecycle::Singleton,
                component: "plugin.wasm".to_string(),
                publisher: String::new(),
                description: String::new(),
                permissions: PluginPermissions { network: vec![] },
                oauth: vec![],
                provider_ids: vec![],
            },
            release: PluginRelease {
                id: "acme-gateway".to_string(),
                version: "0.1.0".to_string(),
                wit_api: "0.1.0".to_string(),
                component_url: "https://example.invalid/x.wasm".to_string(),
                component_sha256: "0".repeat(64),
                size_bytes: None,
                published_at: None,
            },
            release_record: ComponentPluginReleaseRecord {
                plugin_id: "acme-gateway".to_string(),
                version: "0.1.0".to_string(),
                source_url: "https://example.invalid/x.wasm".to_string(),
                sha256: "0".repeat(64),
                signing_key_id: "test".to_string(),
                installed_at: 0,
                active: true,
                revoked: false,
                revocation_reason: None,
            },
            root: component_path.parent().unwrap().to_path_buf(),
            component_path,
        };
        let runtime = ComponentRuntime::new().unwrap();
        let compiled = Arc::new(runtime.compile(&bundle, policy).unwrap());
        let supervisor =
            WasmGatewaySupervisor::spawn("acme-gateway".to_string(), compiled, ctx, config, tuning);
        (supervisor, tmp)
    }

    /// Poll `predicate` against the live snapshot up to `attempts` times with a
    /// short sleep between reads, returning the first snapshot that matches (or
    /// the last one seen).
    async fn wait_for(
        supervisor: &WasmGatewaySupervisor,
        attempts: usize,
        predicate: impl Fn(&GatewaySnapshot) -> bool,
    ) -> GatewaySnapshot {
        let mut snapshot = supervisor.status();
        for _ in 0..attempts {
            snapshot = supervisor.status();
            if predicate(&snapshot) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        snapshot
    }

    #[test]
    fn backoff_grows_then_caps() {
        let tuning = SupervisorTuning {
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(800),
            ..SupervisorTuning::default()
        };
        assert_eq!(backoff_delay(&tuning, 0), Duration::from_millis(100));
        assert_eq!(backoff_delay(&tuning, 1), Duration::from_millis(200));
        assert_eq!(backoff_delay(&tuning, 2), Duration::from_millis(400));
        assert_eq!(backoff_delay(&tuning, 3), Duration::from_millis(800));
        // Capped from here on — never exceeds max_backoff.
        assert_eq!(backoff_delay(&tuning, 4), Duration::from_millis(800));
        assert_eq!(backoff_delay(&tuning, 40), Duration::from_millis(800));
    }

    #[test]
    fn backoff_ceiling_restart_count_matches_the_default_tuning_cap() {
        // The doctor's `gateway-restart-exhausted` threshold must be exactly
        // the point where the DEFAULT tuning's backoff reaches its ceiling: one
        // restart earlier it is still climbing, and at the threshold it is
        // pinned at `max_backoff`.
        let tuning = SupervisorTuning::default();
        assert!(
            backoff_delay(&tuning, GATEWAY_BACKOFF_CEILING_RESTARTS - 1) < tuning.max_backoff,
            "one restart before the ceiling the backoff must still be climbing"
        );
        assert_eq!(
            backoff_delay(&tuning, GATEWAY_BACKOFF_CEILING_RESTARTS),
            tuning.max_backoff,
            "at the ceiling restart count the backoff must be pinned at max_backoff"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn healthy_gateway_surfaces_inbound_health_and_accepts_outbound() {
        build_fixtures();
        let tuning = SupervisorTuning {
            poll_interval: Duration::from_millis(20),
            ..SupervisorTuning::default()
        };
        let (supervisor, _tmp) = build_test_supervisor(
            GatewayConfig {
                account: "acme".to_string(),
                endpoint: "wss://example.invalid/gw".to_string(),
            },
            tuning,
            Duration::from_secs(5),
        )
        .await;

        // The first poll (fires immediately) emits one typed inbound message.
        let snapshot = wait_for(&supervisor, 100, |s| !s.inbound.is_empty()).await;
        assert!(
            snapshot.running,
            "a started gateway must report running: {snapshot:?}"
        );
        assert_eq!(
            snapshot.inbound.len(),
            1,
            "exactly one inbound event: {snapshot:?}"
        );
        assert_eq!(snapshot.inbound[0].event_type, "message");
        assert_eq!(snapshot.inbound[0].payload, b"hello from gateway");
        assert_eq!(snapshot.inbound[0].sequence, 1);

        // Health is surfaced (from the periodic health-check).
        let snapshot = wait_for(&supervisor, 100, |s| s.health.is_some()).await;
        assert!(
            snapshot
                .health
                .as_deref()
                .unwrap_or_default()
                .contains("healthy"),
            "health detail must surface: {snapshot:?}"
        );

        // Outbound delivery is accepted, echoing the sequence.
        let delivery = supervisor
            .deliver_outbound(GatewayOutboundEvent {
                event_type: "reply".to_string(),
                payload: b"pong".to_vec(),
                sequence: 42,
            })
            .await
            .expect("deliver-outbound must succeed");
        assert!(delivery.accepted);
        assert_eq!(delivery.sequence, 42);

        // Graceful stop marks the gateway not-running.
        supervisor.stop().await;
        assert!(
            !supervisor.status().running,
            "a stopped gateway must report not running"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn trapping_gateway_is_restarted_with_capped_backoff() {
        build_fixtures();
        let tuning = SupervisorTuning {
            poll_interval: Duration::from_millis(10),
            base_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(40),
            reset_after: Duration::from_secs(60),
            max_inbound_buffer: 256,
        };
        // A "boom" endpoint makes `poll-inbound` loop forever; the short per-call
        // timeout traps it, and the supervisor restarts with capped backoff.
        let (supervisor, _tmp) = build_test_supervisor(
            GatewayConfig {
                account: "acme".to_string(),
                endpoint: "boom".to_string(),
            },
            tuning,
            Duration::from_millis(150),
        )
        .await;

        // The trap reason (`last_error` naming `poll-inbound`) is recorded on
        // each trap but CLEARED on the next successful restart-start, so it and
        // a bumped `restart_count` need not both appear in the SAME snapshot.
        // Accumulate the two facts across the polling window rather than reading
        // a single snapshot (which races the restart that clears `last_error`).
        let mut saw_restart = false;
        let mut saw_trap_reason = false;
        let mut last = supervisor.status();
        for _ in 0..400 {
            let snapshot = supervisor.status();
            if snapshot.restart_count >= 1 {
                saw_restart = true;
            }
            if snapshot
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("poll-inbound")
            {
                saw_trap_reason = true;
            }
            last = snapshot;
            if saw_restart && saw_trap_reason {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            saw_restart,
            "a trapping gateway must be restarted after its trap: {last:?}"
        );
        assert!(
            saw_trap_reason,
            "the recorded trap reason must name the trapping export: {last:?}"
        );

        // The supervisor never hangs — stop tears it down promptly even mid-restart.
        supervisor.stop().await;
    }
}
