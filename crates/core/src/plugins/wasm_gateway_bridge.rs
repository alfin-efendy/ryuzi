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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
