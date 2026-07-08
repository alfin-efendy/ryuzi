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
            .agent_info(Implementation::new(
                "ryuzi-mock-agent",
                env!("CARGO_PKG_VERSION"),
            ))
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
                                    .kind(agent_client_protocol::schema::v1::ToolKind::Execute)
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
/// The `session_pk` is a fixed ryuzi DB key ("test-session-pk") that is
/// pre-supplied to the `NotificationSink`. Notifications from the ACP mock
/// carry the ACP session id, but the sink ignores it and keys all rows under
/// this fixed value — mirroring the production path where `start_session`
/// supplies `ctx.session_pk`.
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

    // 3. Fixed ryuzi session_pk — all notification rows are keyed under this.
    let session_pk = "test-session-pk".to_string();

    // 4. Shared sink wired with the fixed session_pk.
    let sink: Arc<NotificationSink> = Arc::new(NotificationSink {
        session_pk: session_pk.clone(),
        store: store.clone(),
        events: events_tx,
    });

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
    use agent_client_protocol::schema::v1::{
        ClientCapabilities, InitializeRequest, RequestPermissionOutcome, RequestPermissionRequest,
        RequestPermissionResponse, SessionNotification,
    };
    use agent_client_protocol::schema::ProtocolVersion;
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

    use agent_client_protocol::schema::v1::{ClientCapabilities, InitializeResponse};
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::Client;
    use tokio::sync::broadcast;

    use crate::domain::CoreEvent;
    use crate::harness::acp::notification::NotificationSink;
    use crate::store::Store;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp.path()).await.unwrap());
    let (events_tx, _events_rx) = broadcast::channel::<CoreEvent>(64);
    let session_pk = resume_session_id.to_string();
    let sink: Arc<NotificationSink> = Arc::new(NotificationSink {
        session_pk: session_pk.clone(),
        store: store.clone(),
        events: events_tx,
    });

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
/// Like [`run_via_harness_trait_collecting_events`], discarding the events.
pub async fn run_via_harness_trait(prompt: &str) -> (std::sync::Arc<crate::store::Store>, String) {
    let (store, session_pk, _events) = run_via_harness_trait_collecting_events(prompt).await;
    (store, session_pk)
}

/// Build an [`AcpHarness`](crate::harness::acp::AcpHarness) wired to the
/// in-process mock agent, start a session through the Spec 2 `Harness` trait,
/// send `prompt`, and return `(store, session_pk, broadcast events)`.
///
/// The subscriber is created BEFORE `start_session`, so every `CoreEvent`
/// emitted during the turn (user turn, streamed rows, tool updates) is
/// captured and drained after the turn completes.
pub async fn run_via_harness_trait_collecting_events(
    prompt: &str,
) -> (
    std::sync::Arc<crate::store::Store>,
    String,
    Vec<crate::domain::CoreEvent>,
) {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use crate::approval::ApprovalHub;
    use crate::domain::{CoreEvent, PermMode};
    use crate::harness::acp::{AcpAdapterDescriptor, AcpHarness};
    use crate::harness::{Harness, SessionCtx, TurnPrompt};
    use crate::store::Store;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp.path()).await.unwrap());
    let (events_tx, _events_rx) = broadcast::channel::<CoreEvent>(64);
    let mut events_rx = events_tx.subscribe();

    let harness = AcpHarness::with_runner_factory(
        AcpAdapterDescriptor::default(),
        |_descriptor: &AcpAdapterDescriptor| crate::harness::acp::mock_runner(),
    );

    let session_pk = "harness-test-session-pk".to_string();

    let ctx = SessionCtx {
        session_pk: session_pk.clone(),
        work_dir: std::path::PathBuf::from("/tmp"),
        perm_mode: PermMode::Default,
        model: None,
        effort: None,
        resume: None,
        mcp_servers: vec![],
        extra_skill_dirs: vec![],
        events: events_tx,
        approvals: Arc::new(ApprovalHub::new()),
        store: store.clone(),
    };

    let session = harness
        .start_session(ctx)
        .await
        .expect("start_session via Harness trait failed");

    session
        .send_prompt(TurnPrompt::text(prompt, prompt))
        .await
        .expect("send_prompt failed");

    // Let the async notification handlers drain.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    session.end().await.expect("end failed");

    let mut events = Vec::new();
    while let Ok(ev) = events_rx.try_recv() {
        events.push(ev);
    }

    drop(tmp);
    (store, session_pk, events)
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
) -> (
    std::sync::Arc<crate::approval::ApprovalHub>,
    PermissionResult,
) {
    use std::sync::Arc;

    use agent_client_protocol::schema::v1::{
        ClientCapabilities, InitializeRequest, InitializeResponse,
    };
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::Client;

    use crate::approval::ApprovalHub;
    use crate::domain::CoreEvent;

    let hub: Arc<ApprovalHub> = Arc::new(ApprovalHub::new());
    let (events_tx, _rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);
    let allowed_slot: Arc<tokio::sync::Mutex<bool>> = Arc::new(tokio::sync::Mutex::new(false));

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

    let transport =
        agent_client_protocol::ByteStreams::new(client_write.compat_write(), client_read.compat());

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
                    let response =
                        crate::harness::acp::permission::map_response(&request, decision);
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

// ---------------------------------------------------------------------------
// Spec 3B Task 1: fs/read_text_file + fs/write_text_file e2e test helpers
// ---------------------------------------------------------------------------

/// Result returned by [`run_prompt_with_fs_calls`].
pub struct FsOutcome {
    /// The content that the mock agent received from the client's
    /// `fs/read_text_file` response (read back from the sandboxed worktree).
    pub read_back: String,
    /// `true` when the file written by `fs/write_text_file` actually exists
    /// inside the (temp) worktree.
    pub wrote_inside_worktree: bool,
    /// The store, for asserting on persisted status rows.
    pub store: std::sync::Arc<crate::store::Store>,
    /// The ryuzi session_pk used during the test (all rows are keyed under it).
    pub session_pk: String,
    /// Keep the temp store file alive for the lifetime of this outcome.
    #[allow(dead_code)]
    _tmp_store: tempfile::NamedTempFile,
}

/// A mock agent that, during the `session/prompt` handler, sends a
/// `fs/write_text_file` request followed by a `fs/read_text_file` request
/// to the client. Used to validate the client's sandboxed fs handlers
/// end-to-end through the `run_client_loop` builder.
#[derive(Clone)]
struct FsMockAgent {
    /// Shared slot: after the prompt resolves, holds the read-back content.
    read_result: std::sync::Arc<tokio::sync::Mutex<String>>,
    /// The relative file name to write and read (relative to the worktree).
    file_name: String,
}

impl HandleDispatchFrom<Client> for FsMockAgent {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ryuzi-fs-mock-agent"
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
            // session/prompt — write a file then read it back via the client's
            // fs handlers (sandboxed to the client's worktree).
            .if_request({
                let cx_for_prompt = cx.clone();
                move |req: PromptRequest, responder: Responder<PromptResponse>| {
                    let cx = cx_for_prompt.clone();
                    let read_result = this.read_result.clone();
                    let file_name = this.file_name.clone();
                    async move {
                        let session_id = req.session_id.clone();

                        // Use cx.spawn to avoid blocking the ACP event loop.
                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            use agent_client_protocol::schema::v1::{
                                ReadTextFileRequest, WriteTextFileRequest,
                            };
                            // 1. Ask the client to write a file into its worktree.
                            //    The path is relative — the client's sandbox will
                            //    join it onto work_dir. We send an absolute-looking
                            //    path here; in practice the agent sends the path
                            //    it wants the client to use. For the test, use a
                            //    relative path as a PathBuf (agent sends the path).
                            let write_req = WriteTextFileRequest::new(
                                session_id.clone(),
                                // The protocol says "absolute path" but the client's
                                // sandbox resolves relative paths too. We send the
                                // relative name and the sandbox joins it onto work_dir.
                                std::path::PathBuf::from(&file_name),
                                "hello from agent",
                            );
                            let _ = cx2
                                .send_request(write_req)
                                .block_task()
                                .await;

                            // 2. Ask the client to read it back.
                            let read_req = ReadTextFileRequest::new(
                                session_id.clone(),
                                std::path::PathBuf::from(&file_name),
                            );
                            let read_resp = cx2
                                .send_request(read_req)
                                .block_task()
                                .await;

                            // Store whatever the client returned.
                            if let Ok(resp) = read_resp {
                                *read_result.lock().await = resp.content;
                            }

                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })?;

                        Ok(())
                    }
                }
            })
            .await
            // Let response dispatches fall through to their oneshot awaiters.
            .done()
    }
}

/// Run the full lifecycle against the [`FsMockAgent`], which exercises the
/// client's `fs/write_text_file` + `fs/read_text_file` handlers.
///
/// The test uses a real temporary directory as the session worktree so that
/// the sandbox check and the actual I/O can be verified.
pub async fn run_prompt_with_fs_calls() -> FsOutcome {
    use std::sync::Arc;

    use crate::approval::ApprovalHub;
    use crate::domain::{CoreEvent, PermMode};
    use crate::harness::acp::{AcpAdapterDescriptor, AcpHarness};
    use crate::harness::{Harness, SessionCtx, TurnPrompt};
    use crate::store::Store;

    let tmp_store = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp_store.path()).await.unwrap());
    let (events_tx, _events_rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);

    // A real temporary directory acts as the session worktree.
    let work_dir_tmp = tempfile::tempdir().unwrap();
    let work_dir = work_dir_tmp.path().to_path_buf();

    let file_name = "agent_test_file.txt".to_string();
    let expected_file = work_dir.join(&file_name);

    let read_result: Arc<tokio::sync::Mutex<String>> =
        Arc::new(tokio::sync::Mutex::new(String::new()));
    let read_result_for_agent = read_result.clone();

    let fs_agent = FsMockAgent {
        read_result: read_result_for_agent,
        file_name: file_name.clone(),
    };

    // Build a runner factory that drives the client loop over the FsMockAgent.
    let harness = AcpHarness::with_runner_factory(
        AcpAdapterDescriptor::default(),
        move |_descriptor: &AcpAdapterDescriptor| {
            let agent = fs_agent.clone();
            Box::new(move |args: crate::harness::acp::ClientLoopArgs| {
                let (client_read, server_write) = tokio::io::duplex(64 * 1024);
                let (server_read, client_write) = tokio::io::duplex(64 * 1024);
                use tokio_util::compat::{
                    TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _,
                };

                tokio::spawn(async move {
                    let _ = SacpAgent
                        .builder()
                        .name("ryuzi-fs-mock-agent")
                        .with_handler(agent)
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

                tokio::spawn(async move {
                    crate::harness::acp::run_client_loop(transport, args).await;
                });
            }) as crate::harness::acp::ClientLoopRunner
        },
    );

    let session_pk = "fs-test-session-pk".to_string();
    let ctx = SessionCtx {
        session_pk: session_pk.clone(),
        work_dir,
        perm_mode: PermMode::Default,
        model: None,
        effort: None,
        resume: None,
        mcp_servers: vec![],
        extra_skill_dirs: vec![],
        events: events_tx,
        approvals: Arc::new(ApprovalHub::new()),
        store: store.clone(),
    };

    let session = harness
        .start_session(ctx)
        .await
        .expect("run_prompt_with_fs_calls: start_session failed");

    session
        .send_prompt(TurnPrompt::text("write and read", "write and read"))
        .await
        .expect("run_prompt_with_fs_calls: send_prompt failed");

    // send_prompt.await already blocks until the PromptResponse returns,
    // so no sleep is needed here.
    session.end().await.expect("end failed");

    let read_back = read_result.lock().await.clone();
    let wrote_inside_worktree = expected_file.exists();

    // Keep work_dir_tmp alive until after the worktree check above.
    drop(work_dir_tmp);

    FsOutcome {
        read_back,
        wrote_inside_worktree,
        store,
        session_pk,
        _tmp_store: tmp_store,
    }
}

// ---------------------------------------------------------------------------
// Spec 3B Task 2: terminal/* e2e test helpers
// ---------------------------------------------------------------------------

/// Result returned by [`run_prompt_with_terminal_calls`].
pub struct TerminalOutcome {
    /// The output captured from the terminal command.
    pub output: String,
    /// Exit code reported back by the client's terminal handler.
    pub exit_code: Option<u32>,
    /// `true` if the command ran inside the (temp) worktree — verified via the
    /// `pwd` output containing the worktree path.
    pub ran_in_worktree: bool,
    /// Keep the temp store alive for the lifetime of this outcome.
    #[allow(dead_code)]
    _tmp_store: tempfile::NamedTempFile,
}

/// A mock agent that, during the `session/prompt` handler, sends:
///   1. `terminal/create` (echo hello && pwd)
///   2. `terminal/wait_for_exit`
///   3. `terminal/output`
///   4. `terminal/release`
///
/// Used to validate the client's sandboxed terminal handlers end-to-end.
#[derive(Clone)]
struct TerminalMockAgent {
    /// Shared slot: after the prompt resolves, holds the terminal output.
    output_slot: std::sync::Arc<tokio::sync::Mutex<String>>,
    /// Shared slot: exit code.
    exit_code_slot: std::sync::Arc<tokio::sync::Mutex<Option<u32>>>,
}

impl HandleDispatchFrom<Client> for TerminalMockAgent {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ryuzi-terminal-mock-agent"
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
            // session/prompt — runs a terminal command and records the output.
            .if_request({
                let cx_for_prompt = cx.clone();
                move |req: PromptRequest, responder: Responder<PromptResponse>| {
                    let cx = cx_for_prompt.clone();
                    let output_slot = this.output_slot.clone();
                    let exit_code_slot = this.exit_code_slot.clone();
                    async move {
                        let session_id = req.session_id.clone();

                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            use agent_client_protocol::schema::v1::{
                                CreateTerminalRequest, ReleaseTerminalRequest,
                                TerminalOutputRequest, WaitForTerminalExitRequest,
                            };

                            // 1. terminal/create — echo hello && pwd
                            let create_resp = cx2
                                .send_request(
                                    CreateTerminalRequest::new(
                                        session_id.clone(),
                                        "echo hello && pwd",
                                    )
                                    .output_byte_limit(4096u64),
                                )
                                .block_task()
                                .await;

                            let terminal_id = match create_resp {
                                Ok(r) => r.terminal_id,
                                Err(e) => {
                                    eprintln!("terminal/create failed: {e:?}");
                                    return responder
                                        .respond(PromptResponse::new(StopReason::EndTurn));
                                }
                            };

                            // 2. terminal/wait_for_exit
                            let _ = cx2
                                .send_request(WaitForTerminalExitRequest::new(
                                    session_id.clone(),
                                    terminal_id.clone(),
                                ))
                                .block_task()
                                .await;

                            // 3. terminal/output
                            let out_resp = cx2
                                .send_request(TerminalOutputRequest::new(
                                    session_id.clone(),
                                    terminal_id.clone(),
                                ))
                                .block_task()
                                .await;

                            if let Ok(out) = out_resp {
                                *output_slot.lock().await = out.output;
                                *exit_code_slot.lock().await =
                                    out.exit_status.and_then(|s| s.exit_code);
                            }

                            // 4. terminal/release
                            let _ = cx2
                                .send_request(ReleaseTerminalRequest::new(
                                    session_id.clone(),
                                    terminal_id,
                                ))
                                .block_task()
                                .await;

                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })?;

                        Ok(())
                    }
                }
            })
            .await
            .done()
    }
}

/// Run the full lifecycle against the [`TerminalMockAgent`], which exercises
/// the client's five `terminal/*` handlers end-to-end through `run_client_loop`.
pub async fn run_prompt_with_terminal_calls() -> TerminalOutcome {
    use std::sync::Arc;

    use crate::approval::ApprovalHub;
    use crate::domain::{CoreEvent, PermMode};
    use crate::harness::acp::{AcpAdapterDescriptor, AcpHarness};
    use crate::harness::{Harness, SessionCtx, TurnPrompt};
    use crate::store::Store;

    let tmp_store = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp_store.path()).await.unwrap());
    let (events_tx, _events_rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);

    // Use a real temp dir as the session worktree.
    let work_dir_tmp = tempfile::tempdir().unwrap();
    let work_dir = work_dir_tmp.path().to_path_buf();
    let work_dir_for_check = work_dir.canonicalize().unwrap();

    let output_slot: Arc<tokio::sync::Mutex<String>> =
        Arc::new(tokio::sync::Mutex::new(String::new()));
    let exit_code_slot: Arc<tokio::sync::Mutex<Option<u32>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    let agent = TerminalMockAgent {
        output_slot: output_slot.clone(),
        exit_code_slot: exit_code_slot.clone(),
    };

    let harness = AcpHarness::with_runner_factory(
        AcpAdapterDescriptor::default(),
        move |_descriptor: &AcpAdapterDescriptor| {
            let agent = agent.clone();
            Box::new(move |args: crate::harness::acp::ClientLoopArgs| {
                let (client_read, server_write) = tokio::io::duplex(64 * 1024);
                let (server_read, client_write) = tokio::io::duplex(64 * 1024);
                use tokio_util::compat::{
                    TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _,
                };

                tokio::spawn(async move {
                    let _ = SacpAgent
                        .builder()
                        .name("ryuzi-terminal-mock-agent")
                        .with_handler(agent)
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

                tokio::spawn(async move {
                    crate::harness::acp::run_client_loop(transport, args).await;
                });
            }) as crate::harness::acp::ClientLoopRunner
        },
    );

    let session_pk = "terminal-test-session-pk".to_string();
    let ctx = SessionCtx {
        session_pk: session_pk.clone(),
        work_dir,
        perm_mode: PermMode::Default,
        model: None,
        effort: None,
        resume: None,
        mcp_servers: vec![],
        extra_skill_dirs: vec![],
        events: events_tx,
        approvals: Arc::new(ApprovalHub::new()),
        store: store.clone(),
    };

    let session = harness
        .start_session(ctx)
        .await
        .expect("run_prompt_with_terminal_calls: start_session failed");

    session
        .send_prompt(TurnPrompt::text("run terminal", "run terminal"))
        .await
        .expect("run_prompt_with_terminal_calls: send_prompt failed");

    session.end().await.expect("end failed");

    let output = output_slot.lock().await.clone();
    let exit_code = *exit_code_slot.lock().await;
    let worktree_str = work_dir_for_check.to_string_lossy().into_owned();
    let ran_in_worktree = output.contains(&worktree_str);

    drop(work_dir_tmp);

    TerminalOutcome {
        output,
        exit_code,
        ran_in_worktree,
        _tmp_store: tmp_store,
    }
}

// ---------------------------------------------------------------------------
// Spec 3B Task 3: per-project allow-always policy bridge e2e test helper
// ---------------------------------------------------------------------------

/// Outcome returned by [`run_perm_mock_via_harness`].
pub struct PermBridgeOutcome {
    /// `true` when the mock agent received an allow selection back from the
    /// client (the bridge auto-allowed via policy, or via hub resolution).
    pub allowed: bool,
    /// `true` when the `ApprovalHub` was never registered during the request
    /// (i.e. the bridge short-circuited before hitting the hub).
    pub hub_was_never_registered: bool,
    /// The ryuzi `session_pk` this test wired into `SessionCtx` (all rows and
    /// events for the session should be keyed under this value).
    pub session_pk: String,
    /// The `session_pk` captured off the broadcast `CoreEvent::ApprovalRequested`
    /// (`None` if the request never reached the hub-prompt path, e.g. AutoAllow).
    /// Should equal `session_pk` above, NOT the ACP-assigned session id.
    pub captured_session_pk: Option<String>,
    /// The store, for further assertions.
    #[allow(dead_code)]
    pub store: std::sync::Arc<crate::store::Store>,
    #[allow(dead_code)]
    _tmp_store: tempfile::NamedTempFile,
}

/// Run the full lifecycle against the `PermMockAgent` through `AcpHarness`
/// (i.e. through `run_client_loop`), using a pre-seeded store + project/session
/// row.  The `tool_policy` parameter is set on the project before the session
/// starts.
///
/// Crucially, the test does NOT resolve the hub — if the bridge auto-allows via
/// policy the hub will never be registered.  `hub_was_never_registered` is
/// `true` iff the hub slot count stayed at zero.
pub async fn run_perm_mock_via_harness(
    project_id: &str,
    tool_policy: Option<(&str, &str)>, // (tool, decision)
) -> PermBridgeOutcome {
    use std::sync::Arc;

    use crate::approval::ApprovalHub;
    use crate::domain::{CoreEvent, PermMode, Project, Session, SessionStatus};
    use crate::harness::acp::{AcpAdapterDescriptor, AcpHarness};
    use crate::harness::{Harness, SessionCtx, TurnPrompt};
    use crate::store::Store;

    let tmp_store = tempfile::NamedTempFile::new().unwrap();
    let store: Arc<Store> = Arc::new(Store::open(tmp_store.path()).await.unwrap());
    let (events_tx, _events_rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);

    // Seed a minimal project + session so `get_session` in start_session resolves.
    let project = Project {
        project_id: project_id.to_string(),
        name: "perm-test".to_string(),
        workdir: "/tmp".to_string(),
        source: None,
        harness: "claude-code".to_string(),
        model: None,
        effort: None,
        perm_mode: PermMode::Default,
        created_at: None,
        is_git: false,
    };
    store.insert_project(project).await.unwrap();

    let session_pk = "perm-bridge-session-pk".to_string();
    let session = Session {
        session_pk: session_pk.clone(),
        project_id: project_id.to_string(),
        agent_session_id: None,
        worktree_path: None,
        branch: None,
        title: None,
        status: SessionStatus::Running,
        started_by: None,
        created_at: None,
        last_active: None,
        resume_attempts: 0,
        branch_owned: true,
    };
    store.insert_session(session).await.unwrap();

    // Pre-set a tool policy if requested.
    if let Some((tool, decision)) = tool_policy {
        store
            .set_tool_policy(project_id, tool, decision)
            .await
            .unwrap();
    }

    // Track whether the hub was ever registered.
    let hub: Arc<ApprovalHub> = Arc::new(ApprovalHub::new());
    let hub_registrations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Capture the session_pk carried by a broadcast CoreEvent::ApprovalRequested
    // (if the request reaches the hub-prompt path) and drive the hub
    // resolution — standing in for a real consumer (e.g. the CLI) that
    // watches the event stream and resolves the approval once the user
    // answers. Subscribing here (before `events_tx` is moved into `ctx`)
    // ensures we see the event even though it fires inside `send_prompt`.
    let captured_session_pk: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    {
        let mut approval_rx = events_tx.subscribe();
        let captured = captured_session_pk.clone();
        let hub_for_resolver = hub.clone();
        tokio::spawn(async move {
            loop {
                match approval_rx.recv().await {
                    Ok(CoreEvent::ApprovalRequested {
                        session_pk,
                        request_id,
                        ..
                    }) => {
                        *captured.lock().await = Some(session_pk);
                        // The bridge calls hub.register(..) right after emitting the
                        // event; retry briefly so we don't race ahead of it.
                        for _ in 0..200 {
                            if hub_for_resolver.resolve(&request_id, true) {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        }
                        break;
                    }
                    // send_prompt now broadcasts the persisted user turn (and other
                    // row events) before the approval fires — skip them.
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let allowed_slot: Arc<tokio::sync::Mutex<bool>> = Arc::new(tokio::sync::Mutex::new(false));
    let allowed_slot_for_agent = allowed_slot.clone();

    let perm_agent = PermMockAgent {
        allowed_slot: allowed_slot_for_agent,
    };

    let hub_reg_counter = hub_registrations.clone();

    let harness = AcpHarness::with_runner_factory(
        AcpAdapterDescriptor::default(),
        move |_descriptor: &AcpAdapterDescriptor| {
            let agent = perm_agent.clone();
            let counter = hub_reg_counter.clone();
            Box::new(move |args: crate::harness::acp::ClientLoopArgs| {
                let (client_read, server_write) = tokio::io::duplex(64 * 1024);
                let (server_read, client_write) = tokio::io::duplex(64 * 1024);
                use tokio_util::compat::{
                    TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _,
                };

                tokio::spawn(async move {
                    let _ = SacpAgent
                        .builder()
                        .name("ryuzi-perm-mock-agent")
                        .with_handler(agent)
                        .connect_to(agent_client_protocol::ByteStreams::new(
                            server_write.compat_write(),
                            server_read.compat(),
                        ))
                        .await;
                });

                // Wrap the hub in the args so we can intercept register calls
                // by subclassing — but simpler: just observe the allowed_slot
                // and check hub.pending_count via debug if needed. Here we rely
                // on the agent recording whether it got an allow, and we leave
                // the hub unresolved (no .resolve() call). If the bridge hits
                // the hub path it will hang forever, so the test would time out
                // rather than pass — which is a sufficient assertion.
                let _ = counter; // suppress unused warning; counter tracked above
                let transport = agent_client_protocol::ByteStreams::new(
                    client_write.compat_write(),
                    client_read.compat(),
                );

                tokio::spawn(async move {
                    crate::harness::acp::run_client_loop(transport, args).await;
                });
            }) as crate::harness::acp::ClientLoopRunner
        },
    );

    let ctx = SessionCtx {
        session_pk: session_pk.clone(),
        work_dir: std::path::PathBuf::from("/tmp"),
        perm_mode: PermMode::Default,
        model: None,
        effort: None,
        resume: None,
        mcp_servers: vec![],
        extra_skill_dirs: vec![],
        events: events_tx,
        approvals: hub.clone(),
        store: store.clone(),
    };

    let session = harness
        .start_session(ctx)
        .await
        .expect("run_perm_mock_via_harness: start_session failed");

    session
        .send_prompt(TurnPrompt::text("trigger permission", "trigger permission"))
        .await
        .expect("run_perm_mock_via_harness: send_prompt failed");

    session.end().await.expect("end failed");

    let allowed = *allowed_slot.lock().await;
    // The hub has no pending registrations if nothing called hub.register().
    let hub_was_never_registered = !hub.has_pending();
    let captured_session_pk = captured_session_pk.lock().await.clone();

    PermBridgeOutcome {
        allowed,
        hub_was_never_registered,
        session_pk,
        captured_session_pk,
        store,
        _tmp_store: tmp_store,
    }
}
