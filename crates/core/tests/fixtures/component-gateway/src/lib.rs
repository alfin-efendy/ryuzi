// A component fixture exporting `ryuzi:gateway/gateway@0.1.0` (including the
// Task-10 `poll-inbound`) for the `WasmGatewaySupervisor`. A single fixture
// covers every supervisor behaviour the Step-2 test needs; behaviour is driven
// by the `gateway-config` passed to `start`, which the component stashes in
// per-instance state so later `poll-inbound`/`health-check` calls can read it:
//   - `start` records the config and reports running.
//   - `poll-inbound` emits ONE typed inbound event on the first poll of an
//     instance (sequence 1), then empty lists — so the supervisor surfaces at
//     least one inbound event as observable status.
//   - `deliver-outbound` accepts the outbound event, echoing its sequence.
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

use exports::ryuzi::gateway::gateway::{
    GatewayConfig, GatewayDelivery, GatewayError, GatewayEvent, GatewayState, Guest,
};

#[derive(Default)]
struct State {
    started: bool,
    boom: bool,
    account: String,
    emitted: bool,
    seq: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

struct Fixture;

impl Guest for Fixture {
    fn start(config: GatewayConfig) -> Result<GatewayState, GatewayError> {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.started = true;
            state.boom = config.endpoint.contains("boom");
            state.account = config.account.clone();
            state.emitted = false;
            state.seq = 0;
        });
        Ok(GatewayState {
            running: true,
            detail: format!("connected:{}", config.account),
        })
    }

    fn stop() -> Result<GatewayState, GatewayError> {
        STATE.with(|state| state.borrow_mut().started = false);
        Ok(GatewayState {
            running: false,
            detail: "stopped".to_string(),
        })
    }

    fn deliver_outbound(event: GatewayEvent) -> Result<GatewayDelivery, GatewayError> {
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
            if state.emitted {
                return Ok(Vec::new());
            }
            state.emitted = true;
            state.seq += 1;
            Ok(vec![GatewayEvent {
                event_type: "message".to_string(),
                payload: b"hello from gateway".to_vec(),
                sequence: state.seq,
            }])
        })
    }
}

export!(Fixture);
