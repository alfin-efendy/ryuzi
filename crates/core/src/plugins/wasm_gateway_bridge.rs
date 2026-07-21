//! The typed event contract between the host and a `ryuzi:gateway` WASM
//! component, plus the request/response correlation map built on top of it.
//!
//! # Scope (Task 3 of the WASM websocket/Discord-gateway plan)
//! This module defines the shared vocabulary Tasks 4-6 (the host
//! gateway<->session bridge, design doc §5) and Tasks 7-10 (the `discord`
//! component) both encode/decode against. It is pure `serde` + `tokio`
//! `oneshot` — no `wasmtime`, no network, no Discord-specific branching.
//! [`InboundEvent`]/[`OutboundOp`] are GENERIC gateway-bridge types: any
//! `ryuzi:gateway` component (Discord is just the first producer) speaks
//! this same wire contract.
//!
//! The host<->component wire shape is the WIT `gateway-event` record
//! (`crates/plugin-sdk/wit/deps/gateway.wit`):
//! ```wit
//! record gateway-event {
//!   event-type: string,
//!   payload: list<u8>,
//!   sequence: u64,
//! }
//! ```
//! `event-type` and `payload` are what this module's [`InboundEvent::decode`]/
//! [`InboundEvent::encode`] and [`OutboundOp::decode`]/[`OutboundOp::encode`]
//! translate to/from; `sequence` is a bridge-level (Task 4-6) concern (replay
//! dedup) and plays no part in the event's own identity here.
//!
//! # Wire encoding
//! Each enum is internally tagged on `event_type` (`#[serde(tag =
//! "event_type")]`), and every other field of the active variant is
//! serialized flat alongside that tag in the SAME JSON object — so
//! `encode()` pulls `event_type` back out into the WIT record's separate
//! `event-type` string field, leaving the remaining object as `payload`
//! bytes, and `decode()` does the inverse (re-inserting `event-type` as the
//! `event_type` JSON key before deserializing). This keeps the wire
//! `payload` shape exactly flat, matching design doc §5.2/§5.3's event
//! tables (e.g. `message.mention`'s payload is
//! `{workspace_id, actor, prompt, attachments}`, with no extra nesting).
//!
//! Field names ARE the wire contract (documented per-variant below) — they
//! are picked to match design doc §5.2 (inbound) and §5.3 (outbound)
//! exactly, since the `discord` component (Tasks 7-10) must produce/consume
//! the identical shape.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::domain::{ApprovalDecision, ApprovalRequest, AttachmentRef, Surface};
use crate::gateway::{
    Gateway, GatewayStatus, GatewayStatusPublisher, GatewayStatusSubscription, MessageRef,
};
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::runtime::CompiledComponent;
use crate::plugins::wasm_gateway::{
    GatewayConfig, GatewayInboundEvent, GatewayOutboundEvent, GatewaySnapshot, SupervisorTuning,
    WasmGatewaySupervisor,
};
use crate::router::Router;

// ---------------------------------------------------------------------
// Inbound events (component -> host, delivered via `poll-inbound`)
// ---------------------------------------------------------------------

/// An event the component surfaces to the host through `poll-inbound`.
/// Tagged by its wire `event_type` string (design doc §5.2's first column);
/// see each variant's doc for its exact payload fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum InboundEvent {
    /// `message.mention` — a channel message that @-mentioned the bot.
    #[serde(rename = "message.mention")]
    MessageMention {
        workspace_id: String,
        actor: String,
        prompt: String,
        attachments: Vec<String>,
    },
    /// `message.thread` — a reply inside a thread the bot owns.
    #[serde(rename = "message.thread")]
    MessageThread {
        conversation_id: String,
        actor: String,
        prompt: String,
        attachments: Vec<String>,
    },
    /// `message.dm` — a direct message to the bot.
    #[serde(rename = "message.dm")]
    MessageDm {
        conversation_id: String,
        user_id: String,
        text: String,
    },
    /// `slash.connect` — the `/connect` slash command.
    #[serde(rename = "slash.connect")]
    SlashConnect {
        token: String,
        user_id: String,
        opts: ConnectOptsWire,
        role_ids: Vec<String>,
    },
    /// `slash.end` — the `/end` slash command.
    #[serde(rename = "slash.end")]
    SlashEnd {
        token: String,
        conversation_id: String,
    },
    /// `slash.stop` — the `/stop` slash command.
    #[serde(rename = "slash.stop")]
    SlashStop {
        token: String,
        conversation_id: String,
    },
    /// `slash.status` — the `/status` slash command (pure reply, no session op).
    #[serde(rename = "slash.status")]
    SlashStatus { token: String },
    /// `approval.decision` — an approval button click, resolving a pending
    /// `approval-request` (design doc §5.4).
    #[serde(rename = "approval.decision")]
    ApprovalDecision {
        request_id: String,
        allow: bool,
        actor: String,
    },
    /// `op.result` — the outcome of a previously sent outbound op,
    /// correlated by `op_id` (design doc §5.3). `result`'s fields are
    /// flattened onto the wire alongside `op_id` (no nested `result` key).
    #[serde(rename = "op.result")]
    OpResult {
        op_id: String,
        #[serde(flatten)]
        result: OpResultBody,
    },
}

/// The `opts` object of a `slash.connect` inbound event — wire mirror of
/// [`crate::router::ConnectOpts`], but decoupled from that (and from
/// `crate::control::ProvisionSettings`/`PermMode`) domain types: this module
/// only defines the wire shape, the bridge (Tasks 4-6) is responsible for
/// converting it. Every field is optional, exactly like the native
/// `/connect` command allows a bare `/connect` with no arguments. `git`/
/// `mode` are deliberately short names (not `git_url`/`perm_mode`) — they
/// mirror the Discord slash-command option names the component reads
/// (`gateway/discord/mod.rs`'s `/connect` options: `name`, `git`, `model`,
/// `effort`, `mode`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectOptsWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// The correlated result of one outbound op (design doc §5.3's "expected
/// `op-result`" column), flattened into an `op.result` [`InboundEvent`] and
/// into [`CorrelationValue::OpResult`]. Every field is optional because each
/// outbound op kind only ever populates the ones relevant to it:
/// `create-channel` -> `channel_id`, `create-thread` -> `thread_id`,
/// `send-message`/`post_error` -> `message_id`, `edit-message`/
/// `send-messages` -> `ok`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResultBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

// ---------------------------------------------------------------------
// Outbound ops (host -> component, sent via `deliver-outbound`)
// ---------------------------------------------------------------------

/// An op the host sends to the component through `deliver-outbound`.
/// Tagged by its wire `event_type` string (design doc §5.3's second
/// column). Every op that expects a value back carries an `op_id` the
/// matching `op.result` inbound event echoes (§5.3); `approval-request` also
/// carries an `op_id` for wire uniformity, but is correlated by its
/// `request_id` instead — it resolves via a later `approval.decision`
/// inbound event, never an `op.result` (design doc §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum OutboundOp {
    /// `create-channel` — `Gateway::create_workspace`.
    #[serde(rename = "create-channel")]
    CreateChannel { op_id: String, name: String },
    /// `create-thread` — `Gateway::create_conversation`.
    #[serde(rename = "create-thread")]
    CreateThread {
        op_id: String,
        channel_id: String,
        title: String,
    },
    /// `send-message` — `Gateway::post_status`/`post_error`.
    #[serde(rename = "send-message")]
    SendMessage {
        op_id: String,
        channel_id: String,
        text: String,
    },
    /// `edit-message` — `Gateway::edit_status`.
    #[serde(rename = "edit-message")]
    EditMessage {
        op_id: String,
        channel_id: String,
        message_id: String,
        text: String,
    },
    /// `send-messages` — `Gateway::post_result`.
    #[serde(rename = "send-messages")]
    SendMessages {
        op_id: String,
        channel_id: String,
        chunks: Vec<String>,
    },
    /// `approval-request` — `Gateway::request_approval`.
    #[serde(rename = "approval-request")]
    ApprovalRequest {
        op_id: String,
        request_id: String,
        conversation_id: String,
        tool: String,
        summary: String,
        approver_role_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// `interaction-reply` — the bridge's computed reply to a deferred
    /// `slash.*` interaction (design doc §5.4).
    #[serde(rename = "interaction-reply")]
    InteractionReply { token: String, text: String },
}

// ---------------------------------------------------------------------
// Encode/decode against the WIT `gateway-event {event-type, payload}` shape
// ---------------------------------------------------------------------

/// Serialize a `#[serde(tag = "event_type")]` enum to the WIT
/// `gateway-event` wire shape: `event-type` pulled out as its own string,
/// everything else left as flat JSON `payload` bytes.
fn encode_tagged<T: Serialize>(value: &T) -> (String, Vec<u8>) {
    let mut json =
        serde_json::to_value(value).expect("InboundEvent/OutboundOp always serialize to JSON");
    let obj = json
        .as_object_mut()
        .expect("an internally-tagged enum always serializes to a JSON object");
    let event_type = obj
        .remove("event_type")
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("every InboundEvent/OutboundOp variant carries an event_type tag");
    let payload = serde_json::to_vec(&json).expect("a JSON object always re-serializes to bytes");
    (event_type, payload)
}

/// Inverse of [`encode_tagged`]: re-insert `event_type` into the flat
/// `payload` JSON object, then deserialize the whole tagged enum from it.
fn decode_tagged<T: DeserializeOwned>(event_type: &str, payload: &[u8]) -> Result<T> {
    let mut json: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(payload)
            .with_context(|| format!("gateway event {event_type:?} payload is not valid JSON"))?
    };
    let Some(obj) = json.as_object_mut() else {
        bail!("gateway event {event_type:?} payload must be a JSON object");
    };
    obj.insert(
        "event_type".to_string(),
        serde_json::Value::String(event_type.to_string()),
    );
    serde_json::from_value(json)
        .with_context(|| format!("gateway event {event_type:?} payload does not match its shape"))
}

impl InboundEvent {
    /// Decode a `poll-inbound` `gateway-event`'s `event-type` + `payload`
    /// into a typed [`InboundEvent`]. Errors on an unrecognized `event_type`
    /// or a payload that doesn't match that variant's fields.
    pub fn decode(event_type: &str, payload: &[u8]) -> Result<Self> {
        decode_tagged(event_type, payload)
    }

    /// Encode this event back to the WIT `gateway-event`'s `(event-type,
    /// payload)` pair. Only used by tests here (production inbound flow is
    /// decode-only); Tasks 4-6's fixture-gateway tests reuse it to construct
    /// synthetic `poll-inbound` responses.
    pub fn encode(&self) -> (String, Vec<u8>) {
        encode_tagged(self)
    }
}

impl OutboundOp {
    /// Encode this op to the `(event-type, payload)` pair sent via
    /// `deliver-outbound`.
    pub fn encode(&self) -> (String, Vec<u8>) {
        encode_tagged(self)
    }

    /// Decode a `deliver-outbound` `gateway-event`'s `event-type` +
    /// `payload` back into a typed [`OutboundOp`]. Only used by tests here
    /// (production outbound flow is encode-only); the `discord` component
    /// (Tasks 7-10) has its own guest-side decode.
    pub fn decode(event_type: &str, payload: &[u8]) -> Result<Self> {
        decode_tagged(event_type, payload)
    }
}

// ---------------------------------------------------------------------
// Correlation: op_id / request_id -> pending oneshot waiter
// ---------------------------------------------------------------------

/// The two id spaces a [`Correlation`] multiplexes: an outbound op's
/// `op_id` (`op.result` correlation, design doc §5.3) and an
/// `approval-request`'s `request_id` (`approval.decision` correlation,
/// design doc §5.4). Kept as one enum — rather than two separate maps or a
/// bare `String` key — so a call site's intent ("which space is this id
/// in?") is explicit and the two spaces can never collide even if a
/// component ever reused a string across them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CorrelationKey {
    /// An outbound op's `op_id`.
    Op(String),
    /// An `approval-request`'s `request_id`.
    Approval(String),
}

/// What resolving a [`CorrelationKey`] delivers to its waiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrelationValue {
    /// An `op.result` inbound event's body, for a [`CorrelationKey::Op`] wait.
    OpResult(OpResultBody),
    /// An `approval.decision` inbound event's decision, for a
    /// [`CorrelationKey::Approval`] wait.
    Approval { allow: bool, actor: String },
}

/// What awaiting a [`Correlation::register`] future produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrelationOutcome {
    /// [`Correlation::resolve`] delivered a value before the timeout elapsed.
    Resolved(CorrelationValue),
    /// No matching `resolve` arrived within the registered timeout (or the
    /// `Correlation` was dropped first); the key has already been removed
    /// from the map — no leak.
    TimedOut,
}

/// Pending op-result / approval-decision waiters shared between whatever
/// sends an outbound op or approval request (the Task 4-6 bridge) and the
/// `poll-inbound` loop that later resolves it. Cheaply [`Clone`] (an `Arc`
/// handle to the same map), so one instance can be held by both the bridge
/// and its background poll task.
#[derive(Debug, Clone, Default)]
pub struct Correlation {
    pending: Arc<Mutex<HashMap<CorrelationKey, oneshot::Sender<CorrelationValue>>>>,
}

impl Correlation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `key` and return a future that resolves once
    /// [`Correlation::resolve`] is called for it, or `timeout` elapses
    /// first. The map insert happens synchronously, before this returns —
    /// so a `resolve` racing in on another task can never be missed just
    /// because the caller hasn't polled the returned future yet (an `async
    /// fn` body wouldn't run the insert until first polled; this is a plain
    /// `fn` for exactly that reason).
    ///
    /// On timeout (or if the sender is dropped without sending — e.g. this
    /// `Correlation` itself being torn down), `key` is removed from the map
    /// before the future resolves, so an unresolved registration never
    /// leaks.
    pub fn register(
        &self,
        key: CorrelationKey,
        timeout: Duration,
    ) -> impl Future<Output = CorrelationOutcome> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(key.clone(), tx);
        let pending = Arc::clone(&self.pending);
        async move {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(value)) => CorrelationOutcome::Resolved(value),
                Ok(Err(_)) | Err(_) => {
                    pending.lock().unwrap().remove(&key);
                    CorrelationOutcome::TimedOut
                }
            }
        }
    }

    /// Deliver `value` to the waiter registered under `key`, if any (removing
    /// it from the map either way — a spurious/duplicate resolve is a no-op).
    /// Returns `false` if `key` had no pending registration (already
    /// resolved, already timed out, or never registered) — the caller (the
    /// `poll-inbound` loop) treats that as "drop the event", never a panic.
    pub fn resolve(&self, key: &CorrelationKey, value: CorrelationValue) -> bool {
        match self.pending.lock().unwrap().remove(key) {
            Some(tx) => tx.send(value).is_ok(),
            None => false,
        }
    }

    /// Remove a pending registration without delivering a value — e.g. the
    /// outbound `deliver-outbound` failed, so no `op.result`/`approval.decision`
    /// will ever arrive for it. Dropping the stored sender makes the matching
    /// [`Correlation::register`] future resolve PROMPTLY to
    /// [`CorrelationOutcome::TimedOut`] on its next poll (rather than blocking
    /// for the full timeout), so the caller can still await it — never dropping
    /// a registered future early — without hanging. Returns whether an entry
    /// was removed.
    pub fn cancel(&self, key: &CorrelationKey) -> bool {
        self.pending.lock().unwrap().remove(key).is_some()
    }

    /// Number of currently-pending registrations. Test/introspection only —
    /// production code should never need to poll this instead of awaiting
    /// [`Correlation::register`]'s returned future.
    pub fn len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------
// WasmGateway: `impl Gateway` backed by the supervisor (design doc §5.1/§5.3)
// ---------------------------------------------------------------------

/// How long an outbound op (`create-channel`/`send-message`/…) waits for its
/// `op.result` before giving up. Generous: the supervisor's immediate
/// post-`deliver-outbound` poll normally resolves it within a tick, so this
/// only bounds a wedged/absent component.
const OP_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The approval wait when a request carries no explicit `timeout_ms`. Matches
/// the "no deadline configured" fallback; a request with a `timeout_ms` uses
/// exactly that value and auto-rejects when it elapses.
const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the status-watch task samples the supervisor snapshot to publish
/// `Connected`/`Offline` transitions.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A host-side [`crate::gateway::Gateway`] backed by a
/// [`WasmGatewaySupervisor`]. Each outbound trait method becomes a typed
/// `deliver-outbound` event correlated to its `op.result` (or, for approvals,
/// to a later `approval.decision`) through the shared [`Correlation`]. This
/// lets the daemon's outbound `Router` and approval fan-out drive a WASM
/// gateway component through the same trait they use for native gateways.
///
/// Generic by construction: nothing here branches on `"discord"` or any plugin
/// id — the behaviour is entirely determined by the component the supervisor
/// drives (design doc §2/§5.1).
pub struct WasmGateway {
    id: String,
    supervisor: WasmGatewaySupervisor,
    /// The outbound `Router`, installed by `set_router`. Consumed by the drain
    /// task to dispatch `message.*` inbound events (Task 5); `slash.*` events
    /// are decoded but not yet acted on (Task 6).
    router: Arc<OnceLock<Arc<Router>>>,
    correlation: Arc<Correlation>,
    status: Arc<GatewayStatusPublisher>,
    next_op: AtomicU64,
    /// Background tasks (inbound drain + status watch) aborted on drop.
    tasks: Vec<JoinHandle<()>>,
}

impl WasmGateway {
    /// Build a `WasmGateway` over a fresh supervisor for `plugin_id`'s
    /// component. Spawns the supervisor (with an inbound sink), the inbound
    /// drain task that resolves correlations, and the status-watch task that
    /// publishes connection transitions.
    pub fn new(
        plugin_id: String,
        compiled: Arc<CompiledComponent>,
        ctx: Arc<PluginCapabilityContext>,
        config: GatewayConfig,
        tuning: SupervisorTuning,
    ) -> Self {
        let correlation = Arc::new(Correlation::new());
        let router: Arc<OnceLock<Arc<Router>>> = Arc::new(OnceLock::new());
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let supervisor = WasmGatewaySupervisor::spawn_with_inbound(
            plugin_id.clone(),
            compiled,
            ctx,
            config,
            tuning,
            Some(inbound_tx),
        );

        let status = Arc::new(GatewayStatusPublisher::new(status_of(&supervisor.status())));

        let drain = tokio::spawn(drain_inbound(
            inbound_rx,
            Arc::clone(&correlation),
            Arc::clone(&router),
            plugin_id.clone(),
        ));
        let watch = tokio::spawn(watch_status(
            supervisor.status_handle(),
            Arc::clone(&status),
        ));

        WasmGateway {
            id: plugin_id,
            supervisor,
            router,
            correlation,
            status,
            next_op: AtomicU64::new(0),
            tasks: vec![drain, watch],
        }
    }

    /// A fresh, per-gateway-unique outbound op id.
    fn next_op_id(&self) -> String {
        format!("op-{}", self.next_op.fetch_add(1, Ordering::Relaxed))
    }

    /// Encode and hand `op` to the component. `Ok(())` means the component
    /// accepted the delivery (its `op.result`/`approval.decision`, if any, then
    /// arrives via `poll-inbound`); `Err` means the supervisor is
    /// restarting/stopped or the component rejected it.
    async fn deliver(&self, op: OutboundOp) -> std::result::Result<(), String> {
        let (event_type, payload) = op.encode();
        let delivery = self
            .supervisor
            .deliver_outbound(GatewayOutboundEvent {
                event_type,
                payload,
                // `sequence` is a Task-5 inbound-dedup concern; the component
                // merely echoes an outbound op's sequence, so 0 is fine here.
                sequence: 0,
            })
            .await?;
        if !delivery.accepted {
            return Err("component rejected the outbound op".to_string());
        }
        Ok(())
    }

    /// Register an `op_id` correlation, deliver `op`, and await the matching
    /// `op.result` body. Always awaits the registered future (T3 carry-forward):
    /// on a delivery failure it `cancel`s the registration first so the await
    /// returns promptly instead of blocking for the full timeout.
    async fn run_op(&self, op_id: String, op: OutboundOp) -> Result<OpResultBody> {
        let key = CorrelationKey::Op(op_id);
        let waiter = self.correlation.register(key.clone(), OP_RESULT_TIMEOUT);
        if let Err(reason) = self.deliver(op).await {
            self.correlation.cancel(&key);
            let _ = waiter.await;
            bail!("wasm gateway delivery failed: {reason}");
        }
        match waiter.await {
            CorrelationOutcome::Resolved(CorrelationValue::OpResult(body)) => Ok(body),
            CorrelationOutcome::Resolved(other) => {
                bail!("wasm gateway op {key:?} resolved with a non-op-result value: {other:?}")
            }
            CorrelationOutcome::TimedOut => {
                bail!("wasm gateway op {key:?} timed out waiting for its op.result")
            }
        }
    }
}

/// Map a supervisor snapshot's `running` flag to the coarse `GatewayStatus` the
/// `Gateway::subscribe_status` contract exposes.
fn status_of(snapshot: &GatewaySnapshot) -> GatewayStatus {
    if snapshot.running {
        GatewayStatus::Connected
    } else {
        GatewayStatus::Offline
    }
}

/// Drain inbound events forwarded by the supervisor: correlate `op.result`/
/// `approval.decision` (Task 4) and dispatch `message.*` into the stored
/// `Router` (Task 5), reproducing `gateway/discord/mod.rs`'s native routing
/// rules exactly (design doc §5.2) — `message.mention` -> `on_start`,
/// `message.thread` -> `on_reply`, `message.dm` -> `on_dm`. `slash.*` events
/// are decoded but not yet acted on (interaction-reply correlation is Task 6).
///
/// Every event's raw wire `sequence` is checked against the last-processed
/// sequence FIRST, before it is even decoded: an event with `sequence <=
/// last_sequence` is a replay (e.g. a re-sent batch after a component
/// restart/reconnect) and is dropped outright, so a replayed `message.*`
/// never double-dispatches into the `Router` and a replayed `op.result`/
/// `approval.decision` never double-resolves a correlation. `last_sequence`
/// only advances past an event once it is accepted, so replays are compared
/// against the highest sequence actually processed, not merely seen.
///
/// Exits when the supervisor drops the sink.
async fn drain_inbound(
    mut inbound: mpsc::UnboundedReceiver<GatewayInboundEvent>,
    correlation: Arc<Correlation>,
    router: Arc<OnceLock<Arc<Router>>>,
    gateway_id: String,
) {
    let mut last_sequence: u64 = 0;
    while let Some(event) = inbound.recv().await {
        if event.sequence <= last_sequence {
            tracing::trace!(
                event = %event.event_type,
                sequence = event.sequence,
                last_sequence,
                "wasm gateway inbound event dropped: replayed sequence"
            );
            continue;
        }
        last_sequence = event.sequence;

        match InboundEvent::decode(&event.event_type, &event.payload) {
            Ok(InboundEvent::OpResult { op_id, result }) => {
                correlation.resolve(
                    &CorrelationKey::Op(op_id),
                    CorrelationValue::OpResult(result),
                );
            }
            Ok(InboundEvent::ApprovalDecision {
                request_id,
                allow,
                actor,
            }) => {
                correlation.resolve(
                    &CorrelationKey::Approval(request_id),
                    CorrelationValue::Approval { allow, actor },
                );
            }
            Ok(InboundEvent::MessageMention {
                workspace_id,
                actor,
                prompt,
                attachments,
            }) => {
                if let Some(router) = require_router(&router, &event.event_type) {
                    let attachments = attachment_refs(&attachments);
                    if let Err(error) = router
                        .on_start(&gateway_id, &workspace_id, &actor, &prompt, &attachments)
                        .await
                    {
                        tracing::warn!(
                            event = %event.event_type,
                            "wasm gateway on_start failed: {error}"
                        );
                    }
                }
            }
            Ok(InboundEvent::MessageThread {
                conversation_id,
                actor,
                prompt,
                attachments,
            }) => {
                if let Some(router) = require_router(&router, &event.event_type) {
                    let attachments = attachment_refs(&attachments);
                    if let Err(error) = router
                        .on_reply(&gateway_id, &conversation_id, &actor, &prompt, &attachments)
                        .await
                    {
                        tracing::warn!(
                            event = %event.event_type,
                            "wasm gateway on_reply failed: {error}"
                        );
                    }
                }
            }
            Ok(InboundEvent::MessageDm {
                conversation_id,
                user_id,
                text,
            }) => {
                if let Some(router) = require_router(&router, &event.event_type) {
                    if let Err(error) = router
                        .on_dm(&gateway_id, &conversation_id, &user_id, &text)
                        .await
                    {
                        tracing::warn!(
                            event = %event.event_type,
                            "wasm gateway on_dm failed: {error}"
                        );
                    }
                }
            }
            Ok(
                InboundEvent::SlashConnect { .. }
                | InboundEvent::SlashEnd { .. }
                | InboundEvent::SlashStop { .. }
                | InboundEvent::SlashStatus { .. },
            ) => {
                // Slash-command routing (`on_connect`/`on_end`/`on_stop`) and its
                // `interaction-reply` correlation is Task 6 — these events are
                // decoded (proving the wire contract round-trips) but otherwise a
                // no-op here.
                tracing::trace!(
                    event = %event.event_type,
                    "wasm gateway slash inbound event not yet routed (Task 6)"
                );
            }
            Err(error) => {
                // Undecodable (e.g. the fixture's non-JSON `message` seed) — drop.
                tracing::trace!(
                    event = %event.event_type,
                    "wasm gateway inbound event undecodable, dropped: {error}"
                );
            }
        }
    }
}

/// Look up the stored router, tracing (and returning `None`) if `set_router`
/// hasn't been called yet — matches the native Discord gateway's "events
/// arriving before `set_router` are dropped with a warning" behaviour
/// (`gateway/discord/mod.rs`'s `InboundRouting::handle_message`).
fn require_router<'a>(
    router: &'a OnceLock<Arc<Router>>,
    event_type: &str,
) -> Option<&'a Arc<Router>> {
    let router = router.get();
    if router.is_none() {
        tracing::trace!(
            event = %event_type,
            "wasm gateway inbound event dropped: no router set yet"
        );
    }
    router
}

/// Convert the wire `attachments: Vec<String>` (URLs only — the component
/// pre-computes/normalizes everything else per design doc §5.2, so the bridge
/// does no further Discord-specific processing) into the `Router`'s
/// `&[AttachmentRef]` shape. The wire carries no filename/content-type/size
/// metadata, so `name` is derived from the URL's basename (the same helper
/// `Router::on_connect` uses to name a project from a git URL) and
/// `content_type`/`size` are left unknown: `attachments::materialize_attachments`
/// only uses `name` (for its extension allowlist) and re-derives the real
/// size from the actual download, so a `0` declared size never falsely trips
/// its size cap.
fn attachment_refs(urls: &[String]) -> Vec<AttachmentRef> {
    urls.iter()
        .map(|url| AttachmentRef {
            name: crate::control::basename_of(url),
            url: url.clone(),
            content_type: None,
            size: 0,
        })
        .collect()
}

/// Sample the supervisor snapshot on an interval and publish `Connected`/
/// `Offline` transitions (the publisher only emits on an actual change).
async fn watch_status(
    snapshot: Arc<Mutex<GatewaySnapshot>>,
    publisher: Arc<GatewayStatusPublisher>,
) {
    let mut ticker = tokio::time::interval(STATUS_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let running = snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running;
        publisher.publish(if running {
            GatewayStatus::Connected
        } else {
            GatewayStatus::Offline
        });
    }
}

impl Drop for WasmGateway {
    fn drop(&mut self) {
        // Hard-stop the supervisor and background tasks so a dropped gateway
        // leaves nothing running (mirrors `WasmGatewaySupervisor::abort`).
        self.supervisor.abort();
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[async_trait]
impl Gateway for WasmGateway {
    fn id(&self) -> &str {
        &self.id
    }

    async fn start(&self) -> anyhow::Result<()> {
        // The supervisor already `start`s the component on spawn and keeps it
        // running with capped-backoff restarts, so there is no separate start
        // handshake to perform here.
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        self.supervisor.stop().await;
        Ok(())
    }

    async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
        let op_id = self.next_op_id();
        let op = OutboundOp::CreateChannel {
            op_id: op_id.clone(),
            name: name.to_string(),
        };
        self.run_op(op_id, op)
            .await?
            .channel_id
            .ok_or_else(|| anyhow!("create-channel op.result is missing channel_id"))
    }

    async fn create_conversation(&self, workspace_id: &str, title: &str) -> anyhow::Result<String> {
        let op_id = self.next_op_id();
        let op = OutboundOp::CreateThread {
            op_id: op_id.clone(),
            channel_id: workspace_id.to_string(),
            title: title.to_string(),
        };
        self.run_op(op_id, op)
            .await?
            .thread_id
            .ok_or_else(|| anyhow!("create-thread op.result is missing thread_id"))
    }

    async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
        let op_id = self.next_op_id();
        let op = OutboundOp::SendMessage {
            op_id: op_id.clone(),
            channel_id: surface.conversation_id.clone(),
            text: text.to_string(),
        };
        let message_id = self
            .run_op(op_id, op)
            .await?
            .message_id
            .ok_or_else(|| anyhow!("send-message op.result is missing message_id"))?;
        Ok(MessageRef {
            surface: surface.clone(),
            message_id,
        })
    }

    async fn edit_status(&self, msg: &MessageRef, text: &str) -> anyhow::Result<()> {
        let op_id = self.next_op_id();
        let op = OutboundOp::EditMessage {
            op_id: op_id.clone(),
            channel_id: msg.surface.conversation_id.clone(),
            message_id: msg.message_id.clone(),
            text: text.to_string(),
        };
        self.run_op(op_id, op).await?;
        Ok(())
    }

    async fn post_result(&self, surface: &Surface, chunks: &[String]) -> anyhow::Result<()> {
        let op_id = self.next_op_id();
        let op = OutboundOp::SendMessages {
            op_id: op_id.clone(),
            channel_id: surface.conversation_id.clone(),
            chunks: chunks.to_vec(),
        };
        self.run_op(op_id, op).await?;
        Ok(())
    }

    async fn post_error(&self, surface: &Surface, message: &str) -> anyhow::Result<()> {
        let op_id = self.next_op_id();
        let op = OutboundOp::SendMessage {
            op_id: op_id.clone(),
            channel_id: surface.conversation_id.clone(),
            text: message.to_string(),
        };
        // Native `post_error` returns `()`; the op.result (a message_id) is
        // acknowledged and discarded.
        self.run_op(op_id, op).await?;
        Ok(())
    }

    async fn request_approval(
        &self,
        surface: &Surface,
        req: &ApprovalRequest,
    ) -> anyhow::Result<ApprovalDecision> {
        let op_id = self.next_op_id();
        // Correlated by `request_id` (resolved by a later `approval.decision`),
        // NOT by `op_id` — the T3 mapping (design doc §5.4).
        let key = CorrelationKey::Approval(req.request_id.clone());
        let timeout = req
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_APPROVAL_TIMEOUT);
        let waiter = self.correlation.register(key.clone(), timeout);
        let op = OutboundOp::ApprovalRequest {
            op_id,
            request_id: req.request_id.clone(),
            conversation_id: surface.conversation_id.clone(),
            tool: req.tool.clone(),
            summary: req.summary.clone(),
            approver_role_ids: req.approver_role_ids.clone(),
            started_by: req.started_by.clone(),
            timeout_ms: req.timeout_ms,
        };
        if let Err(reason) = self.deliver(op).await {
            self.correlation.cancel(&key);
            let _ = waiter.await;
            bail!("wasm gateway approval delivery failed: {reason}");
        }
        match waiter.await {
            CorrelationOutcome::Resolved(CorrelationValue::Approval { allow, .. }) => {
                Ok(if allow {
                    ApprovalDecision::AllowOnce
                } else {
                    ApprovalDecision::RejectOnce
                })
            }
            CorrelationOutcome::Resolved(other) => {
                bail!("approval {key:?} resolved with a non-approval value: {other:?}")
            }
            // Timeout auto-rejects, matching the native gateway's behaviour.
            CorrelationOutcome::TimedOut => Ok(ApprovalDecision::RejectOnce),
        }
    }

    fn set_router(&self, router: Arc<Router>) {
        // First writer wins (single `set_router` call, per the trait doc).
        let _ = self.router.set(router);
    }

    fn subscribe_status(&self) -> Option<GatewayStatusSubscription> {
        Some(self.status.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------
    // InboundEvent round-trip
    // -------------------------------------------------------------

    fn assert_inbound_round_trips(event: InboundEvent, expected_event_type: &str) {
        let (event_type, payload) = event.encode();
        assert_eq!(
            event_type, expected_event_type,
            "wire event-type must match the contract exactly"
        );
        let decoded = InboundEvent::decode(&event_type, &payload)
            .expect("a just-encoded event must decode cleanly");
        assert_eq!(decoded, event, "round trip must be lossless");
    }

    #[test]
    fn message_mention_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::MessageMention {
                workspace_id: "ws1".into(),
                actor: "u1".into(),
                prompt: "hello".into(),
                attachments: vec!["https://x/a.png".into()],
            },
            "message.mention",
        );
    }

    #[test]
    fn message_thread_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::MessageThread {
                conversation_id: "conv1".into(),
                actor: "u1".into(),
                prompt: "reply".into(),
                attachments: vec![],
            },
            "message.thread",
        );
    }

    #[test]
    fn message_dm_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::MessageDm {
                conversation_id: "dm1".into(),
                user_id: "u1".into(),
                text: "hi".into(),
            },
            "message.dm",
        );
    }

    #[test]
    fn slash_connect_round_trips_with_full_opts() {
        assert_inbound_round_trips(
            InboundEvent::SlashConnect {
                token: "tok".into(),
                user_id: "u1".into(),
                opts: ConnectOptsWire {
                    name: Some("proj".into()),
                    git: Some("https://git/repo".into()),
                    model: Some("opus".into()),
                    effort: Some("high".into()),
                    mode: Some("bypassPermissions".into()),
                },
                role_ids: vec!["role1".into(), "role2".into()],
            },
            "slash.connect",
        );
    }

    #[test]
    fn slash_connect_round_trips_with_bare_opts() {
        // `/connect` with no arguments — every opts field absent, matching
        // what the native command allows.
        assert_inbound_round_trips(
            InboundEvent::SlashConnect {
                token: "tok".into(),
                user_id: "u1".into(),
                opts: ConnectOptsWire::default(),
                role_ids: vec![],
            },
            "slash.connect",
        );
    }

    #[test]
    fn slash_connect_decodes_when_opts_omits_every_field() {
        // A component may omit optional keys entirely rather than sending
        // explicit nulls — decode must tolerate that.
        let payload = br#"{"token":"tok","user_id":"u1","opts":{},"role_ids":[]}"#.to_vec();
        let decoded = InboundEvent::decode("slash.connect", &payload).unwrap();
        assert_eq!(
            decoded,
            InboundEvent::SlashConnect {
                token: "tok".into(),
                user_id: "u1".into(),
                opts: ConnectOptsWire::default(),
                role_ids: vec![],
            }
        );
    }

    #[test]
    fn slash_end_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::SlashEnd {
                token: "tok".into(),
                conversation_id: "conv1".into(),
            },
            "slash.end",
        );
    }

    #[test]
    fn slash_stop_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::SlashStop {
                token: "tok".into(),
                conversation_id: "conv1".into(),
            },
            "slash.stop",
        );
    }

    #[test]
    fn slash_status_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::SlashStatus {
                token: "tok".into(),
            },
            "slash.status",
        );
    }

    #[test]
    fn approval_decision_round_trips() {
        assert_inbound_round_trips(
            InboundEvent::ApprovalDecision {
                request_id: "req1".into(),
                allow: true,
                actor: "u1".into(),
            },
            "approval.decision",
        );
    }

    #[test]
    fn op_result_round_trips_and_stays_flat_on_the_wire() {
        let event = InboundEvent::OpResult {
            op_id: "op1".into(),
            result: OpResultBody {
                channel_id: Some("chan1".into()),
                thread_id: None,
                message_id: None,
                ok: None,
            },
        };
        let (event_type, payload) = event.encode();
        assert_eq!(event_type, "op.result");
        // `result`'s fields must be flattened alongside `op_id`, not nested
        // under a `"result"` key — this is the wire shape design doc
        // §5.3's `op-result` table row assumes.
        let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(json["op_id"], "op1");
        assert_eq!(json["channel_id"], "chan1");
        assert!(json.get("result").is_none());

        let decoded = InboundEvent::decode(&event_type, &payload).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn decode_rejects_unknown_event_type() {
        let err = InboundEvent::decode("message.unknown", b"{}").unwrap_err();
        assert!(
            err.to_string().contains("message.unknown")
                || format!("{err:#}").contains("message.unknown")
        );
    }

    // -------------------------------------------------------------
    // OutboundOp round-trip
    // -------------------------------------------------------------

    fn assert_outbound_round_trips(op: OutboundOp, expected_event_type: &str) {
        let (event_type, payload) = op.encode();
        assert_eq!(
            event_type, expected_event_type,
            "wire event-type must match the contract exactly"
        );
        let decoded = OutboundOp::decode(&event_type, &payload)
            .expect("a just-encoded op must decode cleanly");
        assert_eq!(decoded, op, "round trip must be lossless");
    }

    #[test]
    fn create_channel_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::CreateChannel {
                op_id: "op1".into(),
                name: "general".into(),
            },
            "create-channel",
        );
    }

    #[test]
    fn create_thread_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::CreateThread {
                op_id: "op1".into(),
                channel_id: "chan1".into(),
                title: "session".into(),
            },
            "create-thread",
        );
    }

    #[test]
    fn send_message_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::SendMessage {
                op_id: "op1".into(),
                channel_id: "chan1".into(),
                text: "hi".into(),
            },
            "send-message",
        );
    }

    #[test]
    fn edit_message_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::EditMessage {
                op_id: "op1".into(),
                channel_id: "chan1".into(),
                message_id: "msg1".into(),
                text: "edited".into(),
            },
            "edit-message",
        );
    }

    #[test]
    fn send_messages_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::SendMessages {
                op_id: "op1".into(),
                channel_id: "chan1".into(),
                chunks: vec!["part1".into(), "part2".into()],
            },
            "send-messages",
        );
    }

    #[test]
    fn approval_request_round_trips_with_all_fields() {
        assert_outbound_round_trips(
            OutboundOp::ApprovalRequest {
                op_id: "op1".into(),
                request_id: "req1".into(),
                conversation_id: "conv1".into(),
                tool: "bash".into(),
                summary: "run `ls`".into(),
                approver_role_ids: vec!["role1".into()],
                started_by: Some("u1".into()),
                timeout_ms: Some(30_000),
            },
            "approval-request",
        );
    }

    #[test]
    fn approval_request_round_trips_with_optional_fields_absent() {
        assert_outbound_round_trips(
            OutboundOp::ApprovalRequest {
                op_id: "op1".into(),
                request_id: "req1".into(),
                conversation_id: "conv1".into(),
                tool: "bash".into(),
                summary: "run `ls`".into(),
                approver_role_ids: vec![],
                started_by: None,
                timeout_ms: None,
            },
            "approval-request",
        );
    }

    #[test]
    fn interaction_reply_round_trips() {
        assert_outbound_round_trips(
            OutboundOp::InteractionReply {
                token: "tok".into(),
                text: "done".into(),
            },
            "interaction-reply",
        );
    }

    // -------------------------------------------------------------
    // Correlation
    // -------------------------------------------------------------

    #[tokio::test]
    async fn register_then_resolve_delivers_the_value_to_the_waiter() {
        let correlation = Correlation::new();
        let key = CorrelationKey::Op("op1".into());
        let waiter = correlation.register(key.clone(), Duration::from_secs(5));

        let value = CorrelationValue::OpResult(OpResultBody {
            channel_id: Some("chan1".into()),
            ..Default::default()
        });
        assert!(correlation.resolve(&key, value.clone()));
        assert_eq!(waiter.await, CorrelationOutcome::Resolved(value));
        assert!(
            correlation.is_empty(),
            "a resolved registration must not remain in the map"
        );
    }

    #[tokio::test]
    async fn resolving_an_approval_key_delivers_the_decision() {
        let correlation = Correlation::new();
        let key = CorrelationKey::Approval("req1".into());
        let waiter = correlation.register(key.clone(), Duration::from_secs(5));

        let value = CorrelationValue::Approval {
            allow: false,
            actor: "u1".into(),
        };
        assert!(correlation.resolve(&key, value.clone()));
        assert_eq!(waiter.await, CorrelationOutcome::Resolved(value));
    }

    #[tokio::test]
    async fn cancel_removes_the_entry_and_the_waiter_resolves_to_timed_out_promptly() {
        let correlation = Correlation::new();
        let key = CorrelationKey::Op("op1".into());
        // A long timeout: only `cancel` should make the waiter resolve, and it
        // must do so promptly (not after the full timeout).
        let waiter = correlation.register(key.clone(), Duration::from_secs(3600));
        assert_eq!(correlation.len(), 1);

        assert!(
            correlation.cancel(&key),
            "cancel must report the entry removed"
        );
        assert!(
            correlation.is_empty(),
            "cancel must remove the pending entry"
        );
        assert_eq!(
            waiter.await,
            CorrelationOutcome::TimedOut,
            "a cancelled registration resolves to TimedOut without blocking"
        );
        // Cancelling again is a harmless no-op.
        assert!(!correlation.cancel(&key));
    }

    #[tokio::test]
    async fn resolve_on_an_unregistered_key_returns_false_and_is_a_no_op() {
        let correlation = Correlation::new();
        let resolved = correlation.resolve(
            &CorrelationKey::Op("missing".into()),
            CorrelationValue::OpResult(OpResultBody::default()),
        );
        assert!(!resolved);
    }

    #[tokio::test(start_paused = true)]
    async fn unresolved_registration_times_out_and_is_removed_from_the_map() {
        let correlation = Correlation::new();
        let key = CorrelationKey::Op("op1".into());
        let waiter = correlation.register(key.clone(), Duration::from_millis(20));

        assert_eq!(
            correlation.len(),
            1,
            "registration must be visible immediately"
        );

        // `start_paused` lets this advance the virtual clock past the
        // timeout instantly instead of sleeping in real wall-clock time.
        tokio::time::advance(Duration::from_millis(25)).await;

        assert_eq!(waiter.await, CorrelationOutcome::TimedOut);
        assert!(
            correlation.is_empty(),
            "a timed-out registration must be removed from the map — no leak"
        );

        // The now-stale key can no longer be resolved (nothing is waiting).
        assert!(!correlation.resolve(&key, CorrelationValue::OpResult(OpResultBody::default())));
    }
}

// ---------------------------------------------------------------------
// WasmGateway integration tests over the extended `component-gateway` fixture
// ---------------------------------------------------------------------

#[cfg(test)]
mod gateway_impl_tests {
    use super::*;

    use crate::gateway::Gateway;
    use crate::plugins::build_fixture_components_once as build_fixtures;
    use crate::plugins::bundle::InstalledBundle;
    use crate::plugins::capabilities::PluginCapabilityContext;
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
    use crate::plugins::wasm_gateway::{GatewayConfig, SupervisorTuning};
    use crate::settings::SettingsStore;
    use crate::store::{ComponentPluginReleaseRecord, Store};
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{
        PluginBundleManifest, PluginLifecycle, PluginPermissions, PluginRelease,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    fn gateway_artifact() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/component-gateway/target/wasm32-wasip2/release")
            .join("ryuzi_component_gateway_fixture.wasm")
    }

    /// Fast tuning so op.results (immediate post-deliver poll) and the deferred
    /// approval decision (one poll later) both resolve within a few ms.
    fn fast_tuning() -> SupervisorTuning {
        SupervisorTuning {
            poll_interval: Duration::from_millis(20),
            ..SupervisorTuning::default()
        }
    }

    async fn build_test_gateway(config: GatewayConfig) -> (WasmGateway, tempfile::NamedTempFile) {
        build_fixtures();
        let mut policy = HostPolicy::deny_all();
        policy.limits.timeout = Duration::from_secs(5);
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
        let gateway = WasmGateway::new(
            "acme-gateway".to_string(),
            compiled,
            ctx,
            config,
            fast_tuning(),
        );
        (gateway, tmp)
    }

    fn test_surface() -> Surface {
        Surface {
            gateway: "acme-gateway".to_string(),
            conversation_id: "chan-1".to_string(),
        }
    }

    fn approval_req(request_id: &str, summary: &str, timeout_ms: Option<u64>) -> ApprovalRequest {
        ApprovalRequest {
            run_id: "run-1".to_string(),
            requesting_agent_id: "agent-1".to_string(),
            requesting_agent_name: "Agent 1".to_string(),
            request_id: request_id.to_string(),
            tool: "Bash".to_string(),
            summary: summary.to_string(),
            approver_role_ids: vec![],
            started_by: None,
            timeout_ms,
            principal: None,
        }
    }

    /// The headline correlation test: `create_workspace` mints an op, delivers a
    /// `create-channel`, and returns the `channel_id` the component echoes back
    /// via the correlated `op.result`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_create_workspace_correlates_op_result() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let channel_id = gateway
            .create_workspace("proj")
            .await
            .expect("create_workspace must resolve its op.result");
        assert_eq!(channel_id, "chan-1");
        assert!(
            gateway.correlation.is_empty(),
            "the resolved op must leave no pending correlation entry"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_create_conversation_returns_thread_id() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let thread_id = gateway
            .create_conversation("chan-1", "a task")
            .await
            .expect("create_conversation must resolve its op.result");
        assert_eq!(thread_id, "thread-1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_post_status_returns_message_ref_from_op_result() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let surface = test_surface();
        let msg = gateway
            .post_status(&surface, "working")
            .await
            .expect("post_status must resolve its op.result");
        assert_eq!(
            msg.message_id, "msg-1",
            "message id must come from op.result"
        );
        assert_eq!(
            msg.surface, surface,
            "the ref keeps the originating surface"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_edit_status_and_post_result_succeed() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let surface = test_surface();
        let msg = MessageRef {
            surface: surface.clone(),
            message_id: "msg-1".to_string(),
        };
        gateway
            .edit_status(&msg, "still working")
            .await
            .expect("edit_status must resolve Ok");
        gateway
            .post_result(&surface, &["part1".to_string(), "part2".to_string()])
            .await
            .expect("post_result must resolve Ok");
        gateway
            .post_error(&surface, "boom")
            .await
            .expect("post_error must resolve Ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_request_approval_resolves_to_allow_once() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let surface = test_surface();
        let decision = gateway
            .request_approval(
                &surface,
                &approval_req("req-1", "please allow", Some(5_000)),
            )
            .await
            .expect("request_approval must resolve to a decision");
        // The fixture emits `approval.decision{allow:true, actor:"tester"}`.
        assert_eq!(decision, ApprovalDecision::AllowOnce);
        assert!(
            gateway.correlation.is_empty(),
            "a resolved approval must leave no pending correlation entry"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_request_approval_times_out_to_reject_once() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        let surface = test_surface();
        // A "silent" summary: the fixture accepts the request but never emits a
        // decision, so the bridge's `timeout_ms` must auto-reject.
        let decision = gateway
            .request_approval(
                &surface,
                &approval_req("req-2", "silent request", Some(150)),
            )
            .await
            .expect("request_approval must resolve (via timeout) to a decision");
        assert_eq!(decision, ApprovalDecision::RejectOnce);
        assert!(
            gateway.correlation.is_empty(),
            "a timed-out approval must leave no pending correlation entry"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wasm_gateway_reports_id_and_status_subscription() {
        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "wss://example.invalid/gw".to_string(),
        })
        .await;

        assert_eq!(gateway.id(), "acme-gateway");
        // A status subscription is offered and reaches `Connected` once the
        // supervisor reports the component running.
        let mut sub = gateway
            .subscribe_status()
            .expect("wasm gateway offers a status subscription");
        if sub.initial == GatewayStatus::Connected {
            return;
        }
        let connected = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match sub.events.recv().await {
                    Ok(GatewayStatus::Connected) => return true,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        sub.resync();
                    }
                    Err(_) => return false,
                }
            }
        })
        .await
        .expect("status must reach Connected within the deadline");
        assert!(connected, "gateway must report Connected");
    }

    // -------------------------------------------------------------
    // Task 5: inbound `message.*` -> Router dispatch + sequence dedup
    // -------------------------------------------------------------
    //
    // These reuse the SAME `component-gateway` fixture (extended for Task 5
    // with a "message-flow"/"dm-flow" `endpoint` marker) but drive a REAL
    // `Router` over a `ControlPlane`, mirroring `router.rs`'s own inbound
    // test harness (`wired_control_plane`/`StateDirGuard`/a `send_prompt`-
    // recording `Harness`) rather than inventing a mock `Router` — `Router`
    // is a concrete struct with no trait seam to mock, and this harness
    // already exists and is proven in `router.rs`'s own Task 4 tests and
    // `daemon.rs`'s inbound tests (duplicated here, not imported: both are
    // private to their own `mod tests`).

    use crate::control::ControlPlane;
    use crate::domain::{
        AgentIdentitySnapshot, PermMode, Project, Session, SessionKind, SessionStatus,
    };
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::router::Router;
    use serial_test::serial;

    /// A minimal `Gateway` that only records `create_conversation` calls
    /// (`on_start` is the only inbound Router method that touches a
    /// `Gateway`), registered under the SAME id the tested `WasmGateway` uses
    /// (`"acme-gateway"`, see `test_surface`/`build_test_gateway` above) so
    /// `Router::on_start` finds it via `gateway_id`. Mirrors `router.rs`'s own
    /// private `FakeGateway` test harness.
    struct RecordingGateway {
        gid: String,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingGateway {
        fn new(gid: &str) -> Self {
            RecordingGateway {
                gid: gid.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Gateway for RecordingGateway {
        fn id(&self) -> &str {
            &self.gid
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_workspace:{name}"));
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(
            &self,
            workspace_id: &str,
            title: &str,
        ) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_conversation:{workspace_id}:{title}"));
            Ok("conv-0".to_string())
        }
        async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("post_status:{}:{}", surface.conversation_id, text));
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m0".to_string(),
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
            _req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::AllowOnce)
        }
    }

    /// A harness session that completes each turn immediately but ALSO
    /// appends the turn's display text to a shared log — the seam this suite
    /// uses to prove a Router call actually reached a live session turn (and,
    /// for the dedup test, that a replayed `message.thread` does NOT produce
    /// a second entry). Mirrors `daemon.rs`'s own `ResumeFakeSession`/
    /// `router.rs`'s `OneShotSession` (both private to their own `mod tests`).
    struct RecordingSession {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl HarnessSession for RecordingSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(prompt.display);
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
    struct RecordingHarness {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Harness for RecordingHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(RecordingSession {
                log: self.log.clone(),
            }))
        }
    }
    struct RecordingHarnessFactory {
        log: Arc<Mutex<Vec<String>>>,
    }
    impl HarnessFactory for RecordingHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(RecordingHarness {
                log: self.log.clone(),
            }))
        }
    }

    /// Redirect `dirs::data_dir()`/`HOME` into a tempdir for the test's
    /// duration — `on_start`'s `start_session` (worktree/scratch-dir prep)
    /// otherwise touches the real state dir. Process-global env, so every
    /// test using it must be `#[serial]`. Mirrors `router.rs`'s own
    /// `StateDirGuard` (private to that module).
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

    /// A `ControlPlane` wired with a working LLM connection + default agent
    /// route (so `start_session`/`continue_session`/`start_chat_session`
    /// resolve a model instead of erroring) and `harness` as its harness
    /// factory. Mirrors `router.rs`'s own `wired_control_plane_with_harness`
    /// (private to that module, so duplicated here rather than exported).
    async fn wired_control_plane(
        harness: Arc<dyn HarnessFactory>,
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        let db_guard = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db_guard.path()).await.unwrap());
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
        let mut regs = crate::plugins::Registries::new();
        regs.harness = harness;
        let cp = {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            ControlPlane::new(store, regs, persistence).await
        };
        let store_ref = cp.store().clone();
        (cp, store_ref, db_guard)
    }

    /// Seed a non-git project directly (no `on_connect`/git provisioning
    /// needed) — mirrors `daemon.rs`'s own `seed_project` test helper.
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

    /// Seed an Idle, project-less `chat` session directly and bind it to
    /// `(gateway_id, conversation_id)` — bypassing `on_start`/`on_connect`
    /// entirely, exactly the pattern `daemon.rs`'s own inbound-`on_reply`
    /// tests use to drive `on_reply` against a cold (never-live) session.
    async fn seed_chat_session(
        cp: &ControlPlane,
        store: &Store,
        gateway_id: &str,
        conversation_id: &str,
        session_pk: &str,
    ) {
        let primary_agent = cp.registry().resolved_snapshot("ryuzi").await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: session_pk.to_string(),
                primary_agent_id: Some(primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(AgentIdentitySnapshot {
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
            .add_surface(gateway_id, conversation_id, session_pk)
            .await
            .unwrap();
    }

    /// Poll `predicate(log)` up to `attempts` times (short sleep between
    /// reads) — mirrors `router.rs`'s `wait_for_sessions`/`wait_for_status`
    /// polling style, applied to this suite's harness-log assertions.
    async fn wait_for_log(
        log: &Arc<Mutex<Vec<String>>>,
        attempts: usize,
        predicate: impl Fn(&[String]) -> bool,
    ) -> Vec<String> {
        for _ in 0..attempts {
            let snapshot = log.lock().unwrap().clone();
            if predicate(&snapshot) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        log.lock().unwrap().clone()
    }

    /// `message.mention` decodes and reaches `Router::on_start`: the fixture's
    /// "message-flow" endpoint emits a mention for `workspace_id "ws-1"`
    /// (bound below to a real project), so `on_start` must call
    /// `create_conversation("ws-1", "start it")` on the gateway registered
    /// under this `WasmGateway`'s own id — proving the decoded `workspace_id`
    /// and prompt-derived title both round-tripped correctly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn inbound_mention_message_drives_on_start_with_decoded_fields() {
        let _guard = StateDirGuard::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (cp, store, _db_guard) =
            wired_control_plane(Arc::new(RecordingHarnessFactory { log })).await;
        let recording = Arc::new(RecordingGateway::new("acme-gateway"));
        let router = Arc::new(Router::new(
            Arc::clone(&cp),
            vec![recording.clone() as Arc<dyn Gateway>],
        ));
        seed_project(&store, "proj-1").await;
        store
            .bind_project("acme-gateway", "ws-1", "proj-1")
            .await
            .unwrap();

        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "message-flow".to_string(),
        })
        .await;
        gateway.set_router(Arc::clone(&router));

        let mut calls = Vec::new();
        for _ in 0..300 {
            calls = recording.calls();
            if !calls.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            calls
                .iter()
                .any(|c| c == "create_conversation:ws-1:start it"),
            "expected create_conversation:ws-1:start it (the decoded mention's \
             workspace_id + prompt-derived title), got: {calls:?}"
        );
    }

    /// `message.dm` decodes and reaches `Router::on_dm`: no `/connect`
    /// binding is involved, so the fixture's "dm-flow" endpoint's single
    /// `message.dm` must bind a project-less `chat` session to its
    /// `conversation_id` — mirroring `router.rs`'s own
    /// `discord_dm_starts_a_chat_session` assertion style.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn inbound_dm_message_drives_on_dm_with_decoded_fields() {
        let _guard = StateDirGuard::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (cp, store, _db_guard) =
            wired_control_plane(Arc::new(RecordingHarnessFactory { log })).await;
        let router = Arc::new(Router::new(Arc::clone(&cp), vec![]));

        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "dm-flow".to_string(),
        })
        .await;
        gateway.set_router(Arc::clone(&router));

        let mut bound = None;
        for _ in 0..300 {
            bound = store
                .resolve_by_conversation("acme-gateway", "dm-conv-1")
                .await
                .unwrap();
            if bound.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let session = bound.expect("on_dm must bind a session to dm-conv-1");
        assert_eq!(
            session.kind,
            SessionKind::Chat,
            "on_dm starts a project-less chat session"
        );
    }

    /// The headline Task-5 test: `message.thread` decodes and reaches
    /// `Router::on_reply` for a session already bound to "conv-0", and the
    /// fixture's immediately-following DUPLICATE `message.thread` (identical
    /// event-type/payload/sequence) must be dropped by the bridge's replay
    /// dedup rather than dispatching a second `on_reply` — proven by the
    /// recording harness's `send_prompt` log settling at exactly ONE entry,
    /// never two.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn inbound_thread_message_drives_on_reply_and_dedups_replays() {
        let _guard = StateDirGuard::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (cp, store, _db_guard) =
            wired_control_plane(Arc::new(RecordingHarnessFactory { log: log.clone() })).await;
        let router = Arc::new(Router::new(Arc::clone(&cp), vec![]));
        // Bound directly (no `on_start` involved): the fixture's "message-flow"
        // endpoint's `message.mention` (workspace_id "ws-1", left unbound here)
        // is a harmless no-op; only its `message.thread` (conversation_id
        // "conv-0") and the duplicate matter for this test.
        seed_chat_session(&cp, &store, "acme-gateway", "conv-0", "session-1").await;

        let (gateway, _tmp) = build_test_gateway(GatewayConfig {
            account: "acme".to_string(),
            endpoint: "message-flow".to_string(),
        })
        .await;
        gateway.set_router(Arc::clone(&router));

        let settled = wait_for_log(&log, 300, |entries| !entries.is_empty()).await;
        assert_eq!(
            settled,
            vec!["continue it".to_string()],
            "on_reply must run exactly once, with the decoded thread prompt"
        );

        // Give a replayed (undeduped) on_reply every chance to show up before
        // asserting the log never grows past one entry.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let final_log = log.lock().unwrap().clone();
        assert_eq!(
            final_log,
            vec!["continue it".to_string()],
            "the duplicate message.thread (same sequence) must be dropped, not \
             dispatched as a second on_reply: {final_log:?}"
        );
    }
}
