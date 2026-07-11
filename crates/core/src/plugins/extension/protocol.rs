//! JSON-RPC method names and request/response shapes for the extension
//! protocol (host <-> extension subprocess), framed over stdio via
//! [`crate::stdio_jsonrpc`].
//!
//! This module is pure (no I/O): it only builds request `Value`s and
//! parses/validates response `Value`s. The write/read/timeout flow lives in
//! `proc.rs`, which is generic over the transport (a real subprocess's
//! pipes, or — in tests — an in-memory `tokio::io::duplex` pair) so protocol
//! logic can be exercised without spawning a process.
//!
//! DT3 wired `initialize`/`shutdown`. DT4 adds `ping` (health supervision —
//! [`ping_request`]/[`parse_ping_response`]). DT5 adds `event/<name>`
//! ([`event_request`]/[`parse_event_response`]) — the host's dispatch of a
//! `HookEvent` to a subscribed extension, per `plugins::extension::events`'s
//! module doc.

use serde_json::{json, Value};

use crate::stdio_jsonrpc;

/// The extension protocol version this host build speaks. `extension/initialize`
/// carries it verbatim (no semver ranges — an extension must match exactly
/// if it reports one at all); a mismatch fails the handshake the same
/// non-fatal way any other malformed response does — see [`InitError`].
pub const PROTOCOL_VERSION: &str = "1";

/// Host -> extension: the one-time startup handshake. Carries the host's
/// protocol version and the events this extension's manifest subscribes to;
/// the extension replies with which of those it actually confirms (and, if
/// it declared `provides_tools`, its tool definitions).
pub const METHOD_INITIALIZE: &str = "extension/initialize";

/// Host -> extension: request a graceful stop (see
/// `proc::ExtensionProc::shutdown`).
pub const METHOD_SHUTDOWN: &str = "extension/shutdown";

/// Host -> extension health probe, sent periodically by `proc::supervise`
/// (DT4) while an extension is `Running`.
pub const METHOD_PING: &str = "extension/ping";

/// Host -> extension event notification method prefix — DT5 dispatch fires
/// `"event/<HookEvent::as_str()>"` (e.g. `"event/tool.before"`), built by
/// [`event_request`].
pub const METHOD_EVENT_PREFIX: &str = "event/";

/// Build the `extension/initialize` request. `events` is the wire form
/// (`HookEvent::as_str()`) of every event this extension's manifest
/// subscribes to. `provides_tools` mirrors `ExtensionSpec::provides_tools` —
/// it tells the extension up front whether the host wants tool definitions
/// back in its response (`result.tools`, captured by
/// [`parse_initialize_response`]) so a well-behaved extension that does not
/// provide tools can skip building/sending them.
pub fn initialize_request(id: i64, events: &[&str], provides_tools: bool) -> Value {
    stdio_jsonrpc::build_request(
        id,
        METHOD_INITIALIZE,
        Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "host": { "name": "ryuzi", "version": env!("CARGO_PKG_VERSION") },
            "events": events,
            "providesTools": provides_tools,
        })),
    )
}

/// Build the `extension/shutdown` request (no params).
pub fn shutdown_request(id: i64) -> Value {
    stdio_jsonrpc::build_request(id, METHOD_SHUTDOWN, None)
}

/// Build the `extension/ping` health-check request (no params).
pub fn ping_request(id: i64) -> Value {
    stdio_jsonrpc::build_request(id, METHOD_PING, None)
}

/// Whether an `extension/ping` response counts as healthy: anything that
/// isn't a JSON-RPC `error` object. Deliberately more permissive than
/// [`parse_initialize_response`] — a ping's only job is proving the
/// extension is alive and still speaking JSON-RPC on this id, not validating
/// a payload shape; the exact ping reply body is not part of the negotiated
/// protocol surface, so a minimal `{"result":{}}` and a chattier one both
/// count as healthy.
pub fn parse_ping_response(resp: &Value) -> bool {
    resp.get("error").is_none()
}

/// Build an `event/<name>` request: the host asks a subscribed extension to
/// react to `event`, carrying `payload` verbatim as `params` (the exact same
/// JSON the on-disk script sink — `harness::native::hooks::run` — receives
/// on stdin). Gating dispatch awaits the response up to the manifest's
/// per-event `timeout_ms`; observational dispatch sends this identical
/// request but does not await it on the caller's hot path — see
/// `plugins::extension::events`'s module doc.
pub fn event_request(id: i64, event: &str, payload: &Value) -> Value {
    stdio_jsonrpc::build_request(
        id,
        &format!("{METHOD_EVENT_PREFIX}{event}"),
        Some(payload.clone()),
    )
}

/// An extension's response to an `event/<name>` dispatch: whether it wants
/// to deny the (gating) action, and why. Observational dispatch parses this
/// too but ignores it — only a gating event's caller inspects `deny`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventAck {
    pub deny: bool,
    pub reason: Option<String>,
}

/// Parse an `event/<name>` response. Deliberately permissive like
/// [`parse_ping_response`], not strict like [`parse_initialize_response`]:
/// anything that is not an explicit `{"result":{"deny":true,...}}` —
/// including a JSON-RPC `error` object, a missing `result`, or `deny`
/// absent/false — parses as "does not deny." A timeout or transport error
/// (which this function never sees; the caller handles those separately) is
/// the ONLY thing that triggers the fail-open policy — an extension that
/// responds with something merely unexpected is treated exactly like one
/// that explicitly allows, never like a crash.
pub fn parse_event_response(resp: &Value) -> EventAck {
    let deny = resp
        .get("result")
        .and_then(|r| r.get("deny"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !deny {
        return EventAck::default();
    }
    let reason = resp
        .get("result")
        .and_then(|r| r.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    EventAck { deny, reason }
}

/// The extension's validated `extension/initialize` response.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InitializeAck {
    /// The events the extension confirms it will actually handle — may be a
    /// subset of what the host offered (e.g. it only registered handlers
    /// for some of them). Kept as wire strings, not parsed to `HookEvent`
    /// here: an extension confirming a string outside the known vocabulary
    /// (or one the manifest never declared) is a policy question for the
    /// caller (DT5), not something this parser should silently drop or
    /// reinterpret.
    pub events: Vec<String>,
    /// Raw tool definitions, present only when the extension declared
    /// `provides_tools`. Kept as opaque `Value`s in this slice — DT6 defines
    /// the typed shape and wraps each into an `ExtensionTool`.
    pub tools: Vec<Value>,
}

/// Why an `extension/initialize` handshake failed. Every variant maps to a
/// non-fatal `ExtensionStatus::Failed(reason)` at the call site — an
/// extension that fails to initialize never brings down the daemon (see the
/// design doc's "Handshake" section).
#[derive(Debug)]
pub enum InitError {
    /// The extension returned a JSON-RPC `error` object.
    Rejected,
    /// `result.ok` was absent or `false`.
    NotOk,
    /// `result.protocolVersion` was present but did not match
    /// [`PROTOCOL_VERSION`].
    ProtocolMismatch,
    /// The response had no usable `result` at all.
    Malformed,
    /// The extension's stdout closed before a response arrived.
    Closed,
    /// A stdio read/write error occurred.
    Io(String),
    /// No response arrived within the handshake's timeout budget.
    Timeout,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Rejected => write!(f, "initialize was rejected"),
            InitError::NotOk => write!(f, "initialize did not report ok"),
            InitError::ProtocolMismatch => write!(f, "initialize protocol version mismatch"),
            InitError::Malformed => write!(f, "initialize response was malformed"),
            InitError::Closed => write!(f, "closed the connection during initialize"),
            InitError::Io(e) => write!(f, "transport error during initialize: {e}"),
            InitError::Timeout => write!(f, "initialize timed out"),
        }
    }
}

impl std::error::Error for InitError {}

/// Parse and validate an `extension/initialize` JSON-RPC response.
/// `protocolVersion` is only checked when the extension bothers to send one
/// — an extension that omits it (a minimal implementation) is not penalized
/// as long as `result.ok` is true.
pub fn parse_initialize_response(resp: &Value) -> Result<InitializeAck, InitError> {
    if resp.get("error").is_some() {
        return Err(InitError::Rejected);
    }
    let Some(result) = resp.get("result") else {
        return Err(InitError::Malformed);
    };
    if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
        if version != PROTOCOL_VERSION {
            return Err(InitError::ProtocolMismatch);
        }
    }
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        return Err(InitError::NotOk);
    }
    let events = result
        .get("events")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(InitializeAck { events, tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_request_shape() {
        let req = initialize_request(1, &["tool.before", "tool.after"], true);
        assert_eq!(req["method"], METHOD_INITIALIZE);
        assert_eq!(req["id"], 1);
        assert_eq!(req["params"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            req["params"]["events"],
            json!(["tool.before", "tool.after"])
        );
        assert_eq!(req["params"]["providesTools"], true);
    }

    #[test]
    fn initialize_request_reports_provides_tools_false_when_the_extension_declares_none() {
        let req = initialize_request(1, &[], false);
        assert_eq!(req["params"]["providesTools"], false);
    }

    #[test]
    fn shutdown_request_has_no_params() {
        let req = shutdown_request(2);
        assert_eq!(req["method"], METHOD_SHUTDOWN);
        assert!(req.get("params").is_none());
    }

    #[test]
    fn ping_request_has_no_params() {
        let req = ping_request(3);
        assert_eq!(req["method"], METHOD_PING);
        assert_eq!(req["id"], 3);
        assert!(req.get("params").is_none());
    }

    #[test]
    fn parse_ping_response_accepts_any_non_error_result() {
        assert!(parse_ping_response(
            &json!({ "jsonrpc": "2.0", "id": 1, "result": {} })
        ));
        assert!(parse_ping_response(
            &json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": true, "extra": "chatty" } })
        ));
    }

    #[test]
    fn parse_ping_response_rejects_an_error_object() {
        assert!(!parse_ping_response(
            &json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -1, "message": "boom" } })
        ));
    }

    #[test]
    fn parse_initialize_response_accepts_ok_true() {
        let resp = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "ok": true, "events": ["tool.before"], "protocolVersion": PROTOCOL_VERSION }
        });
        let ack = parse_initialize_response(&resp).unwrap();
        assert_eq!(ack.events, vec!["tool.before".to_string()]);
        assert!(ack.tools.is_empty());
    }

    #[test]
    fn parse_initialize_response_accepts_a_response_with_no_protocol_version() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": true } });
        assert!(parse_initialize_response(&resp).is_ok());
    }

    #[test]
    fn parse_initialize_response_rejects_protocol_mismatch() {
        let resp = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "ok": true, "protocolVersion": "999" }
        });
        assert!(matches!(
            parse_initialize_response(&resp),
            Err(InitError::ProtocolMismatch)
        ));
    }

    #[test]
    fn parse_initialize_response_rejects_ok_false() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": false } });
        assert!(matches!(
            parse_initialize_response(&resp),
            Err(InitError::NotOk)
        ));
    }

    #[test]
    fn parse_initialize_response_rejects_an_error_object() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -1, "message": "boom" } });
        assert!(matches!(
            parse_initialize_response(&resp),
            Err(InitError::Rejected)
        ));
    }

    #[test]
    fn parse_initialize_response_rejects_a_missing_result() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1 });
        assert!(matches!(
            parse_initialize_response(&resp),
            Err(InitError::Malformed)
        ));
    }

    #[test]
    fn parse_initialize_response_captures_tool_defs() {
        let resp = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "ok": true, "tools": [{ "name": "lint", "inputSchema": {"type":"object"} }] }
        });
        let ack = parse_initialize_response(&resp).unwrap();
        assert_eq!(ack.tools.len(), 1);
        assert_eq!(ack.tools[0]["name"], "lint");
    }

    // ---------- event_request / parse_event_response (DT5) ----------

    #[test]
    fn event_request_shape_carries_the_prefixed_method_and_payload() {
        let payload = json!({ "tool": "bash", "input": { "command": "ls" } });
        let req = event_request(7, "tool.before", &payload);
        assert_eq!(req["method"], "event/tool.before");
        assert_eq!(req["id"], 7);
        assert_eq!(req["params"], payload);
    }

    #[test]
    fn parse_event_response_reports_deny_with_reason() {
        let resp = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "deny": true, "reason": "blocked by linter" }
        });
        let ack = parse_event_response(&resp);
        assert!(ack.deny);
        assert_eq!(ack.reason.as_deref(), Some("blocked by linter"));
    }

    #[test]
    fn parse_event_response_treats_deny_false_as_not_denying() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": { "deny": false } });
        assert_eq!(parse_event_response(&resp), EventAck::default());
    }

    #[test]
    fn parse_event_response_treats_a_missing_result_as_not_denying() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        assert_eq!(parse_event_response(&resp), EventAck::default());
    }

    #[test]
    fn parse_event_response_treats_an_error_object_as_not_denying() {
        // The caller (proc::dispatch_event) never even reaches this parser on
        // a transport failure — but an extension that replies with a
        // well-formed JSON-RPC `error` (rather than crashing/timing out) is
        // deliberately treated the same as an explicit allow, not folded
        // into the transport-failure fail-open path.
        let resp = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -1, "message": "boom" } });
        assert_eq!(parse_event_response(&resp), EventAck::default());
    }

    #[test]
    fn parse_event_response_ignores_a_reason_when_not_denying() {
        let resp = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "deny": false, "reason": "irrelevant" }
        });
        assert_eq!(parse_event_response(&resp), EventAck::default());
    }
}
