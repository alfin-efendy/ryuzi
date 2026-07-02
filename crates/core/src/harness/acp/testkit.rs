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
    InitializeResponse, LoadSessionRequest, LoadSessionResponse, McpCapabilities,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionCapabilities, SessionCloseCapabilities, SessionId, SessionMode, SessionModeState,
    SessionNotification, SessionUpdate, SetSessionModeRequest, SetSessionModeResponse, StopReason,
    TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
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
            // session/load — replay a user + agent message chunk as session/update
            // notifications (the resume transcript), then respond with an empty
            // LoadSessionResponse.
            .if_request({
                let cx_for_load = cx.clone();
                move |req: LoadSessionRequest, responder: Responder<LoadSessionResponse>| {
                    let cx = cx_for_load.clone();
                    async move {
                        let session_id = req.session_id.clone();

                        // Replay: the earlier user turn.
                        let _ = cx.send_notification(SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::UserMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("previous question")),
                            )),
                        ));

                        // Replay: the earlier agent reply.
                        let _ = cx.send_notification(SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("previous answer")),
                            )),
                        ));

                        responder.respond(LoadSessionResponse::new())
                    }
                }
            })
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

/// Drive a resume: connect → initialize → `session/load` against the in-process
/// mock agent, wiring the notification sink so the replayed transcript is
/// persisted. Returns `(store, session_pk)` for assertions.
///
/// The mock replays a user + agent message chunk as `session/update`
/// notifications during load; the sink persists the agent chunk as an
/// assistant text row (user chunks are currently skipped by the sink).
pub async fn drive_load(resume_session_id: &str) -> (std::sync::Arc<crate::store::Store>, String) {
    use std::sync::Arc;

    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::{ClientCapabilities, InitializeResponse};
    use agent_client_protocol::Client;
    use tokio::sync::broadcast;

    use crate::domain::CoreEvent;
    use crate::harness::acp::notification::NotificationSink;
    use crate::store::Store;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp.path()).await.unwrap());
    let (events_tx, _events_rx) = broadcast::channel::<CoreEvent>(64);
    let sink: Arc<NotificationSink> = Arc::new(NotificationSink {
        store: store.clone(),
        events: events_tx,
    });

    let session_pk = resume_session_id.to_string();
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
                // initialize (read supports_load off the response)
                let init: InitializeResponse = cx
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::LATEST)
                            .client_capabilities(ClientCapabilities::new()),
                    )
                    .block_task()
                    .await?;
                let supports_load = init.agent_capabilities.load_session;

                // session/load — the mock replays the transcript as notifications.
                crate::harness::acp::lifecycle::load_session(
                    &cx,
                    supports_load,
                    SessionId::from(session_pk.clone()),
                    std::path::PathBuf::from("/tmp"),
                    vec![],
                )
                .await?;

                Ok(())
            },
        )
        .await
        .expect("drive_load: ACP session/load failed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(tmp);
    (store, resume_session_id.to_string())
}

// ---------------------------------------------------------------------------
// Task 5: end-to-end Harness-trait test helper
// ---------------------------------------------------------------------------

/// Build an [`AcpHarness`](crate::harness::acp::AcpHarness) wired to the
/// in-process mock agent (via the test runner seam), start a session through
/// the Spec 2 `Harness` trait, send `prompt`, then return `(store, session_pk)`
/// for assertions. `session_pk` is the ACP `SessionId` the mock assigned.
///
/// The test runner spawns a tokio task (not an OS thread + fresh runtime) so
/// the mock duplex's I/O stays on the test runtime, and drives the shared
/// `run_client_loop` over the injected transport.
pub async fn run_via_harness_trait(
    prompt: &str,
) -> (std::sync::Arc<crate::store::Store>, String) {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use crate::approval::ApprovalHub;
    use crate::domain::{CoreEvent, PermMode};
    use crate::harness::acp::{AcpAdapterDescriptor, AcpHarness};
    use crate::harness::{Harness, SessionCtx};
    use crate::store::Store;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp.path()).await.unwrap());
    let (events_tx, _events_rx) = broadcast::channel::<CoreEvent>(64);

    // Test runner factory: for each session, produce a runner that drives the
    // shared client loop over a fresh mock duplex on a tokio task.
    let harness = AcpHarness::with_runner_factory(
        AcpAdapterDescriptor::default(),
        |_descriptor: &AcpAdapterDescriptor| {
            crate::harness::acp::mock_runner()
        },
    );

    let ctx = SessionCtx {
        session_pk: "unused-in-3a".into(),
        work_dir: std::path::PathBuf::from("/tmp"),
        perm_mode: PermMode::Default,
        model: None,
        effort: None,
        resume: None,
        mcp_servers: vec![],
        events: events_tx,
        approvals: Arc::new(ApprovalHub::new()),
        store: store.clone(),
    };

    let session = harness
        .start_session(ctx)
        .await
        .expect("start_session via Harness trait failed");

    session
        .send_prompt(prompt.to_string())
        .await
        .expect("send_prompt failed");

    // The agent session id is the ACP SessionId assigned during session/new.
    let session_pk = session
        .agent_session_id()
        .expect("agent_session_id should be present after start_session");

    // Let the async notification handlers drain.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    session.end().await.expect("end failed");
    drop(tmp);
    (store, session_pk)
}

// ---------------------------------------------------------------------------
// Task 4: permission-request test helpers
// ---------------------------------------------------------------------------

/// The option_id strings used in [`perm_request_with_kinds`].
pub const PERM_ALLOW_ONCE_ID: &str = "allow_once";
pub const PERM_REJECT_ONCE_ID: &str = "reject_once";

/// Build a [`RequestPermissionRequest`] offering `AllowOnce` + `RejectOnce`
/// options, for use in unit tests of [`crate::harness::acp::permission`].
pub fn perm_request_with_kinds() -> RequestPermissionRequest {
    let session_id = SessionId::from("test-session-0");
    let tool_call = ToolCallUpdate::new(
        "tc-perm-1",
        ToolCallUpdateFields::new().title("Bash".to_string()),
    );
    let options = vec![
        PermissionOption::new(PERM_ALLOW_ONCE_ID, "Allow once", PermissionOptionKind::AllowOnce),
        PermissionOption::new(PERM_REJECT_ONCE_ID, "Reject once", PermissionOptionKind::RejectOnce),
    ];
    RequestPermissionRequest::new(session_id, tool_call, options)
}

/// Returns `true` if `resp` is a `Selected` outcome with the allow-once option id.
pub fn is_selected_allow_once(resp: &RequestPermissionResponse) -> bool {
    match &resp.outcome {
        RequestPermissionOutcome::Selected(s) => s.option_id.0.as_ref() == PERM_ALLOW_ONCE_ID,
        _ => false,
    }
}

/// Returns `true` if `resp` is a `Cancelled` outcome.
pub fn is_cancelled(resp: &RequestPermissionResponse) -> bool {
    matches!(resp.outcome, RequestPermissionOutcome::Cancelled)
}

/// Outcome returned by [`run_prompt_with_permission`].
pub struct PermissionResult {
    /// `true` when the mock agent received an `allow_once` selection back from the
    /// client (i.e., the client routed the decision through the hub and produced
    /// the correct answer-by-kind response).
    pub allowed: bool,
}

/// A variant of [`MockAgent`] that sends a `request_permission` during the
/// prompt handler and records whether the client replied with an allow selection.
#[derive(Clone)]
struct PermMockAgent {
    /// Shared slot: after the prompt resolves, `true` means the client selected
    /// an allow-once option.
    allowed_slot: std::sync::Arc<tokio::sync::Mutex<bool>>,
}

impl HandleDispatchFrom<Client> for PermMockAgent {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ryuzi-perm-mock-agent"
    }

    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Client>,
    ) -> Result<Handled<Dispatch>, agent_client_protocol::Error> {
        let this = self.clone();
        let base = MockAgent::new();
        MatchDispatchFrom::new(message, &cx)
            // initialize
            .if_request(
                |req: InitializeRequest, responder: Responder<InitializeResponse>| async move {
                    responder.respond(base.initialize_response(&req))
                },
            )
            .await
            // session/new
            .if_request(
                |_req: NewSessionRequest, responder: Responder<NewSessionResponse>| async move {
                    let session_id = uuid::Uuid::new_v4().to_string();
                    responder.respond(MockAgent::new_session_response(session_id))
                },
            )
            .await
            // session/set_mode
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
            // session/prompt — spawn a task that sends request_permission then
            // responds to the prompt. We must NOT use block_task() directly in the
            // handler (it would deadlock the event loop); use cx.spawn() instead.
            .if_request({
                let cx_for_prompt = cx.clone();
                move |req: PromptRequest, responder: Responder<PromptResponse>| {
                    let cx = cx_for_prompt.clone();
                    let allowed_slot = this.allowed_slot.clone();
                    async move {
                        let session_id = req.session_id.clone();

                        // Build a permission request with AllowOnce + RejectOnce options.
                        let tool_call = ToolCallUpdate::new(
                            "tc-perm-1",
                            ToolCallUpdateFields::new().title("Bash".to_string()),
                        );
                        let options = vec![
                            PermissionOption::new(
                                PERM_ALLOW_ONCE_ID,
                                "Allow once",
                                PermissionOptionKind::AllowOnce,
                            ),
                            PermissionOption::new(
                                PERM_REJECT_ONCE_ID,
                                "Reject once",
                                PermissionOptionKind::RejectOnce,
                            ),
                        ];
                        let perm_req =
                            RequestPermissionRequest::new(session_id.clone(), tool_call, options);

                        // Use cx.spawn so block_task() doesn't deadlock the
                        // ACP event loop. The spawned task sends the permission
                        // request, records the outcome, and responds to the prompt.
                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            let perm_resp: RequestPermissionResponse = cx2
                                .send_request(perm_req)
                                .block_task()
                                .await
                                .unwrap_or_else(|_| {
                                    RequestPermissionResponse::new(
                                        RequestPermissionOutcome::Cancelled,
                                    )
                                });

                            // Record whether the client selected allow_once.
                            let allowed = matches!(
                                &perm_resp.outcome,
                                RequestPermissionOutcome::Selected(s)
                                    if s.option_id.0.as_ref() == PERM_ALLOW_ONCE_ID
                            );
                            *allowed_slot.lock().await = allowed;

                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })?;

                        // The spawned task will call responder.respond; return Ok here.
                        Ok(())
                    }
                }
            })
            .await
            // Responses (e.g. the session/request_permission reply that arrives
            // while the spawned task is awaiting block_task()) MUST be returned
            // as Handled::No so the dispatch loop's fallback routes them to the
            // correct oneshot awaiter.  Using `.done()` instead of `.otherwise`
            // achieves this: unhandled requests still get method_not_found from
            // the fallback in incoming_actor; unhandled responses get forwarded
            // to their oneshot via the ResponseRouter fallback.
            .done()
    }
}

/// Run the full lifecycle against the permission mock agent, resolve the approval
/// hub with `decision`, and return `(hub, PermissionResult)`.
///
/// The `decision` is applied as a binary bool to the hub:
/// `AllowOnce | AllowAlways` → `true` (allow), everything else → `false` (deny).
pub async fn run_prompt_with_permission(
    decision: crate::domain::ApprovalDecision,
) -> (std::sync::Arc<crate::approval::ApprovalHub>, PermissionResult) {
    use std::sync::Arc;

    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::{
        ClientCapabilities, InitializeRequest, InitializeResponse,
    };
    use agent_client_protocol::Client;

    use crate::approval::ApprovalHub;
    use crate::domain::CoreEvent;

    let hub: Arc<ApprovalHub> = Arc::new(ApprovalHub::new());
    let (events_tx, _rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);
    let allowed_slot: Arc<tokio::sync::Mutex<bool>> =
        Arc::new(tokio::sync::Mutex::new(false));

    let perm_agent = PermMockAgent {
        allowed_slot: allowed_slot.clone(),
    };

    // Shared state for the client side
    let hub_for_client = hub.clone();
    let events_for_client = events_tx.clone();

    let (client_read, server_write) = tokio::io::duplex(64 * 1024);
    let (server_read, client_write) = tokio::io::duplex(64 * 1024);
    use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

    let _server_join = tokio::spawn(async move {
        // Ignore the error: transport-closed errors are expected when the
        // client drops its end after the test completes.
        let _ = SacpAgent
            .builder()
            .name("ryuzi-perm-mock-agent")
            .with_handler(perm_agent)
            .connect_to(agent_client_protocol::ByteStreams::new(
                server_write.compat_write(),
                server_read.compat(),
            ))
            .await;
    });

    let transport = agent_client_protocol::ByteStreams::new(
        client_write.compat_write(),
        client_read.compat(),
    );

    // The binary allow/deny value derived from the decision.
    let allow = matches!(
        decision,
        crate::domain::ApprovalDecision::AllowOnce | crate::domain::ApprovalDecision::AllowAlways
    );

    Client
        .builder()
        .on_receive_notification(
            async move |_notification: SessionNotification, _cx| Ok(()),
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let hub = hub_for_client.clone();
                let events = events_for_client.clone();
                async move |request: RequestPermissionRequest, responder, _cx| {
                    let request_id = request.tool_call.tool_call_id.0.to_string();
                    let session_pk = request.session_id.0.to_string();
                    let tool = request
                        .tool_call
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    let summary = tool.clone();

                    let _ = events.send(CoreEvent::ApprovalRequested {
                        session_pk,
                        request_id: request_id.clone(),
                        tool,
                        summary,
                    });

                    // Register with the hub, then resolve immediately (binary 3A
                    // path: hub is already wired before the request arrives).
                    let rx = hub.register(request_id.clone());
                    hub.resolve(&request_id, allow);

                    let got_allow = rx.await.unwrap_or(false);
                    let decision = if got_allow {
                        crate::domain::ApprovalDecision::AllowOnce
                    } else {
                        crate::domain::ApprovalDecision::RejectOnce
                    };
                    let response = crate::harness::acp::permission::map_response(&request, decision);
                    responder.respond(response)
                }
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

                // session/new
                let session_resp = crate::harness::acp::lifecycle::new_session(
                    &cx,
                    std::path::PathBuf::from("/tmp"),
                    vec![],
                )
                .await?;
                let session_id = session_resp.session_id.clone();

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

                // prompt — the perm mock will send a request_permission before EndTurn
                let content = vec![ContentBlock::Text(TextContent::new("hi"))];
                let (_stop, _usage) =
                    crate::harness::acp::lifecycle::prompt(&cx, session_id, content).await?;

                Ok(())
            },
        )
        .await
        .expect("run_prompt_with_permission: ACP lifecycle failed");

    let allowed = *allowed_slot.lock().await;
    (hub, PermissionResult { allowed })
}
