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
//! DT3 (this slice) wires `initialize`/`shutdown`. `ping` (DT4 health
//! supervision) and the `event/<name>` notification family (DT5 dispatch)
//! are reserved here as method-name constants only — no request/response
//! shape or dispatch logic for them exists yet, so later slices don't have
//! to renegotiate the wire vocabulary.

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

/// Host -> extension health probe. Reserved for DT4's supervision loop —
/// not sent by this slice.
pub const METHOD_PING: &str = "extension/ping";

/// Host -> extension event notification method prefix — DT5 dispatch fires
/// `"event/<HookEvent::as_str()>"` (e.g. `"event/tool.before"`). Reserved
/// here; not sent by this slice.
pub const METHOD_EVENT_PREFIX: &str = "event/";

/// Build the `extension/initialize` request. `events` is the wire form
/// (`HookEvent::as_str()`) of every event this extension's manifest
/// subscribes to.
pub fn initialize_request(id: i64, events: &[&str]) -> Value {
    stdio_jsonrpc::build_request(
        id,
        METHOD_INITIALIZE,
        Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "host": { "name": "ryuzi", "version": env!("CARGO_PKG_VERSION") },
            "events": events,
        })),
    )
}

/// Build the `extension/shutdown` request (no params).
pub fn shutdown_request(id: i64) -> Value {
    stdio_jsonrpc::build_request(id, METHOD_SHUTDOWN, None)
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
        let req = initialize_request(1, &["tool.before", "tool.after"]);
        assert_eq!(req["method"], METHOD_INITIALIZE);
        assert_eq!(req["id"], 1);
        assert_eq!(req["params"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            req["params"]["events"],
            json!(["tool.before", "tool.after"])
        );
    }

    #[test]
    fn shutdown_request_has_no_params() {
        let req = shutdown_request(2);
        assert_eq!(req["method"], METHOD_SHUTDOWN);
        assert!(req.get("params").is_none());
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
}
