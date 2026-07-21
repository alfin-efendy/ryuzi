//! wasm32-only guest glue: wires [`crate::logic`] to the host-owned
//! `ryuzi:websocket` transport, `ryuzi:http` egress (Discord REST), and
//! `ryuzi:settings`, and exports `ryuzi:gateway/gateway`.
//!
//! Deliberately thin — every protocol/REST decision lives in [`crate::logic`].
//! This module only performs effects: read settings, open/poll/send/close the
//! websocket, issue the planned Discord REST requests, supply a WASI monotonic
//! clock to the state machine, and map the machine's [`logic::Action`]s onto
//! host calls. In particular (Task 10):
//! - `start` registers the guild slash commands via REST after connecting.
//! - `poll-inbound`/`drive` resolves each `MESSAGE_CREATE`'s `is_thread` with a
//!   `GET /channels/{id}` classification (cached), detects a dropped socket
//!   (poll disconnect / `closed`/`closing` state) and reconnects promptly —
//!   resuming to the stored `resume_gateway_url` when a session exists.
//! - `deliver-outbound` decodes the typed op, plans its REST request(s), issues
//!   them, and queues the correlated `op.result` inbound event (except
//!   `approval-request`/`interaction-reply`, which carry no `op.result`); an
//!   `approval-request` also seeds `pending_approvals` (with its `tool`) before
//!   the buttons are posted, so a click authorizes purely against the map.
//! - the three interaction [`logic::Action`]s (defer / update-message /
//!   ephemeral) now issue their `POST /interactions/{id}/{token}/callback` REST.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::logic::{self, Action, RestCtx};

wit_bindgen::generate!({
    path: "wit",
    world: "discord",
    generate_all,
});

use exports::ryuzi::gateway::gateway::{
    GatewayConfig, GatewayDelivery, GatewayError, GatewayEvent, GatewayState as WitGatewayState,
    Guest,
};
use ryuzi::http::http;
use ryuzi::settings::settings;
use ryuzi::websocket::websocket as ws;

/// Host setting keys (namespaced to `discord` by the host).
const KEY_TOKEN: &str = "plugin.discord.token";
const KEY_APP_ID: &str = "plugin.discord.app_id";
const KEY_GUILD_ID: &str = "plugin.discord.guild_id";

/// The live gateway connection: the websocket handle, the pure protocol state,
/// a monotonic clock base for the heartbeat schedule, the inbound events
/// buffered for the next `poll-inbound`, and the Discord app/guild ids for REST.
struct Runtime {
    handle: u64,
    state: logic::GatewayState,
    clock_base: Instant,
    pending: VecDeque<logic::InboundEvent>,
    /// Outstanding approval requests, keyed by `request_id` — seeded by
    /// `deliver-outbound(approval-request)` (before the buttons are posted), so
    /// an `INTERACTION_CREATE` button click can be authorized by
    /// [`logic::handle_interaction`] purely against this map.
    pending_approvals: logic::PendingApprovals,
    /// `channel_id -> is_thread`, memoizing the `GET /channels/{id}` type
    /// classification so a busy thread doesn't re-fetch per message.
    channel_is_thread: HashMap<String, bool>,
    last_error: Option<String>,
    /// Discord application id, for slash-command registration + interaction-reply.
    app_id: Option<String>,
    /// Discord guild id, for channel/command creation.
    guild_id: Option<String>,
}

thread_local! {
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

struct Discord;

impl Guest for Discord {
    fn start(config: GatewayConfig) -> Result<WitGatewayState, GatewayError> {
        let token = settings_get(KEY_TOKEN)
            .ok_or_else(|| GatewayError::InvalidConfig(format!("missing setting `{KEY_TOKEN}`")))?;
        let app_id = settings_get(KEY_APP_ID);
        let guild_id = settings_get(KEY_GUILD_ID);

        let handle = ws::connect(logic::GATEWAY_URL, &[]).map_err(ws_err_to_gateway)?;

        // Register the guild slash commands (bulk overwrite). Best-effort: a
        // failure is recorded in `last_error` (surfaced via `health-check`) but
        // does not abort start — the gateway still connects and serves messages.
        let register_error = register_commands(&token, app_id.as_deref(), guild_id.as_deref());

        RUNTIME.with(|cell| {
            *cell.borrow_mut() = Some(Runtime {
                handle,
                state: logic::GatewayState::new(token),
                clock_base: Instant::now(),
                pending: VecDeque::new(),
                pending_approvals: logic::PendingApprovals::new(),
                channel_is_thread: HashMap::new(),
                last_error: register_error,
                app_id,
                guild_id,
            });
        });

        Ok(WitGatewayState {
            running: true,
            detail: format!("connecting:{}", config.account),
        })
    }

    fn stop() -> Result<WitGatewayState, GatewayError> {
        RUNTIME.with(|cell| {
            if let Some(runtime) = cell.borrow_mut().take() {
                let _ = ws::close(runtime.handle);
            }
        });
        Ok(WitGatewayState {
            running: false,
            detail: "stopped".to_string(),
        })
    }

    fn deliver_outbound(event: GatewayEvent) -> Result<GatewayDelivery, GatewayError> {
        RUNTIME.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(runtime) = guard.as_mut() else {
                return Ok(reject(event.sequence));
            };
            let Some(op) = logic::decode_outbound(&event.event_type, &event.payload) else {
                // An unknown/undecodable op: acknowledge non-acceptance rather
                // than trapping the component.
                return Ok(reject(event.sequence));
            };

            // Seed the pending approval BEFORE posting the buttons, so a click
            // that races the post is still authorizable against the map. Carries
            // `tool` so the later message edit restores native's suffix.
            if let logic::OutboundKind::ApprovalRequest {
                request_id,
                tool,
                approver_role_ids,
                started_by,
                ..
            } = &op
            {
                runtime.pending_approvals.insert(
                    request_id.clone(),
                    logic::PendingApproval {
                        approver_role_ids: approver_role_ids.clone(),
                        started_by: started_by.clone(),
                        tool: tool.clone(),
                    },
                );
            }

            // Plan + issue the REST request(s).
            let requests = {
                let ctx = rest_ctx(runtime);
                logic::plan_outbound(&op, &ctx)
            };
            let mut all_ok = true;
            let mut last_body: Vec<u8> = Vec::new();
            for request in &requests {
                match http_send(request) {
                    Ok(response) => {
                        last_body = response.body;
                        if !(200..300).contains(&response.status) {
                            all_ok = false;
                            runtime.last_error = Some(format!(
                                "discord REST {} -> HTTP {}",
                                request.method, response.status
                            ));
                        }
                    }
                    Err(error) => {
                        all_ok = false;
                        runtime.last_error = Some(describe_http_error(error));
                    }
                }
            }

            // Queue the correlated `op.result` (id ops read the response body;
            // ok ops read the aggregate success). `approval-request`/
            // `interaction-reply` produce no `op.result`.
            let status = if all_ok { 200 } else { 502 };
            if let Some(result) = logic::outbound_op_result(&op, status, &last_body) {
                runtime.pending.push_back(result);
            }

            // `accepted` mirrors REST success: on a failure the bridge's
            // `deliver` returns `Err` and promptly cancels the op/approval wait
            // (an approval whose button-post failed would otherwise hang for the
            // full timeout). Any op.result queued above for a failed id-op is a
            // harmless orphan (its `Correlation::resolve` is a no-op).
            Ok(GatewayDelivery {
                accepted: all_ok,
                sequence: event.sequence,
            })
        })
    }

    fn health_check() -> Result<WitGatewayState, GatewayError> {
        RUNTIME.with(|cell| match &*cell.borrow() {
            Some(runtime) => {
                // Report healthy only once the gateway actually holds (or is
                // resuming) a session — a still-connecting/identifying instance
                // or one whose last host call errored is NOT running, so the
                // supervisor's health/restart signal is accurate rather than a
                // blanket "a runtime exists". The reason always rides in `detail`.
                let connected = matches!(
                    runtime.state.phase,
                    logic::GatewayPhase::Ready | logic::GatewayPhase::Resuming
                );
                let running = connected && runtime.last_error.is_none();
                Ok(WitGatewayState {
                    running,
                    detail: match &runtime.last_error {
                        Some(error) => format!("{:?}:{error}", runtime.state.phase),
                        None => format!("{:?}", runtime.state.phase),
                    },
                })
            }
            None => Ok(WitGatewayState {
                running: false,
                detail: "not-started".to_string(),
            }),
        })
    }

    fn poll_inbound() -> Result<Vec<GatewayEvent>, GatewayError> {
        RUNTIME.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(runtime) = guard.as_mut() else {
                return Ok(Vec::new());
            };
            drive(runtime);
            Ok(runtime
                .pending
                .drain(..)
                .map(|event| GatewayEvent {
                    event_type: event.event_type,
                    payload: event.payload,
                    sequence: event.sequence,
                })
                .collect())
        })
    }
}

/// Advance the protocol one poll tick: drain inbound frames through
/// [`logic::on_frame`] (resolving each `MESSAGE_CREATE`'s `is_thread` first),
/// perform the resulting actions, detect a dropped socket and reconnect, then
/// send a heartbeat if one is due and honour a zombie-connection reconnect.
fn drive(runtime: &mut Runtime) {
    match ws::poll(runtime.handle) {
        Ok(frames) => {
            // A successful poll means the socket is alive; clear any stale error
            // so `health_check`'s liveness signal reflects the current state.
            runtime.last_error = None;
            for frame in frames {
                if !frame.is_text {
                    continue; // Discord's json encoding is text-only.
                }
                let Ok(text) = String::from_utf8(frame.data) else {
                    continue;
                };
                // Resolve `is_thread` (only consulted for a MESSAGE_CREATE) via a
                // REST `GET /channels/{id}` type check ahead of `on_frame`, as
                // native `Handler::message` does. Every other frame skips it.
                let is_thread = match logic::message_create_channel_id(&text) {
                    Some(channel_id) => classify_is_thread(runtime, &channel_id),
                    None => false,
                };
                let actions = logic::on_frame(
                    &mut runtime.state,
                    &text,
                    is_thread,
                    &runtime.pending_approvals,
                );
                perform(runtime, actions);
            }
        }
        Err(ws::WsError::Disconnected) => {
            // The socket dropped mid-poll: reconnect immediately (resuming if a
            // session exists), rather than waiting for the heartbeat blackout.
            let resume = runtime.state.plan_reconnect();
            reconnect(runtime, resume);
            return;
        }
        Err(error) => runtime.last_error = Some(describe_ws_error(error)),
    }

    // Even without a poll error, a closed/closing socket means the connection
    // dropped — reconnect promptly instead of waiting ~41s for the missed-ACK
    // path to notice.
    if socket_is_down(runtime.handle) {
        let resume = runtime.state.plan_reconnect();
        reconnect(runtime, resume);
        return;
    }

    // Heartbeat schedule (monotonic clock supplied to the pure machine).
    let now_ms = runtime.clock_base.elapsed().as_millis() as u64;
    if let Some(frame) = logic::due_heartbeat(&mut runtime.state, now_ms) {
        if let Err(error) = ws_send_text(runtime.handle, &frame) {
            runtime.last_error = Some(describe_ws_error(error));
        }
    }

    // A missed heartbeat ACK (zombie connection) asks for a resume-reconnect.
    if let Some(resume) = runtime.state.take_reconnect() {
        reconnect(runtime, resume);
    }
}

/// Resolve (and cache) whether `channel_id` is a thread, via a `GET
/// /channels/{id}` round-trip classified by [`logic::is_thread_channel`]. Any
/// REST failure classifies as not-a-thread, matching native's `_ => false`.
fn classify_is_thread(runtime: &mut Runtime, channel_id: &str) -> bool {
    if let Some(&cached) = runtime.channel_is_thread.get(channel_id) {
        return cached;
    }
    let request = {
        let ctx = rest_ctx(runtime);
        logic::channel_get_request(&ctx, channel_id)
    };
    let is_thread = match http_send(&request) {
        Ok(response) if (200..300).contains(&response.status) => {
            logic::is_thread_channel(&response.body)
        }
        _ => false,
    };
    runtime
        .channel_is_thread
        .insert(channel_id.to_string(), is_thread);
    is_thread
}

/// Carry out the actions [`logic::on_frame`] produced.
fn perform(runtime: &mut Runtime, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::SendFrame(frame) => {
                if let Err(error) = ws_send_text(runtime.handle, &frame) {
                    runtime.last_error = Some(describe_ws_error(error));
                }
            }
            // The interval already lives in the pure state; the guest drives the
            // schedule via `due_heartbeat`, so there is no separate host effect.
            Action::SetHeartbeat(_) => {}
            Action::EmitInbound(event) => runtime.pending.push_back(event),
            Action::Reconnect { resume } => reconnect(runtime, resume),
            // Discord interaction responses (design §5.4): defer a slash command
            // (type 5), rewrite an approval message after an authorized decision
            // (type 7), or ephemerally refuse an unauthorized click (type 4).
            Action::DeferInteraction { id, token } => {
                let request = {
                    let ctx = rest_ctx(runtime);
                    logic::defer_request(&ctx, &id, &token)
                };
                http_perform(runtime, &request);
            }
            Action::EditInteractionMessage { id, token, text } => {
                let request = {
                    let ctx = rest_ctx(runtime);
                    logic::update_message_request(&ctx, &id, &token, &text)
                };
                http_perform(runtime, &request);
            }
            Action::ReplyEphemeral { id, token, text } => {
                let request = {
                    let ctx = rest_ctx(runtime);
                    logic::ephemeral_reply_request(&ctx, &id, &token, &text)
                };
                http_perform(runtime, &request);
            }
        }
    }
}

/// Close the current socket and open a fresh one. A resume-reconnect dials the
/// per-session `resume_gateway_url` (with the gateway query string appended) so
/// the recovery HELLO's RESUME lands on a shard that still holds the session;
/// a fresh reconnect (or a missing resume url) dials the base gateway. The
/// protocol state (session id, last sequence, pending-resume flag) is preserved
/// so the next HELLO RESUMEs or re-IDENTIFYs as the state machine already decided.
fn reconnect(runtime: &mut Runtime, resume: bool) {
    let _ = ws::close(runtime.handle);
    let url = if resume {
        runtime
            .state
            .resume_gateway_url
            .as_deref()
            .map(logic::resume_ws_url)
            .unwrap_or_else(|| logic::GATEWAY_URL.to_string())
    } else {
        logic::GATEWAY_URL.to_string()
    };
    match ws::connect(&url, &[]) {
        Ok(handle) => {
            runtime.handle = handle;
            runtime.clock_base = Instant::now();
            runtime.last_error = None;
        }
        Err(error) => runtime.last_error = Some(describe_ws_error(error)),
    }
}

/// A closed/closing socket signals a dropped connection the guest must recover.
fn socket_is_down(handle: u64) -> bool {
    matches!(
        ws::state(handle),
        Ok(ws::WsState::Closed) | Ok(ws::WsState::Closing)
    )
}

/// The bot-token + id context for the REST planners. Borrows the long-lived
/// `Runtime`; the token comes from the pure `GatewayState`, the ids from
/// settings read at `start`.
fn rest_ctx(runtime: &Runtime) -> RestCtx<'_> {
    RestCtx {
        token: &runtime.state.token,
        app_id: runtime.app_id.as_deref().unwrap_or_default(),
        guild_id: runtime.guild_id.as_deref().unwrap_or_default(),
    }
}

/// Register the guild slash commands (bulk overwrite). `None` on success;
/// `Some(reason)` when it could not be done (missing ids or a REST failure).
fn register_commands(token: &str, app_id: Option<&str>, guild_id: Option<&str>) -> Option<String> {
    let (Some(app_id), Some(guild_id)) = (app_id, guild_id) else {
        return Some("command registration skipped: missing app_id/guild_id".to_string());
    };
    let ctx = RestCtx {
        token,
        app_id,
        guild_id,
    };
    let request = logic::register_commands_request(&ctx);
    match http_send(&request) {
        Ok(response) if (200..300).contains(&response.status) => None,
        Ok(response) => Some(format!(
            "command registration failed: HTTP {}",
            response.status
        )),
        Err(error) => Some(describe_http_error(error)),
    }
}

/// Issue one planned REST request over the host HTTP capability.
fn http_send(request: &logic::RestRequest) -> Result<http::HttpResponse, http::HttpError> {
    let http_request = http::HttpRequest {
        method: request.method.clone(),
        url: request.url.clone(),
        headers: request
            .headers
            .iter()
            .map(|(name, value)| http::Header {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        body: request.body.as_ref().map(|body| body.as_bytes().to_vec()),
    };
    http::request(&http_request)
}

/// Fire-and-forget REST (interaction responses): a non-2xx / error is recorded
/// in `last_error` but never surfaced as a trap — it must not wedge the loop.
fn http_perform(runtime: &mut Runtime, request: &logic::RestRequest) {
    match http_send(request) {
        Ok(response) if (200..300).contains(&response.status) => {}
        Ok(response) => {
            runtime.last_error = Some(format!(
                "discord REST {} -> HTTP {}",
                request.method, response.status
            ));
        }
        Err(error) => runtime.last_error = Some(describe_http_error(error)),
    }
}

fn reject(sequence: u64) -> GatewayDelivery {
    GatewayDelivery {
        accepted: false,
        sequence,
    }
}

fn ws_send_text(handle: u64, json: &str) -> Result<(), ws::WsError> {
    ws::send(
        handle,
        &ws::WsFrame {
            data: json.as_bytes().to_vec(),
            is_text: true,
        },
    )
}

fn settings_get(key: &str) -> Option<String> {
    match settings::get(key) {
        Ok(setting) if !setting.value.is_empty() => Some(setting.value),
        _ => None,
    }
}

fn ws_err_to_gateway(error: ws::WsError) -> GatewayError {
    match error {
        ws::WsError::Rejected => GatewayError::Rejected,
        ws::WsError::Disconnected => GatewayError::Disconnected,
        other => GatewayError::Failed(describe_ws_error(other)),
    }
}

fn describe_ws_error(error: ws::WsError) -> String {
    match error {
        ws::WsError::InvalidRequest(message) => format!("ws invalid request: {message}"),
        ws::WsError::Rejected => "ws rejected by host allowlist".to_string(),
        ws::WsError::Disconnected => "ws disconnected".to_string(),
        ws::WsError::LimitExceeded(message) => format!("ws limit exceeded: {message}"),
        ws::WsError::Failed(message) => format!("ws failed: {message}"),
    }
}

fn describe_http_error(error: http::HttpError) -> String {
    match error {
        http::HttpError::InvalidRequest(message) => format!("http invalid request: {message}"),
        http::HttpError::Rejected => "http rejected by host allowlist".to_string(),
        http::HttpError::Unavailable => "http capability unavailable".to_string(),
        http::HttpError::Failed(message) => format!("http failed: {message}"),
    }
}

export!(Discord);
