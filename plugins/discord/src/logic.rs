//! Pure Discord gateway v10 protocol state machine.
//!
//! Deterministic, host-free: [`on_frame`] advances the machine from one inbound
//! gateway JSON frame and returns the [`Action`]s the guest must perform;
//! [`due_heartbeat`] decides — given a monotonic clock value the caller supplies
//! — whether an op-1 heartbeat is due (and detects a zombie connection when an
//! ACK is missed). No `ryuzi:*` host handle and no wall clock are touched here,
//! so the whole protocol is exercised by the native `cargo test` below with
//! synthetic Discord frames.
//!
//! Opcodes and intent bit values are the Discord gateway v10 wire constants,
//! verified against the official docs (docs.discord.com/developers).

use serde_json::{json, Value};

/// The Discord gateway websocket endpoint (v10, JSON encoding).
pub const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

// Discord gateway opcodes (verified against docs.discord.com/developers gateway
// events reference): the only ones this protocol core acts on.
pub const OP_DISPATCH: u64 = 0;
pub const OP_HEARTBEAT: u64 = 1;
pub const OP_IDENTIFY: u64 = 2;
pub const OP_RESUME: u64 = 6;
pub const OP_RECONNECT: u64 = 7;
pub const OP_INVALID_SESSION: u64 = 9;
pub const OP_HELLO: u64 = 10;
pub const OP_HEARTBEAT_ACK: u64 = 11;

// Gateway intents (bit positions verified against the Discord gateway docs).
pub const INTENT_GUILDS: u64 = 1 << 0; // 1
pub const INTENT_GUILD_MESSAGES: u64 = 1 << 9; // 512
pub const INTENT_MESSAGE_CONTENT: u64 = 1 << 15; // 32768
/// The exact intents the native serenity gateway used (serenity_port.rs).
pub const GATEWAY_INTENTS: u64 = INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT;

/// Where the protocol currently is in the connect/identify/resume lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayPhase {
    Connecting,
    Identifying,
    Ready,
    Resuming,
}

/// A host-facing inbound `gateway-event` the machine wants surfaced through
/// `poll-inbound` (message/slash/approval in later tasks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEvent {
    pub event_type: String,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

/// A side effect the guest must perform as a result of an inbound frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SendFrame(String),
    SetHeartbeat(u64),
    EmitInbound(InboundEvent),
    Reconnect { resume: bool },
}

/// The full protocol state. Pure data — no host handles, no clock.
#[derive(Debug, Clone)]
pub struct GatewayState {
    pub phase: GatewayPhase,
    pub token: String,
    pub heartbeat_interval_ms: Option<u64>,
    pub last_seq: Option<u64>,
    pub session_id: Option<String>,
    pub resume_gateway_url: Option<String>,
    pub bot_user_id: Option<String>,
    pub heartbeat_acked: bool,
    pub next_heartbeat_due_ms: Option<u64>,
    pub pending_resume: bool,
    pub reconnect: Option<bool>,
}

impl GatewayState {
    pub fn new(token: impl Into<String>) -> Self {
        GatewayState {
            phase: GatewayPhase::Connecting,
            token: token.into(),
            heartbeat_interval_ms: None,
            last_seq: None,
            session_id: None,
            resume_gateway_url: None,
            bot_user_id: None,
            heartbeat_acked: true,
            next_heartbeat_due_ms: None,
            pending_resume: false,
            reconnect: None,
        }
    }

    pub fn take_reconnect(&mut self) -> Option<bool> {
        self.reconnect.take()
    }
}

/// Advance the state machine by one inbound gateway frame, returning the side
/// effects the guest must carry out. A malformed / non-object frame is ignored
/// (empty actions) — the host must never trap on garbage from the wire.
pub fn on_frame(state: &mut GatewayState, raw_json: &str) -> Vec<Action> {
    let value: Value = match serde_json::from_str(raw_json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    // Discord stamps the monotonic sequence `s` on DISPATCH frames only (null
    // otherwise). Capture it faithfully: it is the host-facing `sequence` for
    // emitted `message.*` events (Task 8) and the value carried in every
    // heartbeat + RESUME. `as_u64` is `None` for a null `s`, so non-dispatch
    // frames leave it untouched.
    if let Some(seq) = value.get("s").and_then(Value::as_u64) {
        state.last_seq = Some(seq);
    }

    let op = match value.get("op").and_then(Value::as_u64) {
        Some(op) => op,
        None => return Vec::new(),
    };

    match op {
        OP_HELLO => on_hello(state, &value),
        OP_DISPATCH => on_dispatch(state, &value),
        OP_HEARTBEAT => {
            // Server-requested heartbeat: send one immediately, now awaiting ack.
            state.heartbeat_acked = false;
            vec![Action::SendFrame(heartbeat_frame(state.last_seq))]
        }
        OP_HEARTBEAT_ACK => {
            state.heartbeat_acked = true;
            Vec::new()
        }
        OP_RECONNECT => on_reconnect(state, true),
        OP_INVALID_SESSION => {
            // `d` is a bool: whether the invalidated session may be resumed.
            let resumable = value.get("d").and_then(Value::as_bool).unwrap_or(false);
            on_reconnect(state, resumable)
        }
        _ => Vec::new(),
    }
}

/// HELLO: record the heartbeat interval and open the session — a RESUME on a
/// reconnect that still holds a session, otherwise a fresh IDENTIFY.
fn on_hello(state: &mut GatewayState, value: &Value) -> Vec<Action> {
    let interval = value
        .get("d")
        .and_then(|d| d.get("heartbeat_interval"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    state.heartbeat_interval_ms = Some(interval);
    // Fresh HELLO: treat the connection as alive and re-arm the heartbeat timer
    // on the next `due_heartbeat` tick (which supplies the clock reference).
    state.heartbeat_acked = true;
    state.next_heartbeat_due_ms = None;

    let mut actions = vec![Action::SetHeartbeat(interval)];
    if state.pending_resume && state.session_id.is_some() {
        state.pending_resume = false;
        state.phase = GatewayPhase::Resuming;
        actions.push(Action::SendFrame(resume_frame(state)));
    } else {
        state.pending_resume = false;
        state.phase = GatewayPhase::Identifying;
        actions.push(Action::SendFrame(identify_frame(state)));
    }
    actions
}

/// DISPATCH (op 0): `last_seq` is already updated by the caller. READY/RESUMED
/// drive the phase + session capture; MESSAGE_CREATE / INTERACTION_CREATE and
/// the rest are the Task 8/9 normalization hook (no emission here yet).
fn on_dispatch(state: &mut GatewayState, value: &Value) -> Vec<Action> {
    let event = value.get("t").and_then(Value::as_str).unwrap_or_default();
    match event {
        "READY" => {
            let data = value.get("d");
            state.session_id = str_field(data, "session_id");
            state.resume_gateway_url = str_field(data, "resume_gateway_url");
            state.bot_user_id = data
                .and_then(|d| d.get("user"))
                .and_then(|user| user.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            state.phase = GatewayPhase::Ready;
            Vec::new()
        }
        "RESUMED" => {
            state.phase = GatewayPhase::Ready;
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// RECONNECT / INVALID_SESSION: plan the reconnect. A resumable one keeps the
/// session (the next HELLO sends RESUME); a non-resumable one drops it (the next
/// HELLO sends a fresh IDENTIFY).
fn on_reconnect(state: &mut GatewayState, resume: bool) -> Vec<Action> {
    if resume {
        state.pending_resume = true;
        state.phase = GatewayPhase::Resuming;
    } else {
        state.pending_resume = false;
        state.session_id = None;
        state.resume_gateway_url = None;
        state.last_seq = None;
        state.phase = GatewayPhase::Connecting;
    }
    vec![Action::Reconnect { resume }]
}

/// Decide whether an op-1 heartbeat is due. The first tick after a HELLO arms
/// the timer (nothing due yet); once an interval has elapsed a heartbeat carrying
/// the last sequence is returned. If the previous heartbeat was never ACKed by
/// the time the next one comes due, the connection is a zombie: preserve the
/// session and signal a RESUME-reconnect through the state (the guest checks
/// [`GatewayState::take_reconnect`]) and send nothing.
pub fn due_heartbeat(state: &mut GatewayState, now_ms: u64) -> Option<String> {
    let interval = state.heartbeat_interval_ms?;
    if interval == 0 {
        return None;
    }
    match state.next_heartbeat_due_ms {
        None => {
            state.next_heartbeat_due_ms = Some(now_ms.saturating_add(interval));
            None
        }
        Some(due) if now_ms >= due => {
            if !state.heartbeat_acked {
                // Zombie connection. Keep `session_id` + `last_seq` and arm a
                // RESUME so the recovery HELLO replays missed events, exactly
                // like the RECONNECT / resumable-INVALID_SESSION paths — a fresh
                // IDENTIFY here would silently drop the session and any Discord
                // events that arrived during the heartbeat blackout.
                state.pending_resume = true;
                state.phase = GatewayPhase::Resuming;
                state.reconnect = Some(true);
                return None;
            }
            state.heartbeat_acked = false;
            state.next_heartbeat_due_ms = Some(now_ms.saturating_add(interval));
            Some(heartbeat_frame(state.last_seq))
        }
        Some(_) => None,
    }
}

/// The IDENTIFY frame (op 2): token + the three gateway intents + the connection
/// properties Discord requires.
fn identify_frame(state: &GatewayState) -> String {
    json!({
        "op": OP_IDENTIFY,
        "d": {
            "token": state.token,
            "intents": GATEWAY_INTENTS,
            "properties": {
                "os": "ryuzi",
                "browser": "ryuzi",
                "device": "ryuzi",
            },
        },
    })
    .to_string()
}

/// The RESUME frame (op 6): token + the stored session id + the last sequence.
fn resume_frame(state: &GatewayState) -> String {
    json!({
        "op": OP_RESUME,
        "d": {
            "token": state.token,
            "session_id": state.session_id,
            "seq": state.last_seq,
        },
    })
    .to_string()
}

/// The heartbeat frame (op 1): `d` is the last received sequence, or null.
fn heartbeat_frame(last_seq: Option<u64>) -> String {
    json!({ "op": OP_HEARTBEAT, "d": last_seq }).to_string()
}

/// Read a top-level string field from an optional `d` object.
fn str_field(data: Option<&Value>, key: &str) -> Option<String> {
    data.and_then(|d| d.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(interval: u64) -> String {
        json!({ "op": OP_HELLO, "d": { "heartbeat_interval": interval } }).to_string()
    }

    fn dispatch(event: &str, seq: u64, data: Value) -> String {
        json!({ "op": OP_DISPATCH, "t": event, "s": seq, "d": data }).to_string()
    }

    fn ready(seq: u64) -> String {
        dispatch(
            "READY",
            seq,
            json!({
                "session_id": "sess-abc",
                "resume_gateway_url": "wss://resume.discord.gg",
                "user": { "id": "111222333" }
            }),
        )
    }

    /// Extract the single `SendFrame` JSON from a set of actions, parsed.
    fn sent_frame(actions: &[Action]) -> Value {
        let raw = actions
            .iter()
            .find_map(|a| match a {
                Action::SendFrame(json) => Some(json.clone()),
                _ => None,
            })
            .expect("expected a SendFrame action");
        serde_json::from_str(&raw).expect("SendFrame carries valid JSON")
    }

    #[test]
    fn gateway_intents_are_the_three_verified_bits() {
        assert_eq!(INTENT_GUILDS, 1);
        assert_eq!(INTENT_GUILD_MESSAGES, 512);
        assert_eq!(INTENT_MESSAGE_CONTENT, 32768);
        assert_eq!(GATEWAY_INTENTS, 33281);
        assert_eq!(
            GATEWAY_INTENTS,
            INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT
        );
    }

    #[test]
    fn hello_sets_heartbeat_and_sends_identify() {
        let mut state = GatewayState::new("bot-token");
        let actions = on_frame(&mut state, &hello(41250));

        assert!(actions.contains(&Action::SetHeartbeat(41250)));
        assert_eq!(state.heartbeat_interval_ms, Some(41250));
        assert_eq!(state.phase, GatewayPhase::Identifying);

        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_IDENTIFY));
        assert_eq!(frame["d"]["token"].as_str(), Some("bot-token"));
        assert_eq!(frame["d"]["intents"].as_u64(), Some(33281));
        assert_eq!(
            frame["d"]["intents"].as_u64(),
            Some(INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT)
        );
        // IDENTIFY must carry connection properties (Discord rejects it otherwise).
        assert!(frame["d"]["properties"].is_object());
    }

    #[test]
    fn dispatch_updates_last_seq_without_emitting_yet() {
        let mut state = GatewayState::new("t");
        let actions = on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 42, json!({ "content": "hi" })),
        );
        assert_eq!(state.last_seq, Some(42));
        // Message normalization is Task 8 — nothing is emitted yet.
        assert!(actions.is_empty());
    }

    #[test]
    fn ready_stores_session_and_bot_identity() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &ready(1));

        assert_eq!(state.phase, GatewayPhase::Ready);
        assert_eq!(state.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(
            state.resume_gateway_url.as_deref(),
            Some("wss://resume.discord.gg")
        );
        assert_eq!(state.bot_user_id.as_deref(), Some("111222333"));
        assert_eq!(state.last_seq, Some(1));
    }

    #[test]
    fn due_heartbeat_arms_then_emits_op1_with_last_seq() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &dispatch("MESSAGE_CREATE", 7, json!({})));

        // First tick arms the timer; no heartbeat is due yet.
        assert_eq!(due_heartbeat(&mut state, 0), None);
        // After a full interval, a heartbeat carrying the last sequence is due.
        let frame = due_heartbeat(&mut state, 41250).expect("heartbeat due");
        let value: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["op"].as_u64(), Some(OP_HEARTBEAT));
        assert_eq!(value["d"].as_u64(), Some(7));
    }

    #[test]
    fn server_requested_heartbeat_sends_immediately() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &dispatch("MESSAGE_CREATE", 5, json!({})));

        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_HEARTBEAT, "d": null }).to_string(),
        );
        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_HEARTBEAT));
        assert_eq!(frame["d"].as_u64(), Some(5));
    }

    #[test]
    fn heartbeat_ack_marks_connection_alive() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250));
        assert_eq!(due_heartbeat(&mut state, 0), None); // arm
        assert!(due_heartbeat(&mut state, 41250).is_some()); // send -> awaiting ack
        assert!(!state.heartbeat_acked);

        on_frame(
            &mut state,
            &json!({ "op": OP_HEARTBEAT_ACK, "d": null }).to_string(),
        );
        assert!(state.heartbeat_acked);
    }

    #[test]
    fn missed_heartbeat_ack_signals_reconnect() {
        let mut state = GatewayState::new("bot-token");
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &ready(4)); // live session, last_seq = 4
        assert_eq!(due_heartbeat(&mut state, 0), None); // arm, next due 41250
        assert!(due_heartbeat(&mut state, 41250).is_some()); // send, awaiting ack
                                                             // No ACK arrives; the next due tick detects the zombie connection.
        assert_eq!(due_heartbeat(&mut state, 82500), None);
        assert_eq!(state.take_reconnect(), Some(true));

        // The zombie path must preserve the session and arm a RESUME (not a
        // fresh IDENTIFY) so no events are dropped across the blackout.
        assert!(state.pending_resume);
        assert_eq!(state.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(state.last_seq, Some(4));

        // The recovery connection's HELLO therefore emits RESUME, carrying the
        // preserved session id + sequence — proving no fresh IDENTIFY.
        let actions = on_frame(&mut state, &hello(41250));
        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_RESUME));
        assert_eq!(frame["d"]["session_id"].as_str(), Some("sess-abc"));
        assert_eq!(frame["d"]["seq"].as_u64(), Some(4));
    }

    #[test]
    fn reconnect_opcode_requests_a_resume() {
        let mut state = GatewayState::new("t");
        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_RECONNECT, "d": null }).to_string(),
        );
        assert_eq!(actions, vec![Action::Reconnect { resume: true }]);
        assert!(state.pending_resume);
    }

    #[test]
    fn invalid_session_non_resumable_requests_fresh_identify() {
        let mut state = GatewayState::new("t");
        // Pretend we had a live session that Discord now invalidates.
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &ready(9));

        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_INVALID_SESSION, "d": false }).to_string(),
        );
        assert_eq!(actions, vec![Action::Reconnect { resume: false }]);
        assert!(!state.pending_resume);
        assert_eq!(state.session_id, None);
    }

    #[test]
    fn invalid_session_resumable_requests_resume() {
        let mut state = GatewayState::new("t");
        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_INVALID_SESSION, "d": true }).to_string(),
        );
        assert_eq!(actions, vec![Action::Reconnect { resume: true }]);
        assert!(state.pending_resume);
    }

    #[test]
    fn hello_after_reconnect_sends_resume_frame() {
        let mut state = GatewayState::new("bot-token");
        on_frame(&mut state, &hello(41250));
        on_frame(&mut state, &ready(3));
        // Discord asks us to reconnect; we keep the session and plan a resume.
        on_frame(
            &mut state,
            &json!({ "op": OP_RECONNECT, "d": null }).to_string(),
        );
        assert!(state.pending_resume);

        // On the fresh connection's HELLO we RESUME rather than IDENTIFY.
        let actions = on_frame(&mut state, &hello(41250));
        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_RESUME));
        assert_eq!(frame["d"]["token"].as_str(), Some("bot-token"));
        assert_eq!(frame["d"]["session_id"].as_str(), Some("sess-abc"));
        assert_eq!(frame["d"]["seq"].as_u64(), Some(3));
        assert_eq!(state.phase, GatewayPhase::Resuming);
        assert!(!state.pending_resume);
    }
}
