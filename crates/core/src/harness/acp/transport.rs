//! ACP client transport + connection + `initialize`.
//!
//! Task 1 owns the client-side round-trip against the external
//! `agent-client-protocol` 1.0 crate: build a `Client`, connect it over a
//! transport, send `initialize` (advertising **no** fs/terminal in 3A), and
//! read back the agent capabilities we care about (`session/load`,
//! `session/close`).
//!
//! Two seams matter here:
//! - Production spawns the adapter sidecar and builds a `ByteStreams` over its
//!   stdio (see [`spawn_adapter`]). That path is defined but unused in Task 1.
//! - Tests inject a duplex-backed transport from the testkit, so the whole
//!   builder + transport + `initialize` path is exercised without a real
//!   process.
//!
//! Both feed the same [`connect_and_initialize`], which is transport-agnostic
//! over any `impl ConnectTo<Client>`.

use agent_client_protocol::schema::v1::{
    ClientCapabilities, InitializeRequest, InitializeResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Client, ConnectionTo};
use agent_client_protocol_schema::v1::AGENT_METHOD_NAMES;

use super::{AcpAdapterDescriptor, Caps};

/// Spawn an ACP adapter sidecar per its [`AcpAdapterDescriptor`], with stdio
/// piped and `kill_on_drop` set. Defined for the production path; unused by the
/// Task 1 test path, which injects a duplex transport instead.
///
/// The caller is responsible for taking the child's stdin/stdout and building a
/// `ByteStreams` transport from them (write half = stdin, read half = stdout),
/// then handing that to [`connect_and_initialize`].
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

/// Client capabilities advertised in 3A: **no** fs read/write, **no** terminal.
/// (Tasks 3-4 turn these on once the fs/terminal handlers exist.)
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities::new()
}

/// Read the capabilities Task 1 cares about out of an `initialize` response.
fn extract_caps(response: &InitializeResponse) -> Caps {
    Caps {
        // Top-level bool on AgentCapabilities (wire `loadSession`), NOT a field
        // on SessionCapabilities.
        supports_load: response.agent_capabilities.load_session,
        // Presence of an optional `close` capability on SessionCapabilities.
        supports_close: response
            .agent_capabilities
            .session_capabilities
            .close
            .is_some(),
    }
}

/// Connect a `Client` over `transport`, run `initialize`, and return the agent
/// capabilities we gate on. Proves the full builder + transport + `initialize`
/// round-trip against the real crate API.
///
/// The two `on_receive_*` handlers are minimal stubs in Task 1 (they exist so
/// the builder is shaped correctly); the real notification and permission
/// handlers land in Tasks 3-4. They are written as inline closures so the
/// responder/notification types are inferred by the marker macros.
pub async fn connect_and_initialize(
    transport: impl agent_client_protocol::ConnectTo<Client> + 'static,
) -> Result<Caps, agent_client_protocol::Error> {
    Client
        .builder()
        .on_receive_notification(
            async move |_notification: SessionNotification, _cx| {
                // Task 1 does not consume session updates yet.
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: RequestPermissionRequest, responder, _connection_cx| {
                // Task 1 has no approval hub; decline permission requests until
                // the real handler lands in Task 4.
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<agent_client_protocol::Agent>| {
            let init_response: InitializeResponse = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::LATEST)
                        .client_capabilities(client_capabilities()),
                )
                .block_task()
                .await
                .map_err(|err| {
                    let message = format!("ACP {} failed: {err}", AGENT_METHOD_NAMES.initialize);
                    agent_client_protocol::Error::internal_error().data(message)
                })?;

            Ok(extract_caps(&init_response))
        })
        .await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn client_connects_and_initializes_against_mock_agent() {
        let (transport, _join) = crate::harness::acp::testkit::connect_mock(
            crate::harness::acp::testkit::MockAgent::new(),
        );
        let caps = super::connect_and_initialize(transport).await.unwrap();
        assert!(caps.supports_load, "mock advertises loadSession=true");
        assert!(caps.supports_close, "mock advertises a close capability");
    }
}
