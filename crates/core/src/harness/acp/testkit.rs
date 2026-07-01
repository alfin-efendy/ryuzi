//! In-process mock ACP agent + duplex transport, for validating the client's
//! transport/connection/`initialize` round-trip against the real
//! `agent-client-protocol` 1.0 API without spawning a real sidecar.
//!
//! Modeled on goose's `tests/acp_fixtures` (`serve_agent_in_process` + the
//! `HandleDispatchFrom<Client>` dispatch chain), pared down to only what
//! Task 1's `initialize` needs. Later tasks extend `MockAgent` to answer
//! `session/new`, `session/prompt`, etc.
//!
//! Task 2 extends `MockAgent` to answer `session/new`, `session/set_mode`,
//! and `session/prompt`. The `drive_lifecycle` helper runs the full
//! connect→initialize→new→set_mode→prompt sequence and returns a
//! `LifecycleOutcome` for test assertions.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, Implementation, InitializeRequest,
    InitializeResponse, McpCapabilities, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionCapabilities, SessionCloseCapabilities, SessionId, SessionMode,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionModeRequest,
    SetSessionModeResponse, StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields,
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

    /// Build the `SessionModeState` that the mock always advertises: three
    /// modes matching ryuzi's `PermMode` variants, with `default` active.
    fn mock_mode_state() -> SessionModeState {
        SessionModeState::new(
            "default",
            vec![
                SessionMode::new("default", "Default"),
                SessionMode::new("acceptEdits", "Accept Edits"),
                SessionMode::new("bypassPermissions", "Bypass Permissions"),
            ],
        )
    }

    fn new_session_response(session_id: impl Into<SessionId>) -> NewSessionResponse {
        NewSessionResponse::new(session_id).modes(Self::mock_mode_state())
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
            // initialize
            .if_request(
                |req: InitializeRequest, responder: Responder<InitializeResponse>| async move {
                    let response = this.initialize_response(&req);
                    responder.respond(response)
                },
            )
            .await
            // session/new — return a fresh session id + mode list
            .if_request(
                |_req: NewSessionRequest, responder: Responder<NewSessionResponse>| async move {
                    let session_id = uuid::Uuid::new_v4().to_string();
                    let response = MockAgent::new_session_response(session_id);
                    responder.respond(response)
                },
            )
            .await
            // session/set_mode — accept valid modes, reject unknown ones
            .if_request(
                |req: SetSessionModeRequest,
                 responder: Responder<SetSessionModeResponse>| async move {
                    let valid = ["default", "acceptEdits", "bypassPermissions"];
                    if valid.contains(&req.mode_id.0.as_ref()) {
                        responder.respond(SetSessionModeResponse::new())
                    } else {
                        responder.respond_with_error(
                            agent_client_protocol::Error::invalid_params()
                                .data(format!("unknown mode: {}", req.mode_id.0)),
                        )
                    }
                },
            )
            .await
            // session/prompt — send a few streaming notifications then return EndTurn
            .if_request({
                let cx_for_prompt = cx.clone();
                move |req: PromptRequest, responder: Responder<PromptResponse>| {
                    let cx = cx_for_prompt.clone();
                    async move {
                        let session_id = req.session_id.clone();

                        // 1. Agent message text chunk
                        let _ = cx.send_notification(SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("working")),
                            )),
                        ));

                        // 2. Tool call (pending)
                        let _ = cx.send_notification(SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::ToolCall(
                                ToolCall::new("tc-1", "Bash")
                                    .status(ToolCallStatus::Pending),
                            ),
                        ));

                        // 3. Tool call update (completed)
                        let _ = cx.send_notification(SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                                "tc-1",
                                ToolCallUpdateFields::new()
                                    .status(ToolCallStatus::Completed)
                                    .raw_output(serde_json::json!("output text")),
                            )),
                        ));

                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    }
                }
            })
            .await
            .otherwise(|message: Dispatch| async move {
                // Reject anything else so the client sees a clean protocol
                // error rather than a hang.
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

/// Outcome returned by [`drive_lifecycle`].
pub struct LifecycleOutcome {
    /// The `SessionId` assigned by the mock for the new session.
    pub session_id: SessionId,
    /// `true` when `session/prompt` returned a `StopReason` (any variant).
    pub completed: bool,
}

/// Run the full lifecycle, collect notifications into a temp store, and
/// return `(store, session_pk)` for test assertions.
///
/// The `session_pk` is the ACP `session_id` UUID string assigned by the mock
/// agent during `session/new`. Notifications arrive carrying that same UUID,
/// so the returned store will have rows keyed by `session_pk`.
pub async fn run_prompt_and_collect() -> (std::sync::Arc<crate::store::Store>, String) {
    use std::sync::Arc;

    use agent_client_protocol::schema::v1::{
        ClientCapabilities, InitializeRequest, InitializeResponse, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse,
    };
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::Client;
    use tokio::sync::broadcast;

    use crate::domain::CoreEvent;
    use crate::harness::acp::notification::NotificationSink;
    use crate::store::Store;

    // 1. Temp SQLite store.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp.path()).await.unwrap());

    // 2. Broadcast channel (we don't subscribe — just need the sender).
    let (events_tx, _events_rx) = broadcast::channel::<CoreEvent>(64);

    // 3. Shared sink (session_pk filled in below via Arc<Mutex<>>).
    //    We derive session_pk from the notification's session_id in the
    //    notification handler, so the sink itself doesn't need it.
    let sink: Arc<NotificationSink> = Arc::new(NotificationSink {
        store: store.clone(),
        events: events_tx,
    });

    // 4. Shared slot to capture the ACP session_id after session/new.
    let session_pk_slot: Arc<tokio::sync::Mutex<String>> =
        Arc::new(tokio::sync::Mutex::new(String::new()));
    let session_pk_out = session_pk_slot.clone();

    let (transport, _join) = connect_mock(MockAgent::new());

    Client
        .builder()
        .on_receive_notification(
            {
                let sink = sink.clone();
                async move |notification: SessionNotification, _cx| {
                    crate::harness::acp::notification::handle(notification, &sink).await;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: RequestPermissionRequest, responder, _cx| {
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            transport,
            async move |cx: agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>| {
                // initialize
                let _init: InitializeResponse = cx
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::LATEST)
                            .client_capabilities(ClientCapabilities::new()),
                    )
                    .block_task()
                    .await?;

                // session/new — capture the ACP session_id as our session_pk
                let session_resp = crate::harness::acp::lifecycle::new_session(
                    &cx,
                    std::path::PathBuf::from("/tmp"),
                    vec![],
                )
                .await?;
                let session_id = session_resp.session_id.clone();
                let pk = session_id.0.to_string();
                *session_pk_out.lock().await = pk.clone();

                // set_mode
                let available = session_resp
                    .modes
                    .as_ref()
                    .map(|m| m.available_modes.as_slice())
                    .unwrap_or(&[]);
                crate::harness::acp::lifecycle::set_mode(
                    &cx,
                    session_id.clone(),
                    "default",
                    available,
                )
                .await?;

                // prompt — the mock will send 3 notifications before EndTurn
                let content = vec![ContentBlock::Text(TextContent::new("hi"))];
                let (_stop, _usage) =
                    crate::harness::acp::lifecycle::prompt(&cx, session_id, content).await?;

                Ok(())
            },
        )
        .await
        .expect("run_prompt_and_collect: ACP lifecycle failed");

    // Give the async notification handlers a chance to complete.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let session_pk = session_pk_slot.lock().await.clone();
    // Keep tmp alive until after we've read the store.
    drop(tmp);
    (store, session_pk)
}

/// Run the full lifecycle sequence — connect → initialize → new_session →
/// set_mode → prompt — against the in-process mock agent and return the
/// outcome for test assertions.
///
/// `mode` is the ACP mode string to request (e.g. `"default"`).
/// `prompt_text` is the user message to send in the `session/prompt`.
pub async fn drive_lifecycle(
    mode: &str,
    prompt_text: &str,
) -> Result<LifecycleOutcome, agent_client_protocol::Error> {
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::{
        ClientCapabilities, InitializeRequest, RequestPermissionOutcome, RequestPermissionRequest,
        RequestPermissionResponse, SessionNotification,
    };
    use agent_client_protocol::Client;

    let mode = mode.to_string();
    let prompt_text = prompt_text.to_string();

    let (transport, _join) = connect_mock(MockAgent::new());

    Client
        .builder()
        .on_receive_notification(
            async move |_notification: SessionNotification, _cx| Ok(()),
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: RequestPermissionRequest, responder, _cx| {
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            transport,
            async move |cx: ConnectionTo<agent_client_protocol::Agent>| {
                // 1. initialize
                let _init: InitializeResponse = cx
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::LATEST)
                            .client_capabilities(ClientCapabilities::new()),
                    )
                    .block_task()
                    .await?;

                // 2. session/new
                let session_resp = crate::harness::acp::lifecycle::new_session(
                    &cx,
                    std::path::PathBuf::from("/tmp"),
                    vec![],
                )
                .await?;
                let session_id = session_resp.session_id.clone();

                // 3. set_mode — gather available modes from the response
                let available = session_resp
                    .modes
                    .as_ref()
                    .map(|m| m.available_modes.as_slice())
                    .unwrap_or(&[]);
                crate::harness::acp::lifecycle::set_mode(&cx, session_id.clone(), &mode, available)
                    .await?;

                // 4. prompt
                let content = vec![ContentBlock::Text(TextContent::new(prompt_text))];
                let (stop_reason, _usage) =
                    crate::harness::acp::lifecycle::prompt(&cx, session_id.clone(), content)
                        .await?;

                // StopReason is non_exhaustive but we just care that we got one.
                let _ = stop_reason;

                Ok(LifecycleOutcome {
                    session_id,
                    completed: true,
                })
            },
        )
        .await
}
