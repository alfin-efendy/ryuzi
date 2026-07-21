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
    /// interaction `id` + `token`, BEFORE the paired `EmitInbound(slash.*)`
    /// action is acted on (Task 10 POSTs the REST `type:5` deferred callback to
    /// `/interactions/{id}/{token}/callback`, then routes the event).
    DeferInteraction {
        id: String,
        token: String,
    },
    /// An authorized approval-button click: edit the original message (via
    /// Discord's `UpdateMessage` interaction-response `type:7`, keyed by the
    /// button click's OWN interaction `id` + `token` — no message id needed)
    /// to `text`. Paired with an `EmitInbound(approval.decision)` action.
    EditInteractionMessage {
        id: String,
        token: String,
        text: String,
    },
    /// An unauthorized approval-button click: reply ephemerally (`type:4`,
    /// `flags:64`) via the click's interaction `id` + `token`. No decision is
    /// emitted.
    ReplyEphemeral {
        id: String,
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
    /// The name of the tool being approved — carried so [`handle_button`]'s
    /// message edit can restore native's `" — **{tool}**"` suffix. The guest
    /// fills it from the `deliver-outbound(approval-request)` payload.
    pub tool: String,
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
    /// `id` is the interaction's own snowflake, needed alongside `token` for
    /// the REST `/interactions/{id}/{token}/callback` defer.
    Slash {
        defer: bool,
        id: String,
        token: String,
        event: InboundEvent,
    },
    /// An authorized approval-button click: the decision to emit plus the
    /// text to edit the original message to. `id`+`token` address the REST
    /// `UpdateMessage` callback.
    Approval {
        request_id: String,
        allow: bool,
        actor: String,
        edit: String,
        id: String,
        token: String,
    },
    /// An approval-button click by a clicker `can_approve` rejects: no
    /// decision is emitted, only an ephemeral reply (via `id`+`token`).
    Unauthorized {
        id: String,
        token: String,
        ephemeral: String,
    },
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

    /// Prepare the state for a guest-detected dropped socket (a `ws::poll`
    /// disconnect or a `closed`/`closing` `ws::state`, §6.1): resume if we still
    /// hold a session (arms a RESUME on the next HELLO), else re-IDENTIFY.
    /// Returns whether the guest should reconnect to the stored
    /// `resume_gateway_url` (`true`) or the base gateway (`false`). Mirrors the
    /// RECONNECT/INVALID_SESSION opcode paths, but triggered by the transport
    /// dropping rather than a gateway frame — so the guest need not wait ~41s
    /// for the heartbeat-blackout path to notice.
    pub fn plan_reconnect(&mut self) -> bool {
        if self.session_id.is_some() {
            self.pending_resume = true;
            self.phase = GatewayPhase::Resuming;
            true
        } else {
            self.pending_resume = false;
            self.phase = GatewayPhase::Connecting;
            false
        }
    }
}

/// Build the websocket URL to RESUME on: Discord's `resume_gateway_url` (from
/// READY) with the same `?v=10&encoding=json` query the base gateway uses
/// appended (the READY value is a bare `wss://host`, no query). Resuming to
/// this per-session host — NOT the base [`GATEWAY_URL`] — is required: the base
/// can land on a shard without the session and provoke an INVALID_SESSION.
pub fn resume_ws_url(resume_gateway_url: &str) -> String {
    format!(
        "{}/?v=10&encoding=json",
        resume_gateway_url.trim_end_matches('/')
    )
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
    let id = raw
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
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
        id,
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
/// `"{label} by <@{actor}> — **{tool}**"`, `label` = "✅ Approved"/"🚫 Denied",
/// `tool` from the [`PendingApproval`] — reproducing native
/// `request_approval`'s exact edit string) or an
/// [`InteractionOutcome::Unauthorized`] ephemeral reply (native's exact
/// "You are not authorized to approve this." string). A `request_id` absent
/// from `pending` (already resolved, expired, or unknown) is ignored, same as
/// a malformed `custom_id`.
fn handle_button(raw: &Value, pending: &PendingApprovals) -> InteractionOutcome {
    let id = raw
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
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
            id,
            token,
            ephemeral: "You are not authorized to approve this.".to_string(),
        };
    }

    let label = if allow { "✅ Approved" } else { "🚫 Denied" };
    InteractionOutcome::Approval {
        request_id: request_id.to_string(),
        allow,
        edit: format!("{label} by <@{actor}> — **{}**", approval.tool),
        actor,
        id,
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
            id,
            token,
            event,
        } => {
            let mut actions = Vec::new();
            if defer {
                actions.push(Action::DeferInteraction { id, token });
            }
            actions.push(Action::EmitInbound(event));
            actions
        }
        InteractionOutcome::Approval {
            request_id,
            allow,
            actor,
            edit,
            id,
            token,
        } => vec![
            Action::EditInteractionMessage {
                id,
                token,
                text: edit,
            },
            Action::EmitInbound(build_event(
                "approval.decision",
                json!({ "request_id": request_id, "allow": allow, "actor": actor }),
                0,
            )),
        ],
        InteractionOutcome::Unauthorized {
            id,
            token,
            ephemeral,
        } => vec![Action::ReplyEphemeral {
            id,
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

// =====================================================================
// Task 10: outbound Discord REST (pure request planning + response parsing)
// =====================================================================
//
// The guest (`guest.rs`) performs no Discord-specific decision-making: it
// decodes a `deliver-outbound` event into an [`OutboundKind`], asks
// [`plan_outbound`] for the exact [`RestRequest`]s to send over `ryuzi:http`,
// sends them, and asks [`outbound_op_result`] for the `op.result` inbound
// event (if any) to queue. Everything below is pure — no host handle, no
// network — so it is exercised natively with synthetic REST bodies/statuses.
//
// The bot token appears in exactly ONE place: the `Authorization: Bot {token}`
// header [`auth_headers`] stamps on every request. It is never placed in a
// URL, a body, or a log line (the `token_never_leaves_the_bot_auth_header`
// test enforces this across every op).

/// Discord REST base, v10 JSON (matches the native serenity `Http` base).
pub const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// The bot-token + surrounding ids the REST planners need. Borrowed, so the
/// guest can build one per call from its long-lived `GatewayState`/settings
/// without cloning the token around.
pub struct RestCtx<'a> {
    pub token: &'a str,
    pub app_id: &'a str,
    pub guild_id: &'a str,
}

/// A fully-planned Discord REST request the guest hands verbatim to
/// `ryuzi:http`. `body` is a JSON string (`None` for a bodyless GET/defer);
/// `headers` always carries the `Authorization: Bot {token}` pair, plus
/// `Content-Type: application/json` whenever there is a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// The correlated result body of one outbound op — the `op.result` payload
/// (design §5.3). Only the field relevant to the op kind is ever populated:
/// `create-channel` → `channel_id`, `create-thread` → `thread_id`,
/// `send-message` → `message_id`, `edit-message`/`send-messages` → `ok`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpResultBody {
    pub channel_id: Option<String>,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
    pub ok: Option<bool>,
}

/// A decoded `deliver-outbound` op (design §5.3). The component's own mirror of
/// the host bridge's `OutboundOp` (this crate can't depend on `crates/core`),
/// carrying only the fields the REST planners actually consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundKind {
    CreateChannel {
        op_id: String,
        name: String,
    },
    CreateThread {
        op_id: String,
        channel_id: String,
        title: String,
    },
    SendMessage {
        op_id: String,
        channel_id: String,
        text: String,
    },
    EditMessage {
        op_id: String,
        channel_id: String,
        message_id: String,
        text: String,
    },
    SendMessages {
        op_id: String,
        channel_id: String,
        chunks: Vec<String>,
    },
    ApprovalRequest {
        op_id: String,
        request_id: String,
        conversation_id: String,
        tool: String,
        summary: String,
        approver_role_ids: Vec<String>,
        started_by: Option<String>,
    },
    InteractionReply {
        token: String,
        text: String,
    },
}

impl OutboundKind {
    /// The op's correlation `op_id`, if it has one. `interaction-reply` has
    /// none (it edits a deferred slash response, uncorrelated).
    pub fn op_id(&self) -> Option<&str> {
        match self {
            OutboundKind::CreateChannel { op_id, .. }
            | OutboundKind::CreateThread { op_id, .. }
            | OutboundKind::SendMessage { op_id, .. }
            | OutboundKind::EditMessage { op_id, .. }
            | OutboundKind::SendMessages { op_id, .. }
            | OutboundKind::ApprovalRequest { op_id, .. } => Some(op_id),
            OutboundKind::InteractionReply { .. } => None,
        }
    }
}

/// Decode a `deliver-outbound` `gateway-event` (its `event-type` + flat JSON
/// `payload`, design §5.3) into an [`OutboundKind`]. `None` on an unknown
/// event-type or a payload missing a required field — the guest then rejects
/// the delivery rather than trapping.
pub fn decode_outbound(event_type: &str, payload: &[u8]) -> Option<OutboundKind> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let s = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    match event_type {
        "create-channel" => Some(OutboundKind::CreateChannel {
            op_id: s("op_id")?,
            name: s("name").unwrap_or_default(),
        }),
        "create-thread" => Some(OutboundKind::CreateThread {
            op_id: s("op_id")?,
            channel_id: s("channel_id")?,
            title: s("title").unwrap_or_default(),
        }),
        "send-message" => Some(OutboundKind::SendMessage {
            op_id: s("op_id")?,
            channel_id: s("channel_id")?,
            text: s("text").unwrap_or_default(),
        }),
        "edit-message" => Some(OutboundKind::EditMessage {
            op_id: s("op_id")?,
            channel_id: s("channel_id")?,
            message_id: s("message_id")?,
            text: s("text").unwrap_or_default(),
        }),
        "send-messages" => Some(OutboundKind::SendMessages {
            op_id: s("op_id")?,
            channel_id: s("channel_id")?,
            chunks: strings("chunks"),
        }),
        "approval-request" => Some(OutboundKind::ApprovalRequest {
            op_id: s("op_id")?,
            request_id: s("request_id")?,
            conversation_id: s("conversation_id")?,
            tool: s("tool").unwrap_or_default(),
            summary: s("summary").unwrap_or_default(),
            approver_role_ids: strings("approver_role_ids"),
            started_by: s("started_by"),
        }),
        "interaction-reply" => Some(OutboundKind::InteractionReply {
            token: s("token")?,
            text: s("text").unwrap_or_default(),
        }),
        _ => None,
    }
}

/// The shared REST-request builder: `https://discord.com/api/v10{path}` with
/// the `Authorization: Bot {token}` header (the ONLY place the token ever
/// appears) plus `Content-Type: application/json` whenever there is a body.
fn discord_request(ctx: &RestCtx, method: &str, path: &str, body: Option<Value>) -> RestRequest {
    let mut headers = vec![("Authorization".to_string(), format!("Bot {}", ctx.token))];
    let body = body.map(|value| {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        value.to_string()
    });
    RestRequest {
        method: method.to_string(),
        url: format!("{DISCORD_API_BASE}{path}"),
        headers,
        body,
    }
}

/// The Approve/Deny button message body — content + a single action row of two
/// buttons whose `custom_id`s are `"{request_id}:approve"` / `":deny"` (parsed
/// back by [`parse_custom_id`]). Component type 1 = action row, 2 = button;
/// styles 3 = success, 4 = danger (matches native `ButtonStyle::Success/Danger`).
fn approval_message_body(request_id: &str, tool: &str, summary: &str) -> Value {
    json!({
        "content": format!("🔐 Approve **{tool}**?\n```\n{summary}\n```"),
        "components": [{
            "type": 1,
            "components": [
                { "type": 2, "style": 3, "label": "Approve", "custom_id": format!("{request_id}:approve") },
                { "type": 2, "style": 4, "label": "Deny", "custom_id": format!("{request_id}:deny") },
            ],
        }],
    })
}

/// Plan the REST request(s) for one outbound op. Most ops map to a single
/// request; `send-messages` fans out to one POST per chunk (design §5.3).
/// `approval-request` posts the Approve/Deny button message;
/// `interaction-reply` edits the deferred slash response.
pub fn plan_outbound(op: &OutboundKind, ctx: &RestCtx) -> Vec<RestRequest> {
    match op {
        OutboundKind::CreateChannel { name, .. } => vec![discord_request(
            ctx,
            "POST",
            &format!("/guilds/{}/channels", ctx.guild_id),
            Some(json!({ "name": name, "type": 0 })),
        )],
        OutboundKind::CreateThread {
            channel_id, title, ..
        } => vec![discord_request(
            ctx,
            "POST",
            &format!("/channels/{channel_id}/threads"),
            Some(json!({ "name": title })),
        )],
        OutboundKind::SendMessage {
            channel_id, text, ..
        } => vec![discord_request(
            ctx,
            "POST",
            &format!("/channels/{channel_id}/messages"),
            Some(json!({ "content": text })),
        )],
        OutboundKind::EditMessage {
            channel_id,
            message_id,
            text,
            ..
        } => vec![discord_request(
            ctx,
            "PATCH",
            &format!("/channels/{channel_id}/messages/{message_id}"),
            Some(json!({ "content": text })),
        )],
        OutboundKind::SendMessages {
            channel_id, chunks, ..
        } => chunks
            .iter()
            .map(|chunk| {
                discord_request(
                    ctx,
                    "POST",
                    &format!("/channels/{channel_id}/messages"),
                    Some(json!({ "content": chunk })),
                )
            })
            .collect(),
        OutboundKind::ApprovalRequest {
            request_id,
            conversation_id,
            tool,
            summary,
            ..
        } => vec![discord_request(
            ctx,
            "POST",
            &format!("/channels/{conversation_id}/messages"),
            Some(approval_message_body(request_id, tool, summary)),
        )],
        OutboundKind::InteractionReply { token, text } => vec![discord_request(
            ctx,
            "PATCH",
            &format!("/webhooks/{}/{token}/messages/@original", ctx.app_id),
            Some(json!({ "content": text })),
        )],
    }
}

/// The `PUT /applications/{app_id}/guilds/{guild_id}/commands` guild-command
/// registration request (bulk overwrite with [`build_commands`]) the guest
/// sends once at start.
pub fn register_commands_request(ctx: &RestCtx) -> RestRequest {
    discord_request(
        ctx,
        "PUT",
        &format!(
            "/applications/{}/guilds/{}/commands",
            ctx.app_id, ctx.guild_id
        ),
        Some(build_commands()),
    )
}

/// The `GET /channels/{id}` request whose response [`is_thread_channel`]
/// classifies — the REST round-trip native `Handler::message` does to resolve
/// `is_thread` per inbound message.
pub fn channel_get_request(ctx: &RestCtx, channel_id: &str) -> RestRequest {
    discord_request(ctx, "GET", &format!("/channels/{channel_id}"), None)
}

/// The `POST /interactions/{id}/{token}/callback` deferred-response (`type:5`,
/// ephemeral `flags:64`) that acks a slash command within Discord's 3s window —
/// the REST equivalent of native `defer_ephemeral`.
pub fn defer_request(ctx: &RestCtx, interaction_id: &str, interaction_token: &str) -> RestRequest {
    discord_request(
        ctx,
        "POST",
        &format!("/interactions/{interaction_id}/{interaction_token}/callback"),
        Some(json!({ "type": 5, "data": { "flags": 64 } })),
    )
}

/// The `POST /interactions/{id}/{token}/callback` ephemeral message (`type:4`,
/// `flags:64`) that answers an unauthorized approval-button click.
pub fn ephemeral_reply_request(
    ctx: &RestCtx,
    interaction_id: &str,
    interaction_token: &str,
    text: &str,
) -> RestRequest {
    discord_request(
        ctx,
        "POST",
        &format!("/interactions/{interaction_id}/{interaction_token}/callback"),
        Some(json!({ "type": 4, "data": { "content": text, "flags": 64 } })),
    )
}

/// The `POST /interactions/{id}/{token}/callback` update-message (`type:7`,
/// clears the buttons) that rewrites an approval message after an authorized
/// decision — the REST equivalent of native's `UpdateMessage` response.
pub fn update_message_request(
    ctx: &RestCtx,
    interaction_id: &str,
    interaction_token: &str,
    text: &str,
) -> RestRequest {
    discord_request(
        ctx,
        "POST",
        &format!("/interactions/{interaction_id}/{interaction_token}/callback"),
        Some(json!({ "type": 7, "data": { "content": text, "components": [] } })),
    )
}

/// Classify a `GET /channels/{id}` response body: `true` iff the channel's
/// `type` is one of Discord's thread kinds — 10 (announcement),
/// 11 (public), 12 (private) — matching native's `PublicThread |
/// PrivateThread | NewsThread` check. A non-JSON / typeless body → `false`
/// (native's `_ => false`).
pub fn is_thread_channel(channel_body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(channel_body)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_u64))
        .is_some_and(|kind| matches!(kind, 10..=12))
}

/// If `raw_json` is a `MESSAGE_CREATE` dispatch, return its `d.channel_id` —
/// the channel the guest must `GET`/classify before calling [`on_frame`] with
/// the resolved `is_thread`. `None` for any other frame (no classification
/// needed).
pub fn message_create_channel_id(raw_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw_json).ok()?;
    if value.get("op").and_then(Value::as_u64) != Some(OP_DISPATCH) {
        return None;
    }
    if value.get("t").and_then(Value::as_str) != Some("MESSAGE_CREATE") {
        return None;
    }
    value
        .get("d")
        .and_then(|d| d.get("channel_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Parse one op's REST outcome into its [`OpResultBody`] (design §5.3):
/// id-returning ops (`create-channel`/`create-thread`/`send-message`) read
/// `body.id`; `edit-message`/`send-messages` report `ok` from a 2xx `status`.
/// `approval-request`/`interaction-reply` return `None` (they produce no
/// `op.result`).
pub fn parse_outbound(op: &OutboundKind, status: u16, body: &[u8]) -> Option<OpResultBody> {
    let ok = (200..300).contains(&status);
    match op {
        OutboundKind::CreateChannel { .. } => Some(OpResultBody {
            channel_id: extract_id(body),
            ..Default::default()
        }),
        OutboundKind::CreateThread { .. } => Some(OpResultBody {
            thread_id: extract_id(body),
            ..Default::default()
        }),
        OutboundKind::SendMessage { .. } => Some(OpResultBody {
            message_id: extract_id(body),
            ..Default::default()
        }),
        OutboundKind::EditMessage { .. } | OutboundKind::SendMessages { .. } => {
            Some(OpResultBody {
                ok: Some(ok),
                ..Default::default()
            })
        }
        OutboundKind::ApprovalRequest { .. } | OutboundKind::InteractionReply { .. } => None,
    }
}

/// Read Discord's `id` snowflake from a REST response body (a created
/// channel/thread/message all echo their new id). `None` on a non-JSON or
/// id-less body (e.g. a Discord error object) — the bridge then surfaces the
/// missing id as an op failure.
fn extract_id(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Build the flat `op.result` inbound event (payload `{op_id, <field>}`,
/// design §5.3) the guest queues for the next `poll-inbound`.
pub fn op_result_event(op_id: &str, body: &OpResultBody) -> InboundEvent {
    let mut payload = serde_json::Map::new();
    payload.insert("op_id".to_string(), json!(op_id));
    if let Some(channel_id) = &body.channel_id {
        payload.insert("channel_id".to_string(), json!(channel_id));
    }
    if let Some(thread_id) = &body.thread_id {
        payload.insert("thread_id".to_string(), json!(thread_id));
    }
    if let Some(message_id) = &body.message_id {
        payload.insert("message_id".to_string(), json!(message_id));
    }
    if let Some(ok) = body.ok {
        payload.insert("ok".to_string(), json!(ok));
    }
    InboundEvent {
        event_type: "op.result".to_string(),
        payload: Value::Object(payload).to_string().into_bytes(),
        sequence: 0,
    }
}

/// Guest one-shot: given an op and its REST outcome, produce the `op.result`
/// inbound event to queue, or `None` when the op has no correlated result
/// (`approval-request` resolves via a later `approval.decision`;
/// `interaction-reply` is uncorrelated).
pub fn outbound_op_result(op: &OutboundKind, status: u16, body: &[u8]) -> Option<InboundEvent> {
    let op_id = op.op_id()?;
    let result = parse_outbound(op, status, body)?;
    Some(op_result_event(op_id, &result))
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
        pending_with_tool(request_id, approver_role_ids, started_by, "Bash")
    }

    fn pending_with_tool(
        request_id: &str,
        approver_role_ids: &[&str],
        started_by: Option<&str>,
        tool: &str,
    ) -> PendingApprovals {
        let mut map = PendingApprovals::new();
        map.insert(
            request_id.to_string(),
            PendingApproval {
                approver_role_ids: approver_role_ids.iter().map(|s| s.to_string()).collect(),
                started_by: started_by.map(str::to_string),
                tool: tool.to_string(),
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
            "id": "interaction-1",
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
            "id": "interaction-btn",
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
            ..
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
            ..
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
            ..
        } = outcome
        else {
            panic!("expected Approval outcome");
        };
        assert_eq!(request_id, "req-1");
        assert!(allow);
        assert_eq!(actor, "clicker-1");
        assert_eq!(token, "tok-btn-1");
        // Tool suffix restored (native's exact edit string); default helper
        // tool is "Bash".
        assert_eq!(edit, "✅ Approved by <@clicker-1> — **Bash**");
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
        assert_eq!(edit, "🚫 Denied by <@clicker-1> — **Bash**");
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
        let InteractionOutcome::Unauthorized {
            token, ephemeral, ..
        } = outcome
        else {
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
            id: "interaction-1".to_string(),
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
            id: "interaction-btn".to_string(),
            token: "tok-8".to_string(),
            text: "✅ Approved by <@clicker-1> — **Bash**".to_string(),
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
                id: "interaction-btn".to_string(),
                token: "tok-9".to_string(),
                text: "You are not authorized to approve this.".to_string(),
            }]
        );
    }

    // ---- Task 10: outbound REST planning + response parsing --------------

    const TEST_TOKEN: &str = "SECRET-BOT-TOKEN";

    fn ctx() -> RestCtx<'static> {
        RestCtx {
            token: TEST_TOKEN,
            app_id: "app-1",
            guild_id: "guild-1",
        }
    }

    /// The one Authorization header value a request carries (panics if absent).
    fn auth(req: &RestRequest) -> &str {
        req.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str())
            .expect("every Discord REST request carries an Authorization header")
    }

    fn body_json(req: &RestRequest) -> Value {
        serde_json::from_str(req.body.as_deref().expect("request has a body"))
            .expect("request body is valid JSON")
    }

    fn header(req: &RestRequest, key: &str) -> Option<String> {
        req.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.clone())
    }

    #[test]
    fn plan_create_channel_posts_guild_channel_of_text_type() {
        let op = OutboundKind::CreateChannel {
            op_id: "op1".into(),
            name: "general".into(),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/guilds/guild-1/channels"
        );
        assert_eq!(body_json(req), json!({ "name": "general", "type": 0 }));
        assert_eq!(auth(req), "Bot SECRET-BOT-TOKEN");
        assert_eq!(
            header(req, "content-type").as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn plan_create_thread_posts_thread_on_channel() {
        let op = OutboundKind::CreateThread {
            op_id: "op1".into(),
            channel_id: "chan-1".into(),
            title: "session xyz".into(),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(
            reqs[0].url,
            "https://discord.com/api/v10/channels/chan-1/threads"
        );
        assert_eq!(body_json(&reqs[0]), json!({ "name": "session xyz" }));
    }

    #[test]
    fn plan_send_message_posts_content() {
        let op = OutboundKind::SendMessage {
            op_id: "op1".into(),
            channel_id: "chan-1".into(),
            text: "hello world".into(),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(
            reqs[0].url,
            "https://discord.com/api/v10/channels/chan-1/messages"
        );
        assert_eq!(body_json(&reqs[0]), json!({ "content": "hello world" }));
    }

    #[test]
    fn plan_edit_message_patches_content() {
        let op = OutboundKind::EditMessage {
            op_id: "op1".into(),
            channel_id: "chan-1".into(),
            message_id: "msg-1".into(),
            text: "edited".into(),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(
            reqs[0].url,
            "https://discord.com/api/v10/channels/chan-1/messages/msg-1"
        );
        assert_eq!(body_json(&reqs[0]), json!({ "content": "edited" }));
    }

    #[test]
    fn plan_send_messages_posts_one_request_per_chunk() {
        let op = OutboundKind::SendMessages {
            op_id: "op1".into(),
            channel_id: "chan-1".into(),
            chunks: vec!["part 1".into(), "part 2".into(), "part 3".into()],
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 3);
        for (req, expected) in reqs.iter().zip(["part 1", "part 2", "part 3"]) {
            assert_eq!(req.method, "POST");
            assert_eq!(
                req.url,
                "https://discord.com/api/v10/channels/chan-1/messages"
            );
            assert_eq!(body_json(req), json!({ "content": expected }));
        }
    }

    #[test]
    fn plan_approval_request_posts_buttons_with_custom_ids() {
        let op = OutboundKind::ApprovalRequest {
            op_id: "op1".into(),
            request_id: "req-1".into(),
            conversation_id: "conv-1".into(),
            tool: "Bash".into(),
            summary: "rm -rf /tmp/x".into(),
            approver_role_ids: vec!["r1".into()],
            started_by: Some("u1".into()),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/channels/conv-1/messages"
        );
        let body = body_json(req);
        assert_eq!(
            body["content"].as_str().unwrap(),
            "🔐 Approve **Bash**?\n```\nrm -rf /tmp/x\n```"
        );
        let row = &body["components"][0];
        assert_eq!(row["type"].as_u64(), Some(1)); // action row
        let buttons = row["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0]["type"].as_u64(), Some(2)); // button
        assert_eq!(buttons[0]["style"].as_u64(), Some(3)); // success
        assert_eq!(buttons[0]["custom_id"].as_str(), Some("req-1:approve"));
        assert_eq!(buttons[1]["style"].as_u64(), Some(4)); // danger
        assert_eq!(buttons[1]["custom_id"].as_str(), Some("req-1:deny"));
    }

    #[test]
    fn plan_interaction_reply_patches_original_webhook_message() {
        let op = OutboundKind::InteractionReply {
            token: "int-token".into(),
            text: "✅ connected".into(),
        };
        let reqs = plan_outbound(&op, &ctx());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(
            reqs[0].url,
            "https://discord.com/api/v10/webhooks/app-1/int-token/messages/@original"
        );
        assert_eq!(body_json(&reqs[0]), json!({ "content": "✅ connected" }));
    }

    #[test]
    fn register_commands_puts_guild_commands_body() {
        let req = register_commands_request(&ctx());
        assert_eq!(req.method, "PUT");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/applications/app-1/guilds/guild-1/commands"
        );
        assert_eq!(body_json(&req), build_commands());
        assert_eq!(auth(&req), "Bot SECRET-BOT-TOKEN");
    }

    #[test]
    fn channel_get_is_a_bodyless_get_with_auth() {
        let req = channel_get_request(&ctx(), "chan-1");
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://discord.com/api/v10/channels/chan-1");
        assert!(req.body.is_none());
        assert_eq!(auth(&req), "Bot SECRET-BOT-TOKEN");
    }

    #[test]
    fn defer_request_is_type5_ephemeral_callback() {
        let req = defer_request(&ctx(), "int-1", "int-token");
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/interactions/int-1/int-token/callback"
        );
        assert_eq!(
            body_json(&req),
            json!({ "type": 5, "data": { "flags": 64 } })
        );
    }

    #[test]
    fn ephemeral_reply_is_type4_flags64_callback() {
        let req = ephemeral_reply_request(&ctx(), "int-1", "int-token", "nope");
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/interactions/int-1/int-token/callback"
        );
        assert_eq!(
            body_json(&req),
            json!({ "type": 4, "data": { "content": "nope", "flags": 64 } })
        );
    }

    #[test]
    fn update_message_is_type7_callback_clearing_components() {
        let req = update_message_request(&ctx(), "int-1", "int-token", "✅ Approved");
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://discord.com/api/v10/interactions/int-1/int-token/callback"
        );
        assert_eq!(
            body_json(&req),
            json!({ "type": 7, "data": { "content": "✅ Approved", "components": [] } })
        );
    }

    #[test]
    fn parse_create_channel_extracts_channel_id() {
        let op = OutboundKind::CreateChannel {
            op_id: "op1".into(),
            name: "n".into(),
        };
        let body = br#"{"id":"555","name":"n","type":0}"#;
        assert_eq!(
            parse_outbound(&op, 201, body),
            Some(OpResultBody {
                channel_id: Some("555".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn parse_create_thread_extracts_thread_id() {
        let op = OutboundKind::CreateThread {
            op_id: "op1".into(),
            channel_id: "c".into(),
            title: "t".into(),
        };
        let body = br#"{"id":"777"}"#;
        assert_eq!(
            parse_outbound(&op, 201, body),
            Some(OpResultBody {
                thread_id: Some("777".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn parse_send_message_extracts_message_id() {
        let op = OutboundKind::SendMessage {
            op_id: "op1".into(),
            channel_id: "c".into(),
            text: "t".into(),
        };
        let body = br#"{"id":"999"}"#;
        assert_eq!(
            parse_outbound(&op, 200, body),
            Some(OpResultBody {
                message_id: Some("999".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn parse_edit_message_reports_ok_from_status() {
        let op = OutboundKind::EditMessage {
            op_id: "op1".into(),
            channel_id: "c".into(),
            message_id: "m".into(),
            text: "t".into(),
        };
        assert_eq!(
            parse_outbound(&op, 200, b""),
            Some(OpResultBody {
                ok: Some(true),
                ..Default::default()
            })
        );
        assert_eq!(
            parse_outbound(&op, 403, b""),
            Some(OpResultBody {
                ok: Some(false),
                ..Default::default()
            })
        );
    }

    #[test]
    fn parse_send_messages_reports_ok() {
        let op = OutboundKind::SendMessages {
            op_id: "op1".into(),
            channel_id: "c".into(),
            chunks: vec!["a".into()],
        };
        assert_eq!(parse_outbound(&op, 200, b"").unwrap().ok, Some(true));
    }

    #[test]
    fn parse_approval_and_interaction_reply_have_no_op_result() {
        let approval = OutboundKind::ApprovalRequest {
            op_id: "op1".into(),
            request_id: "r".into(),
            conversation_id: "c".into(),
            tool: "Bash".into(),
            summary: "s".into(),
            approver_role_ids: vec![],
            started_by: None,
        };
        assert_eq!(parse_outbound(&approval, 200, br#"{"id":"1"}"#), None);
        let reply = OutboundKind::InteractionReply {
            token: "t".into(),
            text: "x".into(),
        };
        assert_eq!(parse_outbound(&reply, 200, b""), None);
    }

    #[test]
    fn token_never_leaves_the_bot_auth_header() {
        let ctx = ctx();
        let ops = [
            OutboundKind::CreateChannel {
                op_id: "o".into(),
                name: "n".into(),
            },
            OutboundKind::CreateThread {
                op_id: "o".into(),
                channel_id: "c".into(),
                title: "t".into(),
            },
            OutboundKind::SendMessage {
                op_id: "o".into(),
                channel_id: "c".into(),
                text: "t".into(),
            },
            OutboundKind::EditMessage {
                op_id: "o".into(),
                channel_id: "c".into(),
                message_id: "m".into(),
                text: "t".into(),
            },
            OutboundKind::SendMessages {
                op_id: "o".into(),
                channel_id: "c".into(),
                chunks: vec!["a".into(), "b".into()],
            },
            OutboundKind::ApprovalRequest {
                op_id: "o".into(),
                request_id: "r".into(),
                conversation_id: "c".into(),
                tool: "Bash".into(),
                summary: "s".into(),
                approver_role_ids: vec![],
                started_by: None,
            },
            OutboundKind::InteractionReply {
                token: "t".into(),
                text: "x".into(),
            },
        ];
        let mut requests: Vec<RestRequest> =
            ops.iter().flat_map(|op| plan_outbound(op, &ctx)).collect();
        requests.push(register_commands_request(&ctx));
        requests.push(channel_get_request(&ctx, "c"));
        requests.push(defer_request(&ctx, "i", "it"));
        requests.push(ephemeral_reply_request(&ctx, "i", "it", "x"));
        requests.push(update_message_request(&ctx, "i", "it", "x"));

        for req in &requests {
            // The token appears in EXACTLY one place: the Bot auth header.
            assert!(
                !req.url.contains(TEST_TOKEN),
                "token leaked into URL: {}",
                req.url
            );
            if let Some(body) = &req.body {
                assert!(!body.contains(TEST_TOKEN), "token leaked into body: {body}");
            }
            let mut auth_headers = 0;
            for (name, value) in &req.headers {
                if name.eq_ignore_ascii_case("authorization") {
                    auth_headers += 1;
                    assert_eq!(value, "Bot SECRET-BOT-TOKEN");
                } else {
                    assert!(
                        !value.contains(TEST_TOKEN),
                        "token leaked into header {name}: {value}"
                    );
                }
            }
            assert_eq!(auth_headers, 1, "exactly one Bot auth header per request");
        }
    }

    #[test]
    fn is_thread_channel_classifies_thread_types() {
        assert!(is_thread_channel(br#"{"type":11}"#)); // public thread
        assert!(is_thread_channel(br#"{"type":12}"#)); // private thread
        assert!(is_thread_channel(br#"{"type":10}"#)); // announcement thread
        assert!(!is_thread_channel(br#"{"type":0}"#)); // text channel
        assert!(!is_thread_channel(br#"{"type":1}"#)); // DM
        assert!(!is_thread_channel(b"not json")); // native's `_ => false`
        assert!(!is_thread_channel(br#"{}"#)); // no type
    }

    #[test]
    fn message_create_channel_id_only_for_message_create() {
        let frame = dispatch(
            "MESSAGE_CREATE",
            5,
            json!({ "channel_id": "chan-42", "content": "hi" }),
        );
        assert_eq!(
            message_create_channel_id(&frame),
            Some("chan-42".to_string())
        );
        assert_eq!(message_create_channel_id(&hello(41250)), None);
        assert_eq!(
            message_create_channel_id(&dispatch("READY", 1, json!({}))),
            None
        );
        assert_eq!(message_create_channel_id("not json"), None);
    }

    #[test]
    fn decode_outbound_round_trips_each_kind() {
        let cases = vec![
            (
                "create-channel",
                json!({ "op_id": "o", "name": "general" }),
                OutboundKind::CreateChannel {
                    op_id: "o".into(),
                    name: "general".into(),
                },
            ),
            (
                "create-thread",
                json!({ "op_id": "o", "channel_id": "c", "title": "t" }),
                OutboundKind::CreateThread {
                    op_id: "o".into(),
                    channel_id: "c".into(),
                    title: "t".into(),
                },
            ),
            (
                "send-message",
                json!({ "op_id": "o", "channel_id": "c", "text": "hi" }),
                OutboundKind::SendMessage {
                    op_id: "o".into(),
                    channel_id: "c".into(),
                    text: "hi".into(),
                },
            ),
            (
                "edit-message",
                json!({ "op_id": "o", "channel_id": "c", "message_id": "m", "text": "e" }),
                OutboundKind::EditMessage {
                    op_id: "o".into(),
                    channel_id: "c".into(),
                    message_id: "m".into(),
                    text: "e".into(),
                },
            ),
            (
                "send-messages",
                json!({ "op_id": "o", "channel_id": "c", "chunks": ["a", "b"] }),
                OutboundKind::SendMessages {
                    op_id: "o".into(),
                    channel_id: "c".into(),
                    chunks: vec!["a".into(), "b".into()],
                },
            ),
            (
                "approval-request",
                json!({
                    "op_id": "o", "request_id": "r", "conversation_id": "c",
                    "tool": "Bash", "summary": "s", "approver_role_ids": ["r1"],
                    "started_by": "u1",
                }),
                OutboundKind::ApprovalRequest {
                    op_id: "o".into(),
                    request_id: "r".into(),
                    conversation_id: "c".into(),
                    tool: "Bash".into(),
                    summary: "s".into(),
                    approver_role_ids: vec!["r1".into()],
                    started_by: Some("u1".into()),
                },
            ),
            (
                "interaction-reply",
                json!({ "token": "t", "text": "x" }),
                OutboundKind::InteractionReply {
                    token: "t".into(),
                    text: "x".into(),
                },
            ),
        ];
        for (event_type, payload, expected) in cases {
            let decoded = decode_outbound(event_type, payload.to_string().as_bytes());
            assert_eq!(decoded, Some(expected), "decoding {event_type}");
        }
        assert_eq!(decode_outbound("nope", b"{}"), None);
    }

    #[test]
    fn op_result_event_is_flat_op_result() {
        let event = op_result_event(
            "op-7",
            &OpResultBody {
                channel_id: Some("555".into()),
                ..Default::default()
            },
        );
        assert_eq!(event.event_type, "op.result");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(payload, json!({ "op_id": "op-7", "channel_id": "555" }));
    }

    #[test]
    fn plan_reconnect_resumes_when_a_session_exists() {
        let mut state = GatewayState::new("t");
        on_frame(&mut state, &hello(41250), false, &no_pending());
        on_frame(&mut state, &ready(4), false, &no_pending()); // stores session_id

        assert!(state.plan_reconnect(), "should resume with a live session");
        assert!(state.pending_resume);
        assert_eq!(state.phase, GatewayPhase::Resuming);
        // The next HELLO must therefore RESUME (session preserved), not IDENTIFY.
        let actions = on_frame(&mut state, &hello(41250), false, &no_pending());
        assert_eq!(sent_frame(&actions)["op"].as_u64(), Some(OP_RESUME));
    }

    #[test]
    fn plan_reconnect_reidentifies_without_a_session() {
        let mut state = GatewayState::new("t");
        assert!(!state.plan_reconnect(), "no session → fresh identify");
        assert!(!state.pending_resume);
        assert_eq!(state.phase, GatewayPhase::Connecting);
    }

    #[test]
    fn resume_ws_url_appends_the_query_to_the_session_host() {
        assert_eq!(
            resume_ws_url("wss://gateway-us-east1-b.discord.gg"),
            "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json"
        );
        // A trailing slash is not doubled.
        assert_eq!(
            resume_ws_url("wss://resume.discord.gg/"),
            "wss://resume.discord.gg/?v=10&encoding=json"
        );
    }

    #[test]
    fn outbound_op_result_none_for_approval_and_interaction_reply() {
        let approval = OutboundKind::ApprovalRequest {
            op_id: "o".into(),
            request_id: "r".into(),
            conversation_id: "c".into(),
            tool: "Bash".into(),
            summary: "s".into(),
            approver_role_ids: vec![],
            started_by: None,
        };
        assert!(outbound_op_result(&approval, 200, br#"{"id":"1"}"#).is_none());
        let reply = OutboundKind::InteractionReply {
            token: "t".into(),
            text: "x".into(),
        };
        assert!(outbound_op_result(&reply, 200, b"").is_none());
        let create = OutboundKind::CreateChannel {
            op_id: "op-1".into(),
            name: "n".into(),
        };
        let event = outbound_op_result(&create, 201, br#"{"id":"42"}"#).unwrap();
        assert_eq!(event.event_type, "op.result");
        let payload: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(payload, json!({ "op_id": "op-1", "channel_id": "42" }));
    }
}
