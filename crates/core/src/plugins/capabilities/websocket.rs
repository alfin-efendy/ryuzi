//! Host adapter for `ryuzi:websocket/websocket@0.1.0` (Task 1: plumbing only).
//!
//! This slice supplies a **stub** [`websocket_iface::Host`] impl so a component
//! that imports `ryuzi:websocket` can be *linked and instantiated* — proving
//! the capability's policy/linker/validation wiring end to end — without any
//! real socket behavior. Every method returns
//! [`websocket_iface::WsError::Failed`]; instantiation only needs the functions
//! linked, never called.
//!
//! The real per-`CapabilityState` connection registry (a `tokio-tungstenite`
//! TLS socket the host owns and the component drives via
//! `connect`/`send`/`poll`/`state`/`close`, plus the manifest-allowlist check
//! and per-instance handle/frame/buffer limits) lands in Task 2. See the design
//! doc §4.2.

use crate::plugins::capabilities::wit_bindings::websocket::ryuzi::websocket::websocket as websocket_iface;
use crate::plugins::runtime::CapabilityState;

/// The stub error every method returns until Task 2 replaces this adapter with
/// the real socket-owning implementation.
fn not_implemented() -> websocket_iface::WsError {
    websocket_iface::WsError::Failed("websocket capability not yet implemented".to_string())
}

impl websocket_iface::Host for CapabilityState {
    fn connect(
        &mut self,
        _url: String,
        _headers: Vec<websocket_iface::WsHeader>,
    ) -> Result<u64, websocket_iface::WsError> {
        Err(not_implemented())
    }

    fn send(
        &mut self,
        _handle: u64,
        _frame: websocket_iface::WsFrame,
    ) -> Result<(), websocket_iface::WsError> {
        Err(not_implemented())
    }

    fn poll(
        &mut self,
        _handle: u64,
    ) -> Result<Vec<websocket_iface::WsFrame>, websocket_iface::WsError> {
        Err(not_implemented())
    }

    fn state(
        &mut self,
        _handle: u64,
    ) -> Result<websocket_iface::WsState, websocket_iface::WsError> {
        Err(not_implemented())
    }

    fn close(&mut self, _handle: u64) -> Result<(), websocket_iface::WsError> {
        Err(not_implemented())
    }
}
