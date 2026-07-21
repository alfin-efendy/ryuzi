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
use std::collections::HashMap;

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
    Reconnect {
        resume: bool,
    },
    /// Defer a slash-command interaction (Discord's 3-second ack) via its
    /// interaction `token`, BEFORE the paired `EmitInbound(slash.*)` action is
    /// acted on (Task 10 sends the REST defer, then routes the event).
    DeferInteraction {
        token: String,
    },
    /// An authorized approval-button click: edit the original message (via
    /// Discord's `UpdateMessage` interaction-response, keyed by the button
    /// click's OWN interaction `token` — no message id needed) to `text`.
    /// Paired with an `EmitInbound(approval.decision)` action.
    EditInteractionMessage {
        token: String,
        text: String,
    },
    /// An unauthorized approval-button click: reply ephemerally via the
    /// click's interaction `token`. No decision is emitted.
    ReplyEphemeral {
        token: String,
        text: String,
    },
}

/// The pending state of one outstanding approval request, keyed by
/// `request_id` — populated by the guest from a `deliver-outbound`
/// `approval-request` event (Task 10) before Discord's buttons are even
/// posted, so a click can be authorized purely against this map without any
/// host call. `approver_role_ids` + `started_by` are exactly the two fields
/// [`can_approve`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub approver_role_ids: Vec<String>,
    pub started_by: Option<String>,
}

/// `request_id -> PendingApproval`, threaded into [`on_frame`]/[`handle_interaction`]
/// as a parameter (never owned or mutated here) — see [`PendingApproval`]'s doc.
pub type PendingApprovals = HashMap<String, PendingApproval>;

/// The result of routing one Discord `INTERACTION_CREATE` dispatch, computed
/// purely by [`handle_interaction`]. `token` fields are the interaction's OWN
/// token (distinct per interaction, even for two clicks on the same
/// approval message) — the guest (Task 10) uses it for the deferred slash
/// reply / message edit / ephemeral reply respectively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionOutcome {
    /// A slash command (`/connect`, `/end`, `/stop`, `/status`): the guest
    /// must defer (when `defer` is true — always, today) then route `event`.
    Slash {
        defer: bool,
        token: String,
        event: InboundEvent,
    },
    /// An authorized approval-button click: the decision to emit plus the
    /// text to edit the original message to.
    Approval {
        request_id: String,
        allow: bool,
        actor: String,
        edit: String,
        token: String,
    },
    /// An approval-button click by a clicker `can_approve` rejects: no
    /// decision is emitted, only an ephemeral reply.
    Unauthorized { token: String, ephemeral: String },
    /// Anything else this component doesn't act on: an unknown interaction
    /// type, an unrecognized slash-command name, a malformed/unrecognized
    /// button `custom_id`, or a button click for a `request_id` with no
    /// entry in `pending` (already resolved/expired/unknown). Never panics.
    Ignored,
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
///
/// `is_thread` is only consulted for a `MESSAGE_CREATE` dispatch (every other
/// frame ignores it): whether the message's channel is a thread, per Discord's
/// channel-type classification. The real answer requires a REST
/// `GET /channels/{id}` round trip, which is the guest's job (wired in Task
/// 10) — this function stays pure by taking the pre-resolved answer as a
/// parameter instead of performing the lookup itself.
///
/// `pending` is only consulted for an `INTERACTION_CREATE` dispatch that
/// turns out to be an approval-button click — see [`PendingApprovals`]'s doc.
pub fn on_frame(
    state: &mut GatewayState,
    raw_json: &str,
    is_thread: bool,
    pending: &PendingApprovals,
) -> Vec<Action> {
    let value: Value = match serde_json::from_str(raw_json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    // Discord stamps the monotonic sequence `s` on DISPATCH frames only (null
    // otherwise). Capture it faithfully: it is the host-facing `sequence` for
    // emitted `message.*` events (Task 8 — stamped from `last_seq` below, NOT
    // a fresh counter, so dedup survives a RESUME replay) and the value
    // carried in every heartbeat + RESUME. `as_u64` is `None` for a null `s`,
    // so non-dispatch frames leave it untouched.
    if let Some(seq) = value.get("s").and_then(Value::as_u64) {
        state.last_seq = Some(seq);
    }

    let op = match value.get("op").and_then(Value::as_u64) {
        Some(op) => op,
        None => return Vec::new(),
    };

    match op {
        OP_HELLO => on_hello(state, &value),
        OP_DISPATCH => on_dispatch(state, &value, is_thread, pending),
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
/// drive the phase + session capture; MESSAGE_CREATE normalizes into an
/// inbound `message.*` `gateway-event` (Task 8, stamped with the dispatch's
/// `last_seq`); INTERACTION_CREATE routes through [`handle_interaction`]
/// (Task 9) into a slash `EmitInbound` (+ a paired `DeferInteraction`), an
/// authorized approval-button `EmitInbound(approval.decision)` (+ a paired
/// `EditInteractionMessage`), or an unauthorized click's lone
/// `ReplyEphemeral` — see [`interaction_actions`].
fn on_dispatch(
    state: &mut GatewayState,
    value: &Value,
    is_thread: bool,
    pending: &PendingApprovals,
) -> Vec<Action> {
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
        "MESSAGE_CREATE" => {
            let Some(data) = value.get("d") else {
                return Vec::new();
            };
            let bot_user_id = state.bot_user_id.as_deref().unwrap_or("");
            // `last_seq` was already updated (from this same frame's `s`) by
            // `on_frame` before this call — the exact Discord sequence, not a
            // fresh counter (see `on_frame`'s doc).
            let sequence = state.last_seq.unwrap_or(0);
            match normalize_message(data, bot_user_id, is_thread, sequence) {
                Some(event) => vec![Action::EmitInbound(event)],
                None => Vec::new(),
            }
        }
        "INTERACTION_CREATE" => {
            let Some(data) = value.get("d") else {
                return Vec::new();
            };
            interaction_actions(handle_interaction(data, pending))
        }
        _ => Vec::new(),
    }
}

/// Normalize a raw Discord `MESSAGE_CREATE` dispatch's `d` payload into the
/// host-facing inbound `gateway-event`, reproducing native
/// `InboundRouting::handle_message`'s routing rules
/// (`crates/core/src/gateway/discord/mod.rs:255-312`) exactly, IN ORDER:
///
/// 1. A bot-authored message (including a self-loop) is ignored.
/// 2. A DM (`guild_id` absent) with non-empty text becomes `message.dm`; an
///    attachment-only DM (empty text) is dropped.
/// 3. A thread reply with non-empty content OR at least one attachment
///    becomes `message.thread`; an empty thread message is dropped.
/// 4. A channel message that mentions the bot has the mention stripped
///    (`strip_mentions`); if the stripped prompt is non-empty OR there is at
///    least one attachment it becomes `message.mention`, else it's dropped.
/// 5. Anything else (a bare channel message with no bot mention) is dropped.
///
/// `bot_user_id` is the bot's own id (from READY, tracked in
/// [`GatewayState`]) — an empty string (bot id not yet known) never matches
/// any mention, same as native's `bot_user_id.is_some_and(..)`. `is_thread`
/// is the guest's pre-resolved channel-type classification (see [`on_frame`]'s
/// doc). `sequence` is the dispatch's Discord gateway `s`, stamped verbatim
/// onto the emitted event (design §5.2's RESUME-stable dedup contract) — the
/// caller, not this function, threads it through from [`GatewayState::last_seq`].
fn normalize_message(
    raw: &Value,
    bot_user_id: &str,
    is_thread: bool,
    sequence: u64,
) -> Option<InboundEvent> {
    let author_bot = raw
        .get("author")
        .and_then(|a| a.get("bot"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if author_bot {
        return None;
    }

    // Serenity leaves `guild_id` unset (missing or null) on a DM channel —
    // mirror that here regardless of whether the key is absent or explicitly
    // `null`.
    let is_dm = raw.get("guild_id").map(Value::is_null).unwrap_or(true);
    let author_id = raw
        .get("author")
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let channel_id = raw
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = raw
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let attachments: Vec<String> = raw
        .get("attachments")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| a.get("url").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mentions_bot = raw
        .get("mentions")
        .and_then(Value::as_array)
        .is_some_and(|mentions| {
            mentions
                .iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some(bot_user_id))
        });

    if is_dm {
        if content.is_empty() {
            return None; // Attachment-only DM: dropped, matches native.
        }
        return Some(build_event(
            "message.dm",
            json!({
                "conversation_id": channel_id,
                "user_id": author_id,
                "text": content,
            }),
            sequence,
        ));
    }

    if is_thread {
        if content.is_empty() && attachments.is_empty() {
            return None;
        }
        return Some(build_event(
            "message.thread",
            json!({
                "conversation_id": channel_id,
                "actor": author_id,
                "prompt": content,
                "attachments": attachments,
            }),
            sequence,
        ));
    }

    if mentions_bot {
        let prompt = strip_mentions(content);
        if prompt.is_empty() && attachments.is_empty() {
            return None; // Bare mention, no text/attachment: dropped.
        }
        return Some(build_event(
            "message.mention",
            json!({
                "workspace_id": channel_id,
                "actor": author_id,
                "prompt": prompt,
                "attachments": attachments,
            }),
            sequence,
        ));
    }

    None // Bare channel message, no mention: dropped.
}

/// Build an [`InboundEvent`] from an event-type string + JSON payload value.
fn build_event(event_type: &str, payload: Value, sequence: u64) -> InboundEvent {
    InboundEvent {
        event_type: event_type.to_string(),
        payload: payload.to_string().into_bytes(),
        sequence,
    }
}

/// Removes user mentions from message content, then trims the result.
/// `<@!?\d+>` hand-rolled (no `regex` dependency in this crate, matching the
/// native implementation's own reasoning) — a literal `<@`, an optional `!`,
/// one-or-more ASCII digits, then `>`. Operates on `char`s (not bytes) so it's
/// correct on non-ASCII content; only ASCII digits count (via
/// `char::is_ascii_digit`). A **role** mention (`<@&id>`) has `&` where a
/// digit or `!` is required, so it never matches and is left untouched — only
/// the final trim can affect it. Ports
/// `crates/core/src/gateway/discord/mod.rs`'s `strip_mentions` verbatim —
/// every user mention is stripped, not just the bot's own, since a message
/// can carry other user mentions too.
fn strip_mentions(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < n {
        if chars[i] == '<' && i + 1 < n && chars[i + 1] == '@' {
            let mut j = i + 2;
            if j < n && chars[j] == '!' {
                j += 1;
            }
            let digits_start = j;
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start && j < n && chars[j] == '>' {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out.trim().to_string()
}

/// Plain Discord application-command JSON (a valid REST body; no `serenity`
/// import), reproducing `crates/core/src/gateway/discord/mod.rs::build_commands`
/// verbatim: `/connect` (`name`/`git`/`model`/`effort`/`mode`, `mode` a
/// 3-choice string enum), `/end`, `/stop`, `/status`. Option `"type": 3` is
/// Discord's `ApplicationCommandOptionType.String`, inlined as the literal
/// `3`. Registered via REST in Task 10 — this only produces the
/// command-registration JSON.
pub fn build_commands() -> Value {
    json!([
        {
            "name": "connect",
            "description": "Connect a repo (new folder by name, or clone a git URL) to a new channel",
            "options": [
                { "name": "name", "description": "New project folder name", "type": 3, "required": false },
                { "name": "git", "description": "Git URL to clone", "type": 3, "required": false },
                { "name": "model", "description": "Model override", "type": 3, "required": false },
                { "name": "effort", "description": "Reasoning effort", "type": 3, "required": false },
                {
                    "name": "mode",
                    "description": "Permission mode",
                    "type": 3,
                    "required": false,
                    "choices": [
                        { "name": "default", "value": "default" },
                        { "name": "acceptEdits", "value": "acceptEdits" },
                        { "name": "bypassPermissions", "value": "bypassPermissions" }
                    ]
                }
            ]
        },
        { "name": "end", "description": "End the session in this thread (removes its worktree)" },
        { "name": "stop", "description": "Stop the running turn in this thread" },
        { "name": "status", "description": "Show harness status" }
    ])
}

/// Whether a clicker may approve a tool. Ported verbatim from
/// `crates/core/src/policy.rs::can_approve`: the session starter always may.
/// If NO approver roles are configured, only the starter may approve
/// (safe-by-default). Otherwise the clicker must hold one of the approver
/// roles.
pub fn can_approve(
    clicker_role_ids: &[String],
    approver_role_ids: &[String],
    is_starter: bool,
) -> bool {
    if is_starter {
        return true;
    }
    if approver_role_ids.is_empty() {
        return false;
    }
    clicker_role_ids
        .iter()
        .any(|r| approver_role_ids.contains(r))
}

/// Discord interaction types this component acts on (the rest are ignored).
const INTERACTION_TYPE_APPLICATION_COMMAND: u64 = 2;
const INTERACTION_TYPE_MESSAGE_COMPONENT: u64 = 3;

/// Route one raw Discord `INTERACTION_CREATE` dispatch payload (the frame's
/// `d` object) into an [`InteractionOutcome`], reproducing native
/// `InboundRouting::handle_interaction` (slash routing,
/// `crates/core/src/gateway/discord/mod.rs:318-368`) and
/// `SerenityDiscordPort::request_approval`'s button-click authorization
/// (`crates/core/src/gateway/discord/serenity_port.rs:467-568`). A malformed
/// or unrecognized interaction (unknown `type`, unknown slash-command name,
/// unparseable `custom_id`, or a button click for a `request_id` absent from
/// `pending`) returns [`InteractionOutcome::Ignored`] — never panics.
pub fn handle_interaction(raw: &Value, pending: &PendingApprovals) -> InteractionOutcome {
    match raw.get("type").and_then(Value::as_u64) {
        Some(INTERACTION_TYPE_APPLICATION_COMMAND) => handle_slash(raw),
        Some(INTERACTION_TYPE_MESSAGE_COMPONENT) => handle_button(raw, pending),
        _ => InteractionOutcome::Ignored,
    }
}

/// Extract the interacting user's id + role ids. Discord puts a guild
/// interaction's user under `member.user` (with `member.roles`); a DM
/// interaction (no `member`) puts it directly under `user` (with no roles —
/// DMs have no guild roles). Matches native's
/// `cmd.member.as_ref().map(|m| ...).unwrap_or_default()` role-id fallback.
fn interaction_user_and_roles(raw: &Value) -> (String, Vec<String>) {
    if let Some(member) = raw.get("member") {
        let user_id = member
            .get("user")
            .and_then(|u| u.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let role_ids = member
            .get("roles")
            .and_then(Value::as_array)
            .map(|roles| {
                roles
                    .iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (user_id, role_ids)
    } else {
        let user_id = raw
            .get("user")
            .and_then(|u| u.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        (user_id, Vec::new())
    }
}

/// Parse `data.options[{name,value}]` (Discord's chat-input command option
/// array shape) into a flat `name -> value` map, matching native's
/// `CommandDataOptionValue::String` extraction. Non-string option values
/// (none of `/connect`'s options are anything else) are simply absent.
fn parse_string_options(data: Option<&Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(options) = data
        .and_then(|d| d.get("options"))
        .and_then(Value::as_array)
    {
        for opt in options {
            if let (Some(name), Some(value)) = (
                opt.get("name").and_then(Value::as_str),
                opt.get("value").and_then(Value::as_str),
            ) {
                out.insert(name.to_string(), value.to_string());
            }
        }
    }
    out
}

/// A chat-input (`APPLICATION_COMMAND`) interaction: build the matching
/// `slash.*` inbound event, always paired with a defer (Discord's 3-second
/// ack — the guest must defer before the router resolves and replies, Task
/// 10). Wire fields verbatim (design §5.2): `slash.connect{token,user_id,
/// opts{name,git,model,effort,mode},role_ids}`, `slash.end{token,
/// conversation_id}`, `slash.stop{token,conversation_id}`,
/// `slash.status{token}`. An unrecognized command name is ignored.
fn handle_slash(raw: &Value) -> InteractionOutcome {
    let token = raw
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let channel_id = raw
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (user_id, role_ids) = interaction_user_and_roles(raw);
    let data = raw.get("data");
    let name = data
        .and_then(|d| d.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let options = parse_string_options(data);

    let (event_type, payload) = match name {
        "connect" => (
            "slash.connect",
            json!({
                "token": token,
                "user_id": user_id,
                "opts": {
                    "name": options.get("name"),
                    "git": options.get("git"),
                    "model": options.get("model"),
                    "effort": options.get("effort"),
                    "mode": options.get("mode"),
                },
                "role_ids": role_ids,
            }),
        ),
        "end" => (
            "slash.end",
            json!({ "token": token, "conversation_id": channel_id }),
        ),
        "stop" => (
            "slash.stop",
            json!({ "token": token, "conversation_id": channel_id }),
        ),
        "status" => ("slash.status", json!({ "token": token })),
        _ => return InteractionOutcome::Ignored,
    };

    InteractionOutcome::Slash {
        defer: true,
        token: token.clone(),
        event: build_event(event_type, payload, 0),
    }
}

/// `"{request_id}:approve"` / `"{request_id}:deny"` custom-id parsing —
/// matches native's `custom_id.ends_with(":approve"/":deny")` (the
/// `request_id` may itself contain colons; only the KNOWN suffix is
/// stripped, matching native exactly rather than splitting on the first
/// colon). Anything else (native's "unexpected custom_id — ignore") returns
/// `None`.
fn parse_custom_id(custom_id: &str) -> Option<(&str, bool)> {
    if let Some(request_id) = custom_id.strip_suffix(":approve") {
        Some((request_id, true))
    } else if let Some(request_id) = custom_id.strip_suffix(":deny") {
        Some((request_id, false))
    } else {
        None
    }
}

/// A `MESSAGE_COMPONENT` interaction (an approval button click): parse the
/// `custom_id`, look up the pending approval, authorize via [`can_approve`]
/// exactly like native `request_approval`'s collector loop, and produce
/// either an authorized [`InteractionOutcome::Approval`] (edit text
/// `"{label} by <@{actor}>"`, `label` = "✅ Approved"/"🚫 Denied" — native
/// also appends `" — **{tool}**"`, omitted here since [`PendingApproval`]
/// deliberately carries only what [`can_approve`] needs, not the tool name;
/// a delegated simplification, not a fidelity gap in the authorization
/// logic itself) or an [`InteractionOutcome::Unauthorized`] ephemeral reply
/// (native's exact "You are not authorized to approve this." string). A
/// `request_id` absent from `pending` (already resolved, expired, or
/// unknown) is ignored, same as a malformed `custom_id`.
fn handle_button(raw: &Value, pending: &PendingApprovals) -> InteractionOutcome {
    let token = raw
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(custom_id) = raw
        .get("data")
        .and_then(|d| d.get("custom_id"))
        .and_then(Value::as_str)
    else {
        return InteractionOutcome::Ignored;
    };
    let Some((request_id, allow)) = parse_custom_id(custom_id) else {
        return InteractionOutcome::Ignored;
    };
    let Some(approval) = pending.get(request_id) else {
        return InteractionOutcome::Ignored;
    };

    let (actor, clicker_role_ids) = interaction_user_and_roles(raw);
    let is_starter = approval.started_by.as_deref() == Some(actor.as_str());
    if !can_approve(&clicker_role_ids, &approval.approver_role_ids, is_starter) {
        return InteractionOutcome::Unauthorized {
            token,
            ephemeral: "You are not authorized to approve this.".to_string(),
        };
    }

    let label = if allow { "✅ Approved" } else { "🚫 Denied" };
    InteractionOutcome::Approval {
        request_id: request_id.to_string(),
        allow,
        edit: format!("{label} by <@{actor}>"),
        actor,
        token,
    }
}

/// Convert an [`InteractionOutcome`] into the [`Action`]s the guest must
/// perform, in order — a defer always precedes its paired event; an
/// authorized decision's message edit always precedes its paired event
/// (mirroring native's straight-line "decision locked, then the
/// best-effort edit" sequencing, though here both are guest-performed
/// effects rather than one being this function's own await).
fn interaction_actions(outcome: InteractionOutcome) -> Vec<Action> {
    match outcome {
        InteractionOutcome::Slash {
            defer,
            token,
            event,
        } => {
            let mut actions = Vec::new();
            if defer {
                actions.push(Action::DeferInteraction { token });
            }
            actions.push(Action::EmitInbound(event));
            actions
        }
        InteractionOutcome::Approval {
            request_id,
            allow,
            actor,
            edit,
            token,
        } => vec![
            Action::EditInteractionMessage { token, text: edit },
            Action::EmitInbound(build_event(
                "approval.decision",
                json!({ "request_id": request_id, "allow": allow, "actor": actor }),
                0,
            )),
        ],
        InteractionOutcome::Unauthorized { token, ephemeral } => vec![Action::ReplyEphemeral {
            token,
            text: ephemeral,
        }],
        InteractionOutcome::Ignored => Vec::new(),
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
        let actions = on_frame(&mut state, &hello(41250), false, &no_pending());

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
    fn dispatch_updates_last_seq_and_drops_empty_message_create() {
        let mut state = GatewayState::new("t");
        // No `content`/`author`/`channel_id` at all: normalization treats the
        // missing content as empty — same as a DM with no text, dropped.
        let actions = on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 42, json!({})),
            false,
            &no_pending(),
        );
        assert_eq!(state.last_seq, Some(42));
        assert!(actions.is_empty());
    }

    #[test]
    fn ready_stores_session_and_bot_identity() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(1), false, &no_pending());

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
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 7, json!({})),
            false,
            &no_pending(),
        );

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
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 5, json!({})),
            false,
            &no_pending(),
        );

        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_HEARTBEAT, "d": null }).to_string(),
            false,
            &no_pending(),
        );
        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_HEARTBEAT));
        assert_eq!(frame["d"].as_u64(), Some(5));
    }

    #[test]
    fn heartbeat_ack_marks_connection_alive() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        assert_eq!(due_heartbeat(&mut state, 0), None); // arm
        assert!(due_heartbeat(&mut state, 41250).is_some()); // send -> awaiting ack
        assert!(!state.heartbeat_acked);

        on_frame(
            &mut state,
            &json!({ "op": OP_HEARTBEAT_ACK, "d": null }).to_string(),
            false,
            &no_pending(),
        );
        assert!(state.heartbeat_acked);
    }

    #[test]
    fn missed_heartbeat_ack_signals_reconnect() {
        let mut state = GatewayState::new("bot-token");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(4), false, &no_pending()); // live session, last_seq = 4
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
        let actions = on_frame(&mut state, &hello(41250), false, &no_pending());
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
            false,
            &no_pending(),
        );
        assert_eq!(actions, vec![Action::Reconnect { resume: true }]);
        assert!(state.pending_resume);
    }

    #[test]
    fn invalid_session_non_resumable_requests_fresh_identify() {
        let mut state = GatewayState::new("t");
        // Pretend we had a live session that Discord now invalidates.
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(9), false, &no_pending());

        let actions = on_frame(
            &mut state,
            &json!({ "op": OP_INVALID_SESSION, "d": false }).to_string(),
            false,
            &no_pending(),
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
            false,
            &no_pending(),
        );
        assert_eq!(actions, vec![Action::Reconnect { resume: true }]);
        assert!(state.pending_resume);
    }

    #[test]
    fn hello_after_reconnect_sends_resume_frame() {
        let mut state = GatewayState::new("bot-token");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(3), false, &no_pending());
        // Discord asks us to reconnect; we keep the session and plan a resume.
        on_frame(
            &mut state,
            &json!({ "op": OP_RECONNECT, "d": null }).to_string(),
            false,
            &no_pending(),
        );
        assert!(state.pending_resume);

        // On the fresh connection's HELLO we RESUME rather than IDENTIFY.
        let actions = on_frame(&mut state, &hello(41250), false, &no_pending());
        let frame = sent_frame(&actions);
        assert_eq!(frame["op"].as_u64(), Some(OP_RESUME));
        assert_eq!(frame["d"]["token"].as_str(), Some("bot-token"));
        assert_eq!(frame["d"]["session_id"].as_str(), Some("sess-abc"));
        assert_eq!(frame["d"]["seq"].as_u64(), Some(3));
        assert_eq!(state.phase, GatewayPhase::Resuming);
        assert!(!state.pending_resume);
    }

    // ---- Task 8: message normalization -----------------------------------

    /// A synthetic Discord `MESSAGE_CREATE` `d` payload. `guild_id: None`
    /// produces a DM (key omitted, matching a real DM payload); `Some(id)`
    /// produces a guild channel message.
    fn message_create(
        channel_id: &str,
        guild_id: Option<&str>,
        author_id: &str,
        author_bot: bool,
        content: &str,
        mention_ids: &[&str],
        attachment_urls: &[&str],
    ) -> Value {
        let mut data = json!({
            "channel_id": channel_id,
            "author": { "id": author_id, "bot": author_bot },
            "content": content,
            "mentions": mention_ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>(),
            "attachments": attachment_urls
                .iter()
                .map(|url| json!({ "url": url }))
                .collect::<Vec<_>>(),
        });
        if let Some(guild_id) = guild_id {
            data["guild_id"] = json!(guild_id);
        }
        data
    }

    /// Extract the single `EmitInbound` event from a set of actions.
    fn emitted_event(actions: &[Action]) -> InboundEvent {
        actions
            .iter()
            .find_map(|a| match a {
                Action::EmitInbound(event) => Some(event.clone()),
                _ => None,
            })
            .expect("expected an EmitInbound action")
    }

    fn payload_json(event: &InboundEvent) -> Value {
        serde_json::from_slice(&event.payload).expect("payload is valid JSON")
    }

    #[test]
    fn normalize_message_drops_bot_authored() {
        let raw = message_create("10", Some("99"), "1", true, "hi", &[], &[]);
        assert_eq!(normalize_message(&raw, "bot-id", false, 5), None);
    }

    #[test]
    fn normalize_message_dm_with_content_routes_to_dm() {
        let raw = message_create("20", None, "7", false, "hello there", &[], &[]);
        let event = normalize_message(&raw, "bot-id", false, 42).expect("dm routed");
        assert_eq!(event.event_type, "message.dm");
        assert_eq!(event.sequence, 42);
        assert_eq!(
            payload_json(&event),
            json!({
                "conversation_id": "20",
                "user_id": "7",
                "text": "hello there",
            })
        );
    }

    #[test]
    fn normalize_message_dm_attachment_only_is_dropped() {
        let raw = message_create(
            "20",
            None,
            "7",
            false,
            "",
            &[],
            &["https://cdn.discordapp.com/f.png"],
        );
        assert_eq!(normalize_message(&raw, "bot-id", false, 1), None);
    }

    #[test]
    fn normalize_message_thread_reply_with_content_routes_to_thread() {
        let raw = message_create(
            "30",
            Some("99"),
            "8",
            false,
            "continuing the task",
            &[],
            &[],
        );
        let event = normalize_message(&raw, "bot-id", true, 7).expect("thread routed");
        assert_eq!(event.event_type, "message.thread");
        assert_eq!(event.sequence, 7);
        assert_eq!(
            payload_json(&event),
            json!({
                "conversation_id": "30",
                "actor": "8",
                "prompt": "continuing the task",
                "attachments": Vec::<String>::new(),
            })
        );
    }

    #[test]
    fn normalize_message_thread_attachment_only_routes_to_thread() {
        let raw = message_create(
            "30",
            Some("99"),
            "8",
            false,
            "",
            &[],
            &["https://cdn.discordapp.com/plan.pdf"],
        );
        let event = normalize_message(&raw, "bot-id", true, 8).expect("thread routed");
        assert_eq!(event.event_type, "message.thread");
        assert_eq!(
            payload_json(&event),
            json!({
                "conversation_id": "30",
                "actor": "8",
                "prompt": "",
                "attachments": ["https://cdn.discordapp.com/plan.pdf"],
            })
        );
    }

    #[test]
    fn normalize_message_channel_mention_strips_mention_and_routes() {
        let raw = message_create(
            "40",
            Some("99"),
            "9",
            false,
            "<@111222333> please build the widget",
            &["111222333"],
            &[],
        );
        let event = normalize_message(&raw, "111222333", false, 11).expect("mention routed");
        assert_eq!(event.event_type, "message.mention");
        assert_eq!(event.sequence, 11);
        assert_eq!(
            payload_json(&event),
            json!({
                "workspace_id": "40",
                "actor": "9",
                "prompt": "please build the widget",
                "attachments": Vec::<String>::new(),
            })
        );
    }

    #[test]
    fn normalize_message_channel_mention_empty_after_strip_no_attachment_is_dropped() {
        let raw = message_create(
            "40",
            Some("99"),
            "9",
            false,
            "<@111222333>",
            &["111222333"],
            &[],
        );
        assert_eq!(normalize_message(&raw, "111222333", false, 1), None);
    }

    #[test]
    fn normalize_message_bare_channel_message_no_mention_is_dropped() {
        let raw = message_create("40", Some("99"), "9", false, "just chatting", &[], &[]);
        assert_eq!(normalize_message(&raw, "111222333", false, 1), None);
    }

    #[test]
    fn on_frame_wires_message_create_to_mention_event_with_dispatch_sequence() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(1), false, &no_pending()); // bot_user_id == "111222333"

        let data = message_create(
            "40",
            Some("99"),
            "9",
            false,
            "<@111222333> ship it",
            &["111222333"],
            &[],
        );
        let actions = on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 99, data),
            false,
            &no_pending(),
        );

        let event = emitted_event(&actions);
        assert_eq!(event.event_type, "message.mention");
        // The emitted sequence is Discord's dispatch `s` (99), NOT a fresh
        // return-order counter — required for RESUME-stable dedup.
        assert_eq!(event.sequence, 99);
        assert_eq!(
            payload_json(&event),
            json!({
                "workspace_id": "40",
                "actor": "9",
                "prompt": "ship it",
                "attachments": Vec::<String>::new(),
            })
        );
    }

    #[test]
    fn on_frame_wires_message_create_to_thread_event_when_is_thread_true() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(1), false, &no_pending());

        let data = message_create("77", Some("99"), "5", false, "still working", &[], &[]);
        let actions = on_frame(
            &mut state,
            &dispatch("MESSAGE_CREATE", 123, data),
            true,
            &no_pending(),
        );

        let event = emitted_event(&actions);
        assert_eq!(event.event_type, "message.thread");
        assert_eq!(event.sequence, 123);
        assert_eq!(
            payload_json(&event),
            json!({
                "conversation_id": "77",
                "actor": "5",
                "prompt": "still working",
                "attachments": Vec::<String>::new(),
            })
        );
    }

    #[test]
    fn strip_mentions_removes_user_mention_but_leaves_role_mention() {
        assert_eq!(strip_mentions("<@123> hi <@!456> there"), "hi  there");
        // A role mention (`&`) never matches the user-mention pattern.
        assert_eq!(strip_mentions("<@&789> team ping"), "<@&789> team ping");
    }

    // ---- Task 9: slash commands + approval-button handling ---------------

    fn no_pending() -> PendingApprovals {
        PendingApprovals::new()
    }

    fn pending_with(
        request_id: &str,
        approver_role_ids: &[&str],
        started_by: Option<&str>,
    ) -> PendingApprovals {
        let mut map = PendingApprovals::new();
        map.insert(
            request_id.to_string(),
            PendingApproval {
                approver_role_ids: approver_role_ids.iter().map(|s| s.to_string()).collect(),
                started_by: started_by.map(str::to_string),
            },
        );
        map
    }

    /// A synthetic APPLICATION_COMMAND (`type: 2`) interaction, matching
    /// Discord's wire shape: `token` (top-level), `channel_id`,
    /// `member.user.id` + `member.roles`, `data.name` +
    /// `data.options[{name,value}]`.
    fn slash_interaction(
        name: &str,
        token: &str,
        channel_id: &str,
        user_id: &str,
        role_ids: &[&str],
        options: &[(&str, &str)],
    ) -> Value {
        json!({
            "type": 2,
            "token": token,
            "channel_id": channel_id,
            "member": {
                "user": { "id": user_id },
                "roles": role_ids,
            },
            "data": {
                "name": name,
                "options": options
                    .iter()
                    .map(|(n, v)| json!({ "name": n, "value": v }))
                    .collect::<Vec<_>>(),
            },
        })
    }

    /// A synthetic MESSAGE_COMPONENT (`type: 3`) interaction (an approval
    /// button click).
    fn button_interaction(
        token: &str,
        channel_id: &str,
        custom_id: &str,
        user_id: &str,
        role_ids: &[&str],
    ) -> Value {
        json!({
            "type": 3,
            "token": token,
            "channel_id": channel_id,
            "member": {
                "user": { "id": user_id },
                "roles": role_ids,
            },
            "data": { "custom_id": custom_id },
        })
    }

    // ---------- build_commands ----------

    #[test]
    fn build_commands_defines_connect_end_stop_status() {
        let names: Vec<String> = build_commands()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        for expected in ["connect", "end", "stop", "status"] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn build_commands_connect_has_expected_options_and_mode_choices() {
        let commands = build_commands();
        let connect = commands
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "connect")
            .unwrap();
        let opts = connect["options"].as_array().unwrap();
        let opt_names: Vec<String> = opts
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_string())
            .collect();
        for expected in ["name", "git", "model", "effort", "mode"] {
            assert!(
                opt_names.contains(&expected.to_string()),
                "missing {expected}: {opt_names:?}"
            );
        }
        let mode = opts.iter().find(|o| o["name"] == "mode").unwrap();
        let values: Vec<String> = mode["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["value"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(values, vec!["default", "acceptEdits", "bypassPermissions"]);
    }

    // ---------- can_approve ----------

    #[test]
    fn can_approve_starter_always_wins() {
        assert!(can_approve(&[], &[], true));
    }

    #[test]
    fn can_approve_empty_approver_list_means_starter_only() {
        assert!(!can_approve(&[], &[], false));
    }

    #[test]
    fn can_approve_role_intersection() {
        let approver = vec!["r1".to_string()];
        assert!(can_approve(&["r1".to_string()], &approver, false));
        assert!(!can_approve(&["r2".to_string()], &approver, false));
    }

    // ---------- handle_interaction: slash commands ----------

    #[test]
    fn handle_interaction_connect_emits_event_with_token_opts_and_role_ids() {
        let raw = slash_interaction(
            "connect",
            "tok-1",
            "chan-1",
            "u1",
            &["r1", "r2"],
            &[
                ("name", "myproj"),
                ("git", "https://example.com/x.git"),
                ("model", "opus"),
                ("effort", "high"),
                ("mode", "acceptEdits"),
            ],
        );
        let outcome = handle_interaction(&raw, &no_pending());
        let InteractionOutcome::Slash {
            defer,
            token,
            event,
        } = outcome
        else {
            panic!("expected Slash outcome");
        };
        assert!(defer);
        assert_eq!(token, "tok-1");
        assert_eq!(event.event_type, "slash.connect");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            payload,
            json!({
                "token": "tok-1",
                "user_id": "u1",
                "opts": {
                    "name": "myproj",
                    "git": "https://example.com/x.git",
                    "model": "opus",
                    "effort": "high",
                    "mode": "acceptEdits",
                },
                "role_ids": ["r1", "r2"],
            })
        );
    }

    #[test]
    fn handle_interaction_connect_missing_options_are_null() {
        let raw = slash_interaction("connect", "tok-2", "chan-1", "u1", &[], &[]);
        let outcome = handle_interaction(&raw, &no_pending());
        let InteractionOutcome::Slash { event, .. } = outcome else {
            panic!("expected Slash outcome");
        };
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            payload["opts"],
            json!({
                "name": null,
                "git": null,
                "model": null,
                "effort": null,
                "mode": null,
            })
        );
    }

    #[test]
    fn handle_interaction_end_emits_token_and_conversation_id() {
        let raw = slash_interaction("end", "tok-3", "chan-9", "u1", &[], &[]);
        let outcome = handle_interaction(&raw, &no_pending());
        let InteractionOutcome::Slash {
            defer,
            token,
            event,
        } = outcome
        else {
            panic!("expected Slash outcome");
        };
        assert!(defer);
        assert_eq!(token, "tok-3");
        assert_eq!(event.event_type, "slash.end");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            payload,
            json!({ "token": "tok-3", "conversation_id": "chan-9" })
        );
    }

    #[test]
    fn handle_interaction_stop_emits_token_and_conversation_id() {
        let raw = slash_interaction("stop", "tok-4", "chan-9", "u1", &[], &[]);
        let outcome = handle_interaction(&raw, &no_pending());
        let InteractionOutcome::Slash { event, .. } = outcome else {
            panic!("expected Slash outcome");
        };
        assert_eq!(event.event_type, "slash.stop");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            payload,
            json!({ "token": "tok-4", "conversation_id": "chan-9" })
        );
    }

    #[test]
    fn handle_interaction_status_emits_token_only() {
        let raw = slash_interaction("status", "tok-5", "chan-1", "u1", &[], &[]);
        let outcome = handle_interaction(&raw, &no_pending());
        let InteractionOutcome::Slash { event, .. } = outcome else {
            panic!("expected Slash outcome");
        };
        assert_eq!(event.event_type, "slash.status");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(payload, json!({ "token": "tok-5" }));
    }

    #[test]
    fn handle_interaction_unknown_command_name_is_ignored() {
        let raw = slash_interaction("bogus", "tok-6", "chan-1", "u1", &[], &[]);
        assert_eq!(
            handle_interaction(&raw, &no_pending()),
            InteractionOutcome::Ignored
        );
    }

    // ---------- handle_interaction: approval buttons ----------

    #[test]
    fn handle_interaction_button_approve_by_authorized_role_emits_decision() {
        let pending = pending_with("req-1", &["approver"], Some("starter-1"));
        let raw = button_interaction(
            "tok-btn-1",
            "chan-1",
            "req-1:approve",
            "clicker-1",
            &["approver"],
        );
        let outcome = handle_interaction(&raw, &pending);
        let InteractionOutcome::Approval {
            request_id,
            allow,
            actor,
            edit,
            token,
        } = outcome
        else {
            panic!("expected Approval outcome");
        };
        assert_eq!(request_id, "req-1");
        assert!(allow);
        assert_eq!(actor, "clicker-1");
        assert_eq!(token, "tok-btn-1");
        assert_eq!(edit, "✅ Approved by <@clicker-1>");
    }

    #[test]
    fn handle_interaction_button_deny_by_authorized_role_emits_decision_false() {
        let pending = pending_with("req-1", &["approver"], Some("starter-1"));
        let raw = button_interaction(
            "tok-btn-2",
            "chan-1",
            "req-1:deny",
            "clicker-1",
            &["approver"],
        );
        let outcome = handle_interaction(&raw, &pending);
        let InteractionOutcome::Approval { allow, edit, .. } = outcome else {
            panic!("expected Approval outcome");
        };
        assert!(!allow);
        assert_eq!(edit, "🚫 Denied by <@clicker-1>");
    }

    #[test]
    fn handle_interaction_button_starter_is_always_allowed() {
        // Empty approver_role_ids would deny anyone else, but the starter
        // clicking their own request is always authorized.
        let pending = pending_with("req-2", &[], Some("starter-9"));
        let raw = button_interaction("tok-btn-3", "chan-1", "req-2:approve", "starter-9", &[]);
        let outcome = handle_interaction(&raw, &pending);
        assert!(matches!(
            outcome,
            InteractionOutcome::Approval { allow: true, .. }
        ));
    }

    #[test]
    fn handle_interaction_button_unauthorized_click_denies_no_decision() {
        let pending = pending_with("req-3", &["approver"], Some("starter-1"));
        let raw = button_interaction(
            "tok-btn-4",
            "chan-1",
            "req-3:approve",
            "rando",
            &["not-approver"],
        );
        let outcome = handle_interaction(&raw, &pending);
        let InteractionOutcome::Unauthorized { token, ephemeral } = outcome else {
            panic!("expected Unauthorized outcome");
        };
        assert_eq!(token, "tok-btn-4");
        assert_eq!(ephemeral, "You are not authorized to approve this.");
    }

    #[test]
    fn handle_interaction_button_missing_pending_entry_is_ignored() {
        let raw = button_interaction(
            "tok-btn-5",
            "chan-1",
            "unknown-req:approve",
            "u1",
            &["approver"],
        );
        assert_eq!(
            handle_interaction(&raw, &no_pending()),
            InteractionOutcome::Ignored
        );
    }

    #[test]
    fn handle_interaction_button_bad_custom_id_is_ignored() {
        let pending = pending_with("req-1", &["approver"], Some("starter-1"));
        let raw = button_interaction("tok-btn-6", "chan-1", "req-1:maybe", "u1", &["approver"]);
        assert_eq!(
            handle_interaction(&raw, &pending),
            InteractionOutcome::Ignored
        );
    }

    #[test]
    fn handle_interaction_unknown_interaction_type_is_ignored() {
        let raw = json!({ "type": 99, "token": "t" });
        assert_eq!(
            handle_interaction(&raw, &no_pending()),
            InteractionOutcome::Ignored
        );
    }

    #[test]
    fn handle_interaction_missing_type_field_is_ignored_not_panicking() {
        let raw = json!({ "token": "t" });
        assert_eq!(
            handle_interaction(&raw, &no_pending()),
            InteractionOutcome::Ignored
        );
    }

    // ---------- on_frame wiring: INTERACTION_CREATE ----------

    #[test]
    fn on_frame_wires_interaction_create_slash_to_defer_and_emit() {
        let mut state = GatewayState::new("t");
        let raw = slash_interaction("status", "tok-7", "chan-1", "u1", &[], &[]);
        let frame = dispatch("INTERACTION_CREATE", 1, raw);
        let actions = on_frame(&mut state, &frame, false, &no_pending());

        assert!(actions.contains(&Action::DeferInteraction {
            token: "tok-7".to_string()
        }));
        let event = actions
            .iter()
            .find_map(|a| match a {
                Action::EmitInbound(e) => Some(e.clone()),
                _ => None,
            })
            .expect("expected an EmitInbound action");
        assert_eq!(event.event_type, "slash.status");
    }

    #[test]
    fn on_frame_wires_interaction_create_authorized_button_to_edit_and_emit() {
        let mut state = GatewayState::new("t");
        let pending = pending_with("req-9", &["approver"], Some("starter-1"));
        let raw = button_interaction(
            "tok-8",
            "chan-1",
            "req-9:approve",
            "clicker-1",
            &["approver"],
        );
        let frame = dispatch("INTERACTION_CREATE", 1, raw);
        let actions = on_frame(&mut state, &frame, false, &pending);

        assert!(actions.contains(&Action::EditInteractionMessage {
            token: "tok-8".to_string(),
            text: "✅ Approved by <@clicker-1>".to_string(),
        }));
        let event = actions
            .iter()
            .find_map(|a| match a {
                Action::EmitInbound(e) => Some(e.clone()),
                _ => None,
            })
            .expect("expected an EmitInbound action");
        assert_eq!(event.event_type, "approval.decision");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            payload,
            json!({ "request_id": "req-9", "allow": true, "actor": "clicker-1" })
        );
    }

    #[test]
    fn on_frame_wires_interaction_create_unauthorized_button_to_ephemeral_reply_only() {
        let mut state = GatewayState::new("t");
        let pending = pending_with("req-9", &["approver"], Some("starter-1"));
        let raw = button_interaction("tok-9", "chan-1", "req-9:approve", "rando", &[]);
        let frame = dispatch("INTERACTION_CREATE", 1, raw);
        let actions = on_frame(&mut state, &frame, false, &pending);

        assert_eq!(
            actions,
            vec![Action::ReplyEphemeral {
                token: "tok-9".to_string(),
                text: "You are not authorized to approve this.".to_string(),
            }]
        );
    }
}
