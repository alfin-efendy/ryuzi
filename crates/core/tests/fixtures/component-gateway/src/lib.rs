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
//     are posted. The delivery echoes the event's sequence. `sequence` itself
//     is assigned lazily, at the moment an event actually lands in a returned
//     `poll-inbound` batch (never at queue time) — so it always reflects true
//     wire delivery order, even across the deferred path's one-poll gap
//     (queuing an approval decision, then queuing/returning something else in
//     between, must never let the eventually-returned decision carry a LOWER
//     sequence than something the host already saw).
//   - `health-check` reports healthy.
//   - when `config.endpoint` contains "boom", `poll-inbound` loops forever, so
//     the host fuel/epoch budget traps it and the supervisor restarts the
//     component with capped backoff. Because each restart is a FRESH instance,
//     the boom config is re-applied by the supervisor's own `start` call, so it
//     traps again — exactly the repeated-trap scenario the backoff cap bounds.
//   - when `config.endpoint` contains "message-flow" (Task 5), the first poll
//     ALSO emits `message.mention` (workspace_id "ws-1"), then `message.thread`
//     (conversation_id "conv-0", matching the first id `FakeGateway::
//     create_conversation` hands back in the host bridge's own tests), then a
//     DUPLICATE of that same `message.thread` event — same event-type, payload,
//     AND `sequence` — proving the host bridge's replay dedup drops it instead
//     of double-dispatching `on_reply`.
//   - when `config.endpoint` contains "dm-flow" (Task 5), the first poll ALSO
//     emits a single `message.dm` event (conversation_id "dm-conv-1").
//   - when `config.endpoint` contains "slash-flow" (Task 6), the first poll ALSO
//     emits a single `slash.connect` event (token "tok-connect", opts name
//     "proj"), so the host bridge's slash routing drives `Router::on_connect`
//     and then delivers the computed `interaction-reply` back (which this
//     fixture accepts and ignores, like any unrecognized outbound op).

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
    /// Inbound events (event-type, payload) returned on the NEXT
    /// `poll-inbound` (op.results, so they come back on the immediate
    /// post-`deliver-outbound` poll; also approval decisions PROMOTED here
    /// from `deferred` on a prior poll). No `sequence` is stored — see the
    /// module doc: it is assigned only once an event is actually placed into
    /// a returned batch.
    ready: VecDeque<(String, Vec<u8>)>,
    /// Inbound events (event-type, payload) held back one extra poll
    /// (approval decisions), so they arrive a poll AFTER the delivery that
    /// produced them. Also unsequenced until promoted to `ready`.
    deferred: VecDeque<(String, Vec<u8>)>,
    /// Task 5: `config.endpoint` contained "message-flow" — emit the
    /// mention/thread/duplicate-thread sequence on the first poll.
    message_flow: bool,
    message_flow_emitted: bool,
    /// Task 5: `config.endpoint` contained "dm-flow" — emit a `message.dm` on
    /// the first poll.
    dm_flow: bool,
    dm_flow_emitted: bool,
    /// Task 6: `config.endpoint` contained "slash-flow" — emit a
    /// `slash.connect` on the first poll.
    slash_flow: bool,
    slash_flow_emitted: bool,
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

fn queue(queue: &mut VecDeque<(String, Vec<u8>)>, event_type: &str, payload: String) {
    queue.push_back((event_type.to_string(), payload.into_bytes()));
}

impl Guest for Fixture {
    fn start(config: GatewayConfig) -> Result<GatewayState, GatewayError> {
        STATE.with(|state| {
            *state.borrow_mut() = State {
                boom: config.endpoint.contains("boom"),
                account: config.account.clone(),
                message_flow: config.endpoint.contains("message-flow"),
                dm_flow: config.endpoint.contains("dm-flow"),
                slash_flow: config.endpoint.contains("slash-flow"),
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
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"channel_id\":\"chan-1\"}}");
                        queue(&mut state.ready, "op.result", payload);
                    }
                }
                "create-thread" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let payload =
                            format!("{{\"op_id\":\"{op_id}\",\"thread_id\":\"thread-1\"}}");
                        queue(&mut state.ready, "op.result", payload);
                    }
                }
                "send-message" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"message_id\":\"msg-1\"}}");
                        queue(&mut state.ready, "op.result", payload);
                    }
                }
                "edit-message" | "send-messages" => {
                    if let Some(op_id) = json_str(&event.payload, "op_id") {
                        let payload = format!("{{\"op_id\":\"{op_id}\",\"ok\":true}}");
                        queue(&mut state.ready, "op.result", payload);
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
                            let payload = format!(
                                "{{\"request_id\":\"{request_id}\",\"allow\":true,\"actor\":\"tester\"}}"
                            );
                            queue(&mut state.deferred, "approval.decision", payload);
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
            let mut batch: Vec<GatewayEvent> = Vec::new();

            // "ready" events (op.results from a just-delivered op, or an
            // approval decision PROMOTED here on a prior poll) come back now.
            // `sequence` is assigned HERE — at actual return time, in return
            // order — never at queue time, so it always matches true wire
            // delivery order (see the module doc).
            let ready: Vec<(String, Vec<u8>)> = state.ready.drain(..).collect();
            for (event_type, payload) in ready {
                let seq = next_seq(&mut state);
                batch.push(GatewayEvent {
                    event_type,
                    payload,
                    sequence: seq,
                });
            }

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
            // Task 5: the host bridge's message-routing + replay-dedup tests.
            if state.message_flow && !state.message_flow_emitted {
                state.message_flow_emitted = true;
                let mention_seq = next_seq(&mut state);
                batch.push(GatewayEvent {
                    event_type: "message.mention".to_string(),
                    payload:
                        br#"{"workspace_id":"ws-1","actor":"u1","prompt":"start it","attachments":[]}"#
                            .to_vec(),
                    sequence: mention_seq,
                });
                let thread_seq = next_seq(&mut state);
                let thread_payload =
                    br#"{"conversation_id":"conv-0","actor":"u1","prompt":"continue it","attachments":[]}"#
                        .to_vec();
                batch.push(GatewayEvent {
                    event_type: "message.thread".to_string(),
                    payload: thread_payload.clone(),
                    sequence: thread_seq,
                });
                // A duplicate of the SAME thread event (identical event-type,
                // payload, AND sequence) — the host bridge must drop this as a
                // replay instead of dispatching a second `on_reply`.
                batch.push(GatewayEvent {
                    event_type: "message.thread".to_string(),
                    payload: thread_payload,
                    sequence: thread_seq,
                });
            }
            if state.dm_flow && !state.dm_flow_emitted {
                state.dm_flow_emitted = true;
                let seq = next_seq(&mut state);
                batch.push(GatewayEvent {
                    event_type: "message.dm".to_string(),
                    payload:
                        br#"{"conversation_id":"dm-conv-1","user_id":"user-9","text":"hello there"}"#
                            .to_vec(),
                    sequence: seq,
                });
            }
            // Task 6: the host bridge's slash-command routing test. A single
            // `slash.connect` drives `Router::on_connect`; the bridge posts back
            // an `interaction-reply` via `deliver-outbound`, which this fixture
            // accepts and ignores (no `op.result` — it is fire-and-forget).
            if state.slash_flow && !state.slash_flow_emitted {
                state.slash_flow_emitted = true;
                let seq = next_seq(&mut state);
                batch.push(GatewayEvent {
                    event_type: "slash.connect".to_string(),
                    payload:
                        br#"{"token":"tok-connect","user_id":"u1","opts":{"name":"proj"},"role_ids":[]}"#
                            .to_vec(),
                    sequence: seq,
                });
            }

            // "deferred" events (approval decisions queued by a `deliver-
            // outbound` call before this poll) become ready for the NEXT
            // poll, so they arrive one poll after their delivery. Still no
            // sequence assigned — that happens only once THEY are actually
            // returned, above.
            let deferred: Vec<(String, Vec<u8>)> = state.deferred.drain(..).collect();
            state.ready.extend(deferred);

            Ok(batch)
        })
    }
}

export!(Fixture);
