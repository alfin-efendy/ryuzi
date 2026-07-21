//! First-party Discord gateway component.
//!
//! Exports the generic `ryuzi:gateway/gateway@0.1.0` interface
//! (`start`/`stop`/`deliver-outbound`/`health-check`/`poll-inbound`) and speaks
//! the raw Discord gateway v10 protocol over the host-owned `ryuzi:websocket`
//! transport (Discord REST over `ryuzi:http` follows in Task 10). The host owns
//! the TLS socket and enforces the manifest network allowlist; this component
//! only drives the protocol.
//!
//! # Architecture: pure `logic` vs. wasm `guest`
//! The entire Discord gateway protocol state machine — HELLO→IDENTIFY, the
//! heartbeat schedule + ACK tracking, READY session capture, sequence (`s`)
//! tracking, and the RECONNECT / INVALID_SESSION reconnect/resume decisions —
//! lives in the [`logic`] module as pure, deterministic functions over plain
//! Rust types with the clock passed in as a parameter, so it is exercised
//! entirely by native `cargo test` with synthetic gateway JSON frames and no
//! wasm host. The [`guest`] module (compiled only for `wasm32`) is thin glue: it
//! reads settings, drives `ryuzi:websocket` connect/poll/send, supplies a WASI
//! monotonic clock to the state machine, and maps the machine's emissions onto
//! the `ryuzi:gateway` export.
//!
//! Message normalization (Task 8) is now wired into [`logic::on_frame`]'s
//! `MESSAGE_CREATE` dispatch handling, reproducing the native inbound routing
//! rules exactly (`message.mention`/`message.thread`/`message.dm`). Slash
//! commands + approval-button routing (Task 9) are now wired into
//! [`logic::on_frame`]'s `INTERACTION_CREATE` dispatch handling
//! (`logic::handle_interaction`, `logic::can_approve`, `logic::build_commands`).
//! Discord REST + outbound-op wiring (Task 10) builds on this protocol core.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
