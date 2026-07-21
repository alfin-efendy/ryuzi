// A component fixture exporting `ryuzi:gateway/gateway@0.1.0` (including the
// Task-10 `poll-inbound`) for the `WasmGatewaySupervisor` and the Task-4 host
// gateway bridge (`WasmGateway`). One fixture covers every behaviour those
// tests need; behaviour is driven by the `gateway-config` passed to `start`
// and by the outbound ops handed to `deliver-outbound`:
//   - `start` records the config and reports running.
//   - `poll-inbound` emits ONE seed inbound event on the first poll of an
//     instance (event-type `message`, sequence 1), then whatever `op.result`/
//     `approval.decision` events prior `deliver-outbound` calls queued — so
//     the supervisor surfaces at least one inbound event as observable status
//     and the bridge's `Correlation` receives the results it awaits.
//   - `deliver-outbound` decodes the typed outbound op (by its `event-type`)
//     and queues the matching correlated inbound event:
//       * `create-channel`  -> `op.result{op_id, channel_id:"chan-1"}`
//       * `create-thread`   -> `op.result{op_id, thread_id:"thread-1"}`
//       * `send-message`     -> `op.result{op_id, message_id:"msg-1"}`
//       * `edit-message` / `send-messages` -> `op.result{op_id, ok:true}`
//       * `approval-request` -> `approval.decision{request_id, allow:true,
//         actor:"tester"}`, unless the request's `summary` contains "silent"
//         (then NO decision is queued, so the host bridge exercises its
//         approval-timeout -> auto-reject path).
//     `op.result`s are queued "ready" (returned on the immediate post-deliver
//     poll); an `approval.decision` is queued "deferred" (returned one poll
//     LATER), modelling a user clicking the button a moment after the buttons
//     are posted. The delivery echoes the event's sequence.
//   - `health-check` reports healthy.
//   - when `config.endpoint` contains "boom", `poll-inbound` loops forever, so
//     the host fuel/epoch budget traps it and the supervisor restarts the
//     component with capped backoff. Because each restart is a FRESH instance,
//     the boom config is re-applied by the supervisor's own `start` call, so it
//     traps again — exactly the repeated-trap scenario the backoff cap bounds.

wit_bindgen::generate!({
    path: "wit",
    world: "gateway-fixture",
    generate_all,
});

use std::cell::RefCell;
use std::collections::VecDeque;

use exports::ryuzi::gateway::gateway::{
    GatewayConfig, GatewayDelivery, GatewayError, GatewayEvent, GatewayState, Guest,
};

#[derive(Default)]
struct State {
    boom: bool,
    account: String,
    emitted: bool,
    seq: u64,
    /// Inbound events returned on the NEXT `poll-inbound` (op.results, so they
    /// come back on the immediate post-`deliver-outbound` poll).
    ready: VecDeque<GatewayEvent>,
    /// Inbound events held back one extra poll (approval decisions), so they
    /// arrive a poll AFTER the delivery that produced them.
    deferred: VecDeque<GatewayEvent>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

struct Fixture;

/// Extract a flat JSON string field's value from `payload`. Test payloads are
/// simple, un-escaped flat objects (`{"op_id":"op-1",...}`), so a full JSON
/// parser is unnecessary — keeping the fixture dependency-free.
fn json_str(payload: &[u8], key: &str) -> Option<String> {
    let text = std::str::from_utf8(payload).ok()?;
    let needle = format!("\"{key}\"");
    let after_key = &text[text.find(&needle)? + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let inner = after_colon.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

fn next_seq(state: &mut State) -> u64 {
    state.seq += 1;
    state.seq
}

fn queue(queue: &mut VecDeque<GatewayEvent>, seq: u64, event_type: &str, payload: String) {
    queue.push_back(GatewayEvent {
        event_type: event_type.to_string(),
        payload: payload.into_bytes(),
        sequence: seq,
    });
}

impl Guest for Fixture {
    fn start(config: GatewayConfig) -> Result<GatewayState, GatewayError> {
        STATE.with(|state| {
            *state.borrow_mut() = State {
                boom: config.endpoint.contains("boom"),
                account: config.account.clone(),
                ..State::default()
            };
        });
        Ok(GatewayState {
            running: true,
            detail: format!("connected:{}", config.account),
        })
    }

    fn stop() -> Result<GatewayState, GatewayError> {
        Ok(GatewayState {
            running: false,
            detail: "stopped".to_string(),
        })
    }

    fn deliver_outbound(event: GatewayEvent) -> Result<GatewayDelivery, GatewayError> {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            match event.event_type.as_str() {
                "create-channel" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let seq = next_seq(&mut state);
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"channel_id\":\"chan-1\"}}");
                        queue(&mut state.ready, seq, "op.result", payload);
                    }
                }
                "create-thread" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let seq = next_seq(&mut state);
                        let payload =
                            format!("{{\"op_id\":\"{op_id}\",\"thread_id\":\"thread-1\"}}");
                        queue(&mut state.ready, seq, "op.result", payload);
                    }
                }
                "send-message" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let seq = next_seq(&mut state);
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"message_id\":\"msg-1\"}}");
                        queue(&mut state.ready, seq, "op.result", payload);
                    }
                }
                "edit-message" | "send-messages" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let seq = next_seq(&mut state);
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"ok\":true}}");
                        queue(&mut state.ready, seq, "op.result", payload);
                    }
                }
                "approval-request" => {
                    // A "silent" request (summary marker) never resolves, so the
                    // host bridge exercises its approval-timeout -> auto-reject.
                    let silent = json_str(&event.payload, "summary")
                        .map(|summary| summary.contains("silent"))
                        .unwrap_or(false);
                    if !silent {
                        if let Some(request_id) = json_str(&event.payload, "request_id") {
                            let seq = next_seq(&mut state);
                            let payload = format!(
                                "{{\"request_id\":\"{request_id}\",\"allow\":true,\"actor\":\"tester\"}}"
                            );
                            queue(&mut state.deferred, seq, "approval.decision", payload);
                        }
                    }
                }
                _ => {}
            }
        });
        Ok(GatewayDelivery {
            accepted: true,
            sequence: event.sequence,
        })
    }

    fn health_check() -> Result<GatewayState, GatewayError> {
        let account = STATE.with(|state| state.borrow().account.clone());
        Ok(GatewayState {
            running: true,
            detail: format!("healthy:{account}"),
        })
    }

    fn poll_inbound() -> Result<Vec<GatewayEvent>, GatewayError> {
        let boom = STATE.with(|state| state.borrow().boom);
        // `black_box` keeps the optimizer from eliding this otherwise
        // side-effect-free loop, so the host's fuel/epoch budget really fires.
        if boom {
            let mut counter: u64 = 0;
            loop {
                counter = counter.wrapping_add(1);
                std::hint::black_box(counter);
            }
        }
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            // "ready" events (op.results from a just-delivered op) come back
            // now; "deferred" events (approval decisions) become ready for the
            // NEXT poll, so they arrive a poll after their delivery.
            let mut batch: Vec<GatewayEvent> = state.ready.drain(..).collect();
            let deferred: Vec<GatewayEvent> = state.deferred.drain(..).collect();
            state.ready.extend(deferred);
            // The one-time seed inbound event the supervisor lifecycle test asserts.
            if !state.emitted {
                state.emitted = true;
                let seq = next_seq(&mut state);
                batch.push(GatewayEvent {
                    event_type: "message".to_string(),
                    payload: b"hello from gateway".to_vec(),
                    sequence: seq,
                });
            }
            Ok(batch)
        })
    }
}

export!(Fixture);
