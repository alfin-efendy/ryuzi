//! wasm32-only guest glue: wires [`crate::logic`] to the host-owned
//! `ryuzi:websocket` transport + `ryuzi:settings`, and exports
//! `ryuzi:gateway/gateway`.
//!
//! Deliberately thin — every protocol decision lives in [`crate::logic`]. This
//! module only performs effects: read settings, open/poll/send/close the
//! websocket, supply a WASI monotonic clock to the state machine, and map the
//! machine's [`logic::Action`]s onto host calls. Message normalization (Task 8),
//! slash commands + approvals (Task 9), and Discord REST for `deliver-outbound`
//! (Task 10) build on this skeleton.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Instant;

use crate::logic::{self, Action};

wit_bindgen::generate!({
    path: "wit",
    world: "discord",
    generate_all,
});

use exports::ryuzi::gateway::gateway::{
    GatewayConfig, GatewayDelivery, GatewayError, GatewayEvent, GatewayState as WitGatewayState,
    Guest,
};
use ryuzi::settings::settings;
use ryuzi::websocket::websocket as ws;

/// Host setting keys (namespaced to `discord` by the host).
const KEY_TOKEN: &str = "plugin.discord.token";
const KEY_APP_ID: &str = "plugin.discord.app_id";
const KEY_GUILD_ID: &str = "plugin.discord.guild_id";

/// The live gateway connection: the websocket handle, the pure protocol state,
/// a monotonic clock base for the heartbeat schedule, and the inbound events
/// buffered for the next `poll-inbound`.
struct Runtime {
    handle: u64,
    state: logic::GatewayState,
    clock_base: Instant,
    pending: VecDeque<logic::InboundEvent>,
    last_error: Option<String>,
    /// Read at `start`; consumed by Task 9 slash-command registration.
    #[allow(dead_code)]
    app_id: Option<String>,
    /// Read at `start`; consumed by Task 9 slash-command registration.
    #[allow(dead_code)]
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

        let handle = ws::connect(logic::GATEWAY_URL, &[]).map_err(ws_err_to_gateway)?;

        RUNTIME.with(|cell| {
            *cell.borrow_mut() = Some(Runtime {
                handle,
                state: logic::GatewayState::new(token),
                clock_base: Instant::now(),
                pending: VecDeque::new(),
                last_error: None,
                app_id: settings_get(KEY_APP_ID),
                guild_id: settings_get(KEY_GUILD_ID),
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
        // Discord REST for the typed outbound ops (create channel/thread, send/
        // edit message, approvals, interaction replies) lands in Task 10. Until
        // then the op is acknowledged but not acted on.
        Ok(GatewayDelivery {
            accepted: false,
            sequence: event.sequence,
        })
    }

    fn health_check() -> Result<WitGatewayState, GatewayError> {
        RUNTIME.with(|cell| match &*cell.borrow() {
            Some(runtime) => Ok(WitGatewayState {
                running: true,
                detail: match &runtime.last_error {
                    Some(error) => format!("{:?}:{error}", runtime.state.phase),
                    None => format!("{:?}", runtime.state.phase),
                },
            }),
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
/// [`logic::on_frame`], perform the resulting actions, then send a heartbeat if
/// one is due and honour a zombie-connection reconnect.
fn drive(runtime: &mut Runtime) {
    // Drain everything the host buffered since the last poll.
    match ws::poll(runtime.handle) {
        Ok(frames) => {
            for frame in frames {
                if !frame.is_text {
                    continue; // Discord's json encoding is text-only.
                }
                let Ok(text) = String::from_utf8(frame.data) else {
                    continue;
                };
                let actions = logic::on_frame(&mut runtime.state, &text);
                perform(runtime, actions);
            }
        }
        Err(error) => runtime.last_error = Some(describe_ws_error(error)),
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
        }
    }
}

/// Close the current socket and open a fresh one. The protocol state (session
/// id, last sequence, pending-resume flag) is preserved so the next HELLO on the
/// new connection RESUMEs or re-IDENTIFYs as the state machine already decided.
fn reconnect(runtime: &mut Runtime, _resume: bool) {
    let _ = ws::close(runtime.handle);
    match ws::connect(logic::GATEWAY_URL, &[]) {
        Ok(handle) => {
            runtime.handle = handle;
            runtime.clock_base = Instant::now();
            runtime.last_error = None;
        }
        Err(error) => runtime.last_error = Some(describe_ws_error(error)),
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

export!(Discord);
