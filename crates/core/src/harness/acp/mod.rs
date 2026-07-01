//! ACP (Agent Client Protocol) client foundation.
//!
//! Spec 3A / Task 1: client transport + `initialize`, validated against an
//! in-process mock ACP agent. This module owns the low-level round-trip against
//! the external `agent-client-protocol` 1.0 crate; higher layers (control plane,
//! session driver) land in later tasks.

pub mod transport;

#[cfg(test)]
mod testkit;

/// Static description of how to launch an ACP adapter sidecar (the bundled
/// Claude Code adapter, in production). Kept here so the transport layer can be
/// driven from host-injected config without pulling in process-spawn concerns at
/// the call site. Not exercised by the in-process test path (which injects a
/// duplex transport instead of spawning a process).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpAdapterDescriptor {
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Environment variables to set (key, value).
    pub env: Vec<(String, String)>,
    /// Environment variables to remove from the inherited environment.
    pub env_remove: Vec<String>,
}

/// Agent capabilities extracted from an `initialize` round-trip that the higher
/// layers care about in 3A. Deliberately small: we only read what the cutover
/// (Spec 3B) needs to gate `session/load` and `session/close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The agent advertises `session/load` (resume) — top-level
    /// `agent_capabilities.loadSession` bool.
    pub supports_load: bool,
    /// The agent advertises `session/close` — presence of
    /// `agent_capabilities.sessionCapabilities.close`.
    pub supports_close: bool,
}
