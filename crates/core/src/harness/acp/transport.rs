//! ACP client transport utilities: adapter spawn + capability extraction.
//!
//! The SINGLE site that assembles the `Client.builder()` chain — wiring
//! notification sink, permission handler, fs handlers, and capabilities — is
//! [`super::run_client_loop`] in `mod.rs`. This module only owns:
//!
//! - [`spawn_adapter`]: spawns the ACP adapter sidecar process (production).
//! - [`PermissionContext`]: the shared state bundle for the permission handler.
//!
//! The previous `connect_and_initialize` helper that duplicated the builder
//! assembly is removed. Its tests now go through the testkit helpers (which
//! drive `run_client_loop` via the mock runner), keeping a single builder site.

use std::sync::Arc;

use crate::domain::CoreEvent;

use super::AcpAdapterDescriptor;

/// Bundle of shared state threaded into the ACP client for the permission
/// handler. Defined here so callers can name the type concisely.
pub struct PermissionContext {
    pub hub: Arc<crate::approval::ApprovalHub>,
    pub events: tokio::sync::broadcast::Sender<CoreEvent>,
}

/// Spawn an ACP adapter sidecar per its [`AcpAdapterDescriptor`], with stdio
/// piped and `kill_on_drop` set. Used by the production runner in `mod.rs`.
///
/// The caller is responsible for taking the child's stdin/stdout and building
/// a `ByteStreams` transport from them (write half = stdin, read half = stdout).
pub async fn spawn_adapter(
    descriptor: &AcpAdapterDescriptor,
) -> std::io::Result<tokio::process::Child> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new(&descriptor.command);
    cmd.args(&descriptor.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for key in &descriptor.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in &descriptor.env {
        cmd.env(key, value);
    }

    cmd.spawn()
}


#[cfg(test)]
mod tests {
    /// The `client_connects_and_initializes` scenario is covered by the
    /// harness-trait test in `mod.rs`:
    ///   `acp_harness_starts_a_session_and_streams_via_the_harness_trait`
    /// which drives the full `run_client_loop` (including `initialize`) via
    /// the mock runner. No separate `connect_and_initialize` helper exists any
    /// more — that function duplicated the builder and is removed.

    #[tokio::test]
    async fn permission_request_is_answered_from_the_hub() {
        let (_hub, got) = crate::harness::acp::testkit::run_prompt_with_permission(
            crate::domain::ApprovalDecision::AllowOnce,
        )
        .await;
        assert!(got.allowed, "agent received an allow selection");
    }
}
