//! In-process mock ACP agent + duplex transport, for validating the client's
//! transport/connection/`initialize` round-trip against the real
//! `agent-client-protocol` 1.0 API without spawning a real sidecar.
//!
//! Modeled on goose's `tests/acp_fixtures` (`serve_agent_in_process` + the
//! `HandleDispatchFrom<Client>` dispatch chain), pared down to only what
//! Task 1's `initialize` needs. Later tasks extend `MockAgent` to answer
//! `session/new`, `session/prompt`, etc.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, Implementation, InitializeRequest, InitializeResponse, McpCapabilities,
    SessionCapabilities, SessionCloseCapabilities,
};
use agent_client_protocol::util::MatchDispatchFrom;
use agent_client_protocol::{
    Agent as SacpAgent, Client, ConnectionTo, Dispatch, HandleDispatchFrom, Handled, Responder,
};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

/// The concrete transport handed to the client: an `agent-client-protocol`
/// `ByteStreams` built over one end of a `tokio::io::duplex` pair.
pub type DuplexTransport = agent_client_protocol::ByteStreams<
    tokio_util::compat::Compat<tokio::io::DuplexStream>,
    tokio_util::compat::Compat<tokio::io::DuplexStream>,
>;

/// A minimal ACP `Agent`-role handler that answers `initialize` and rejects
/// everything else. Configurable so tests can assert on advertised caps.
#[derive(Clone)]
pub struct MockAgent {
    /// Value advertised for `agent_capabilities.load_session` (wire `loadSession`).
    load_session: bool,
    /// Whether to advertise a `session_capabilities.close` capability.
    supports_close: bool,
    /// Value advertised for `mcp_capabilities.http`.
    mcp_http: bool,
}

impl MockAgent {
    /// A mock advertising `loadSession=true`, a `close` capability, and
    /// `mcp.http=false` — the defaults Task 1's test asserts against.
    pub fn new() -> Self {
        Self {
            load_session: true,
            supports_close: true,
            mcp_http: false,
        }
    }

    fn initialize_response(&self, req: &InitializeRequest) -> InitializeResponse {
        let mut session_capabilities = SessionCapabilities::new();
        if self.supports_close {
            session_capabilities = session_capabilities.close(SessionCloseCapabilities::new());
        }

        let capabilities = AgentCapabilities::new()
            .load_session(self.load_session)
            .session_capabilities(session_capabilities)
            .mcp_capabilities(McpCapabilities::new().http(self.mcp_http));

        InitializeResponse::new(req.protocol_version)
            .agent_info(Implementation::new("ryuzi-mock-agent", env!("CARGO_PKG_VERSION")))
            .agent_capabilities(capabilities)
    }
}

impl Default for MockAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleDispatchFrom<Client> for MockAgent {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ryuzi-mock-agent"
    }

    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Client>,
    ) -> Result<Handled<Dispatch>, agent_client_protocol::Error> {
        let this = self.clone();
        MatchDispatchFrom::new(message, &cx)
            .if_request(
                |req: InitializeRequest, responder: Responder<InitializeResponse>| async move {
                    let response = this.initialize_response(&req);
                    responder.respond(response)
                },
            )
            .await
            .otherwise(|message: Dispatch| async move {
                // Task 1 only needs `initialize`; reject anything else so the
                // client sees a clean protocol error rather than a hang.
                if let Dispatch::Request(_req, responder) = message {
                    responder
                        .respond_with_error(agent_client_protocol::Error::method_not_found())?;
                }
                Ok(())
            })
            .await
            .map(|()| Handled::Yes)
    }
}

/// Serve `agent` over one end of a fresh `tokio::io::duplex` pair and return a
/// `ByteStreams` transport wired to the other end for the client, plus the
/// server task handle.
///
/// Mirrors goose's `serve_agent_in_process`: two duplex pairs (one per
/// direction), server reads/writes its ends, client gets the mirror.
pub fn connect_mock(agent: MockAgent) -> (DuplexTransport, tokio::task::JoinHandle<()>) {
    let (client_read, server_write) = tokio::io::duplex(64 * 1024);
    let (server_read, client_write) = tokio::io::duplex(64 * 1024);

    let join = tokio::spawn(async move {
        // Server role = Agent. `connect_to` runs the handler until the transport
        // closes (i.e. when the client drops its end after the test finishes).
        let result = SacpAgent
            .builder()
            .name("ryuzi-mock-agent")
            .with_handler(agent)
            .connect_to(agent_client_protocol::ByteStreams::new(
                server_write.compat_write(),
                server_read.compat(),
            ))
            .await;
        if let Err(err) = result {
            // Test-only diagnostic; the client dropping its transport at the end
            // of a test is the normal shutdown path and may surface here.
            eprintln!("mock ACP agent server exited: {err}");
        }
    });

    let transport =
        agent_client_protocol::ByteStreams::new(client_write.compat_write(), client_read.compat());
    (transport, join)
}
