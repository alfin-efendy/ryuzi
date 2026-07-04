//! ACP (Agent Client Protocol) client foundation.
//!
//! Spec 3A: client transport + `initialize` (Task 1), session lifecycle
//! (Task 2), notification sink (Task 3), permission bridge (Task 4), and — in
//! this module — the [`AcpHarness`]/[`AcpSession`]/[`AcpHarnessFactory`] that
//! implement the Spec 2 `Harness` seam (Task 5).
//!
//! ## Client-loop architecture (the crux)
//!
//! `Builder::connect_with(transport, driver)` runs `driver(cx)` to **completion**
//! and returns its value — but a [`HarnessSession`] must `send_prompt` many times
//! *after* `start_session` returns, so the live `cx` must outlive it. The
//! solution (mirroring goose's `AcpProvider`): the `connect_with` driver runs a
//! request-draining loop. It performs the handshake (`initialize`, then
//! `session/new` + `set_mode` or `session/load`), signals readiness over a
//! `oneshot`, then blocks on `while let Some(req) = rx.recv().await { .. }` where
//! each [`ClientRequest`] performs one `cx.send_request(..).block_task().await`
//! round-trip and replies over a per-request `oneshot`. [`AcpSession`] holds the
//! `mpsc::Sender`; its methods enqueue a `ClientRequest` and await the reply.
//! Dropping the sender ends the loop (and the connection).
//!
//! The loop must run on a home that owns the transport's tokio I/O: in
//! production a **dedicated OS thread with a current-thread runtime** (tokio I/O
//! handles can't cross runtimes) driving the spawned sidecar's stdio; in tests a
//! plain tokio task over an injected duplex transport. This split is the
//! [`ClientLoopRunner`] seam.

pub mod fs;
pub mod lifecycle;
pub mod notification;
pub mod permission;
pub mod terminal;
pub mod transport;

#[cfg(test)]
pub(crate) mod testkit;

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, CreateTerminalRequest,
    CreateTerminalResponse, FileSystemCapabilities, InitializeRequest, InitializeResponse,
    KillTerminalRequest, KillTerminalResponse, ReadTextFileRequest, ReleaseTerminalRequest,
    ReleaseTerminalResponse, SessionId, TerminalOutputRequest, TerminalOutputResponse, TextContent,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Client, ConnectionTo};
use agent_client_protocol_schema::v1::AGENT_METHOD_NAMES;
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::domain::{CoreEvent, NewMessage};
use crate::harness::acp::notification::NotificationSink;
use crate::harness::acp::transport::PermissionContext;
use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
use crate::store::Store;

/// One request the [`AcpSession`] hands to the client loop. Each carries a
/// reply channel; the loop performs the round-trip and answers.
enum ClientRequest {
    /// Send `session/prompt` and report the resulting [`StopReason`] (via Debug
    /// string, since it is `#[non_exhaustive]`).
    Prompt {
        content: Vec<ContentBlock>,
        reply: oneshot::Sender<anyhow::Result<String>>,
    },
    /// Send a `session/cancel` notification for the current session.
    Cancel { reply: oneshot::Sender<()> },
}

/// Result of the loop's startup handshake, delivered over a `oneshot` once the
/// session is established (or an error if the handshake failed).
struct Ready {
    session_id: SessionId,
}

/// Boxed driver for the `connect_with` loop. Lets [`AcpHarness::start_session`]
/// share all lifecycle logic while the transport home differs: a dedicated
/// thread plus sidecar in production; a tokio task over an injected duplex in
/// tests. The runner builds/owns the transport, then calls [`run_client_loop`].
///
/// Fire-and-forget: the loop's lifetime is bound by the `mpsc::Sender` the
/// session holds — when it drops, `rx.recv()` returns `None`, the loop exits,
/// and the transport (and, in production, the sidecar) is torn down.
pub(crate) type ClientLoopRunner = Box<dyn FnOnce(ClientLoopArgs) + Send>;

/// Everything [`run_client_loop`] needs, bundled so a [`ClientLoopRunner`] can
/// forward them across a thread boundary.
pub(crate) struct ClientLoopArgs {
    rx: mpsc::Receiver<ClientRequest>,
    ready_tx: oneshot::Sender<anyhow::Result<Ready>>,
    sink: Arc<NotificationSink>,
    perm: PermissionContext,
    resume: Option<String>,
    perm_mode: crate::domain::PermMode,
    work_dir: std::path::PathBuf,
    /// MCP servers to attach at session/new (and session/load on resume).
    mcp_servers: Vec<crate::domain::McpServerSpec>,
}

/// The `connect_with` driver: run the lifecycle handshake, signal readiness,
/// then drain [`ClientRequest`]s until the sender is dropped. Transport-agnostic
/// over any `impl ConnectTo<Client>`, so both the production and test runners
/// share it.
async fn run_client_loop(
    transport: impl agent_client_protocol::ConnectTo<Client> + 'static,
    args: ClientLoopArgs,
) {
    let ClientLoopArgs {
        mut rx,
        ready_tx,
        sink,
        perm,
        resume,
        perm_mode,
        work_dir,
        mcp_servers,
    } = args;
    let acp_mcp_servers: Vec<agent_client_protocol_schema::v1::McpServer> =
        mcp_servers.iter().map(crate::mcp::to_acp).collect();

    let sink_for_handler = sink.clone();
    let sink_for_write = sink.clone();
    let perm = (
        perm.hub,
        perm.events,
        perm.session_pk,
        perm.project_id,
        perm.perm_mode,
        perm.store,
    );
    // Clone work_dir for the fs/terminal handler closures (they must be 'static + Send).
    let work_dir_for_read = work_dir.clone();
    let work_dir_for_write = work_dir.clone();
    let work_dir_for_terminal_create = work_dir.clone();

    // One TerminalManager per session — shared across the five terminal handlers.
    let term_mgr = std::sync::Arc::new(crate::harness::acp::terminal::TerminalManager::new());
    let term_mgr_create = term_mgr.clone();
    let term_mgr_output = term_mgr.clone();
    let term_mgr_wait = term_mgr.clone();
    let term_mgr_kill = term_mgr.clone();
    let term_mgr_release = term_mgr.clone();

    // Wrap ready_tx in an Option so it can be consumed in either the error
    // path (where we forward the real cause) or the success path.
    let mut ready_tx_opt: Option<oneshot::Sender<anyhow::Result<Ready>>> = Some(ready_tx);

    let result = Client
        .builder()
        .on_receive_notification(
            {
                let sink = sink_for_handler;
                async move |notification: agent_client_protocol::schema::v1::SessionNotification,
                            _cx| {
                    crate::harness::acp::notification::handle(notification, &sink).await;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let (
                    hub,
                    events,
                    session_pk_for_perm,
                    project_id,
                    perm_mode_for_perm,
                    store_for_perm,
                ) = perm;
                async move |request: agent_client_protocol::schema::v1::RequestPermissionRequest,
                            responder,
                            _cx| {
                    let request_id = request.tool_call.tool_call_id.0.to_string();
                    // Ryuzi's own session_pk (NOT `request.session_id`, which is the
                    // ACP-assigned id) — consumers route ApprovalRequested by ryuzi
                    // pk (see e.g. `run_cmd.rs`'s `session_pk == session.session_pk`
                    // guard and the daemon fan-out), mirroring the notification sink.
                    let session_pk = session_pk_for_perm.clone();
                    let tool = request
                        .tool_call
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    let summary = tool.clone();

                    // MCP tool permissions from the Apps screen take precedence:
                    // allow → silent, deny → silent reject, ask → prompt.
                    match crate::mcp::tool_perm_for_title(&store_for_perm, &tool)
                        .await
                        .as_deref()
                    {
                        Some("allow") => {
                            let response = crate::harness::acp::permission::map_response(
                                &request,
                                crate::domain::ApprovalDecision::AllowOnce,
                            );
                            return responder.respond(response);
                        }
                        Some("deny") => {
                            let response = crate::harness::acp::permission::map_response(
                                &request,
                                crate::domain::ApprovalDecision::RejectOnce,
                            );
                            return responder.respond(response);
                        }
                        _ => {}
                    }

                    // Consult the per-project tool policy before prompting.
                    let project_policy = store_for_perm
                        .get_tool_policy(&project_id, &tool)
                        .await
                        .unwrap_or(None);
                    let outcome = crate::policy::decide_tool_permission(
                        perm_mode_for_perm,
                        project_policy.as_deref(),
                        &tool,
                    );

                    if outcome == crate::policy::PolicyOutcome::AutoAllow {
                        // Policy says auto-allow: respond immediately without
                        // prompting the user through the hub.
                        let response = crate::harness::acp::permission::map_response(
                            &request,
                            crate::domain::ApprovalDecision::AllowOnce,
                        );
                        return responder.respond(response);
                    }

                    let _ = events.send(CoreEvent::ApprovalRequested {
                        session_pk,
                        request_id: request_id.clone(),
                        tool,
                        summary,
                    });

                    let rx = hub.register(request_id.clone());
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
        // fs/read_text_file — sandboxed to the session worktree.
        .on_receive_request(
            {
                async move |request: ReadTextFileRequest, responder, _cx| {
                    let result =
                        crate::harness::acp::fs::read_text_file(&work_dir_for_read, request);
                    match result {
                        Ok(response) => responder.respond(response),
                        Err(err) => {
                            tracing::warn!("fs/read_text_file failed: {err}");
                            responder.respond_with_error(
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("fs/read_text_file: {err}")),
                            )
                        }
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // fs/write_text_file — sandboxed to the session worktree.
        .on_receive_request(
            {
                async move |request: WriteTextFileRequest, responder, _cx| {
                    // Capture a display path for the status event before the
                    // request is consumed by write_text_file.
                    let display_path = request.path.display().to_string();
                    let result =
                        crate::harness::acp::fs::write_text_file(&work_dir_for_write, request);
                    match result {
                        Ok(response) => {
                            // Persist a real-seq status row and emit CoreEvent.
                            sink_for_write
                                .record_status(format!("wrote {display_path}"))
                                .await;
                            responder.respond(response)
                        }
                        Err(err) => {
                            tracing::warn!("fs/write_text_file failed: {err}");
                            responder.respond_with_error(
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("fs/write_text_file: {err}")),
                            )
                        }
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // terminal/create — sandbox cwd to the session worktree, spawn the PTY.
        .on_receive_request(
            {
                async move |request: CreateTerminalRequest, responder, _cx| {
                    let sandboxed_cwd = crate::harness::acp::terminal::sandbox_cwd(
                        &work_dir_for_terminal_create,
                        request.cwd,
                    );
                    let sandboxed_cwd = match sandboxed_cwd {
                        Ok(p) => p,
                        Err(err) => {
                            tracing::error!("terminal/create sandbox failed: {err}");
                            return responder.respond_with_error(
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("terminal/create: {err}")),
                            );
                        }
                    };
                    let output_byte_limit = request.output_byte_limit.unwrap_or(1024 * 1024);
                    match term_mgr_create.create(&request.command, sandboxed_cwd, output_byte_limit)
                    {
                        Ok(terminal_id) => {
                            responder.respond(CreateTerminalResponse::new(terminal_id))
                        }
                        Err(err) => {
                            tracing::warn!("terminal/create failed: {err}");
                            responder.respond_with_error(
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("terminal/create: {err}")),
                            )
                        }
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // terminal/output — return accumulated output and exit status.
        .on_receive_request(
            {
                async move |request: TerminalOutputRequest, responder, _cx| match term_mgr_output
                    .output(&request.terminal_id)
                {
                    Ok(out) => {
                        let mut resp = TerminalOutputResponse::new(out.output, out.truncated);
                        if let Some(status) = out.exit_status {
                            resp = resp.exit_status(status);
                        }
                        responder.respond(resp)
                    }
                    Err(err) => {
                        tracing::warn!("terminal/output failed: {err}");
                        responder.respond_with_error(
                            agent_client_protocol::Error::internal_error()
                                .data(format!("terminal/output: {err}")),
                        )
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // terminal/wait_for_exit — async wait until the process exits.
        .on_receive_request(
            {
                async move |request: WaitForTerminalExitRequest, responder, _cx| {
                    match term_mgr_wait.wait_for_exit(&request.terminal_id).await {
                        Ok(()) => {
                            // Return a WaitForTerminalExitResponse with the exit status.
                            // We must fetch it from output() to get the code.
                            let exit_status = term_mgr_wait
                                .output(&request.terminal_id)
                                .ok()
                                .and_then(|o| o.exit_status)
                                .unwrap_or_default();
                            responder.respond(WaitForTerminalExitResponse::new(exit_status))
                        }
                        Err(err) => {
                            tracing::warn!("terminal/wait_for_exit failed: {err}");
                            responder.respond_with_error(
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("terminal/wait_for_exit: {err}")),
                            )
                        }
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // terminal/kill — send SIGKILL to the child process.
        .on_receive_request(
            {
                async move |request: KillTerminalRequest, responder, _cx| match term_mgr_kill
                    .kill(&request.terminal_id)
                {
                    Ok(()) => responder.respond(KillTerminalResponse::new()),
                    Err(err) => {
                        tracing::warn!("terminal/kill failed: {err}");
                        responder.respond_with_error(
                            agent_client_protocol::Error::internal_error()
                                .data(format!("terminal/kill: {err}")),
                        )
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // terminal/release — free the terminal's resources.
        .on_receive_request(
            {
                async move |request: ReleaseTerminalRequest, responder, _cx| match term_mgr_release
                    .release(&request.terminal_id)
                {
                    Ok(()) => responder.respond(ReleaseTerminalResponse::new()),
                    Err(err) => {
                        tracing::warn!("terminal/release failed: {err}");
                        responder.respond_with_error(
                            agent_client_protocol::Error::internal_error()
                                .data(format!("terminal/release: {err}")),
                        )
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            transport,
            async move |cx: ConnectionTo<agent_client_protocol::Agent>| {
                // --- handshake: initialize (advertising fs read+write + terminal) ---------
                let init: InitializeResponse = cx
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::LATEST).client_capabilities(
                            ClientCapabilities::new()
                                .fs(FileSystemCapabilities::new()
                                    .read_text_file(true)
                                    .write_text_file(true))
                                .terminal(true),
                        ),
                    )
                    .block_task()
                    .await
                    .map_err(|err| {
                        let msg = format!("ACP {} failed: {err}", AGENT_METHOD_NAMES.initialize);
                        if let Some(tx) = ready_tx_opt.take() {
                            let _ = tx.send(Err(anyhow::anyhow!("{msg}")));
                        }
                        agent_client_protocol::Error::internal_error().data(msg)
                    })?;
                let supports_load = init.agent_capabilities.load_session;

                // --- establish the session: load (resume) or new + set_mode ----
                let session_id = if let Some(ref resume_id) = resume {
                    if supports_load {
                        let sid = SessionId::from(resume_id.clone());
                        crate::harness::acp::lifecycle::load_session(
                            &cx,
                            supports_load,
                            sid.clone(),
                            work_dir.clone(),
                            acp_mcp_servers.clone(),
                        )
                        .await
                        .map_err(|err| {
                            let msg = format!("ACP session/load failed: {err}");
                            if let Some(tx) = ready_tx_opt.take() {
                                let _ = tx.send(Err(anyhow::anyhow!("{msg}")));
                            }
                            err
                        })?;
                        sid
                    } else {
                        // Agent can't resume; fall back to a fresh session.
                        fresh_session_propagating(
                            &cx,
                            work_dir.clone(),
                            perm_mode,
                            acp_mcp_servers.clone(),
                            &mut ready_tx_opt,
                        )
                        .await?
                    }
                } else {
                    fresh_session_propagating(
                        &cx,
                        work_dir.clone(),
                        perm_mode,
                        acp_mcp_servers.clone(),
                        &mut ready_tx_opt,
                    )
                    .await?
                };

                // Signal readiness back to start_session.
                if let Some(tx) = ready_tx_opt.take() {
                    let _ = tx.send(Ok(Ready {
                        session_id: session_id.clone(),
                    }));
                }

                // --- drain ClientRequests until the sender is dropped ----------
                while let Some(req) = rx.recv().await {
                    match req {
                        ClientRequest::Prompt { content, reply } => {
                            let outcome = crate::harness::acp::lifecycle::prompt(
                                &cx,
                                session_id.clone(),
                                content,
                            )
                            .await;
                            let result = outcome
                                .map(|(stop, _usage)| format!("{stop:?}"))
                                .map_err(|e| anyhow::anyhow!("{e}"));
                            let _ = reply.send(result);
                        }
                        ClientRequest::Cancel { reply } => {
                            let _ =
                                cx.send_notification(CancelNotification::new(session_id.clone()));
                            let _ = reply.send(());
                        }
                    }
                }

                // Session is ending — release all terminal resources.
                term_mgr.release_all();

                Ok::<(), agent_client_protocol::Error>(())
            },
        )
        .await;

    if let Err(err) = result {
        tracing::warn!("ACP client loop exited: {err}");
    }
}

/// `session/new` + `set_mode` with handshake-error propagation over `ready_tx`.
///
/// Like [`fresh_session`] but sends the error through `ready_tx_opt` before
/// returning `Err` so `start_session` sees the real cause instead of a generic
/// "loop ended before ready" message.
async fn fresh_session_propagating(
    cx: &ConnectionTo<agent_client_protocol::Agent>,
    work_dir: std::path::PathBuf,
    perm_mode: crate::domain::PermMode,
    mcp_servers: Vec<agent_client_protocol_schema::v1::McpServer>,
    ready_tx_opt: &mut Option<oneshot::Sender<anyhow::Result<Ready>>>,
) -> Result<SessionId, agent_client_protocol::Error> {
    let session_resp = crate::harness::acp::lifecycle::new_session(cx, work_dir, mcp_servers)
        .await
        .map_err(|err| {
            let msg = format!("ACP session/new failed: {err}");
            if let Some(tx) = ready_tx_opt.take() {
                let _ = tx.send(Err(anyhow::anyhow!("{msg}")));
            }
            err
        })?;
    let session_id = session_resp.session_id.clone();

    let mode_id = crate::harness::acp::lifecycle::perm_mode_to_acp_mode(perm_mode);
    let available = session_resp
        .modes
        .as_ref()
        .map(|m| m.available_modes.as_slice())
        .unwrap_or(&[]);
    // set_mode is best-effort: if the agent didn't offer the mode, stay put.
    let _ =
        crate::harness::acp::lifecycle::set_mode(cx, session_id.clone(), mode_id, available).await;

    Ok(session_id)
}

/// Production [`ClientLoopRunner`]: spawn a dedicated OS thread with a
/// current-thread tokio runtime, spawn the adapter sidecar there (its tokio I/O
/// can't cross runtimes), wrap its stdio in `ByteStreams`, and drive the loop.
fn spawn_thread_runner(descriptor: AcpAdapterDescriptor) -> ClientLoopRunner {
    Box::new(move |args: ClientLoopArgs| {
        // Detached: the loop ends when the session's sender drops.
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = args
                        .ready_tx
                        .send(Err(anyhow::anyhow!("failed to build ACP runtime: {e}")));
                    return;
                }
            };
            rt.block_on(async move {
                use tokio_util::compat::{
                    TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _,
                };

                let mut child = match transport::spawn_adapter(&descriptor).await {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = args
                            .ready_tx
                            .send(Err(anyhow::anyhow!("failed to spawn ACP adapter: {e}")));
                        return;
                    }
                };
                let stdin = match child.stdin.take() {
                    Some(s) => s,
                    None => {
                        let _ = args
                            .ready_tx
                            .send(Err(anyhow::anyhow!("adapter has no stdin")));
                        return;
                    }
                };
                let stdout = match child.stdout.take() {
                    Some(s) => s,
                    None => {
                        let _ = args
                            .ready_tx
                            .send(Err(anyhow::anyhow!("adapter has no stdout")));
                        return;
                    }
                };
                let byte_streams =
                    agent_client_protocol::ByteStreams::new(stdin.compat_write(), stdout.compat());
                run_client_loop(byte_streams, args).await;
                let _ = child.kill().await;
                let _ = child.wait().await;
            });
        });
    })
}

/// Test-only [`ClientLoopRunner`]: drive the shared [`run_client_loop`] over a
/// fresh in-process mock duplex on a tokio task (so the duplex's I/O stays on
/// the test runtime rather than a separate current-thread runtime).
#[cfg(test)]
pub(crate) fn mock_runner() -> ClientLoopRunner {
    Box::new(|args: ClientLoopArgs| {
        let (transport, _join) = crate::harness::acp::testkit::connect_mock(
            crate::harness::acp::testkit::MockAgent::new(),
        );
        tokio::spawn(async move {
            run_client_loop(transport, args).await;
        });
    })
}

/// A registered ACP harness: implements the Spec 2 [`Harness`] seam over the
/// bundled adapter [`AcpAdapterDescriptor`]. In tests the transport home is
/// swapped for an injected duplex via [`AcpHarness::with_runner_factory`].
pub struct AcpHarness {
    descriptor: AcpAdapterDescriptor,
    /// Seam producing a [`ClientLoopRunner`] per session. Production spawns the
    /// sidecar on a dedicated thread; tests inject a duplex-backed runner.
    runner_factory: Box<dyn Fn(&AcpAdapterDescriptor) -> ClientLoopRunner + Send + Sync>,
}

impl AcpHarness {
    /// Production constructor: sessions spawn the adapter sidecar described by
    /// `descriptor` on a dedicated thread.
    pub fn new(descriptor: AcpAdapterDescriptor) -> Self {
        Self {
            descriptor,
            runner_factory: Box::new(|d: &AcpAdapterDescriptor| spawn_thread_runner(d.clone())),
        }
    }

    /// Test seam: build an `AcpHarness` whose sessions run the client loop via a
    /// caller-supplied [`ClientLoopRunner`] factory (e.g. one that drives an
    /// in-process mock over a duplex transport instead of spawning a process).
    #[cfg(test)]
    pub(crate) fn with_runner_factory(
        descriptor: AcpAdapterDescriptor,
        factory: impl Fn(&AcpAdapterDescriptor) -> ClientLoopRunner + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            runner_factory: Box::new(factory),
        }
    }
}

#[async_trait]
impl Harness for AcpHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        let sink = Arc::new(NotificationSink {
            session_pk: ctx.session_pk.clone(),
            store: ctx.store.clone(),
            events: ctx.events.clone(),
        });

        // Resolve the project_id for per-project policy lookups. If the session
        // is not found (e.g. in tests that don't pre-seed the store), fall back
        // to an empty string — the lookup will simply return None.
        let project_id = ctx
            .store
            .get_session(&ctx.session_pk)
            .await
            .ok()
            .flatten()
            .map(|s| s.project_id)
            .unwrap_or_default();

        let perm = PermissionContext {
            hub: ctx.approvals.clone(),
            events: ctx.events.clone(),
            session_pk: ctx.session_pk.clone(),
            project_id,
            perm_mode: ctx.perm_mode,
            store: ctx.store.clone(),
        };

        let (tx, rx) = mpsc::channel::<ClientRequest>(32);
        let (ready_tx, ready_rx) = oneshot::channel::<anyhow::Result<Ready>>();

        let args = ClientLoopArgs {
            rx,
            ready_tx,
            sink,
            perm,
            resume: ctx.resume.clone(),
            perm_mode: ctx.perm_mode,
            work_dir: ctx.work_dir.clone(),
            mcp_servers: ctx.mcp_servers.clone(),
        };

        let runner = (self.runner_factory)(&self.descriptor);
        runner(args);

        // Wait for the handshake (initialize + session established) to finish.
        let ready = ready_rx
            .await
            .map_err(|_| anyhow::anyhow!("ACP client loop ended before session was ready"))??;

        Ok(Box::new(AcpSession {
            tx: tokio::sync::Mutex::new(Some(tx)),
            session_pk: ctx.session_pk.clone(),
            session_id: ready.session_id,
            store: ctx.store.clone(),
            events: ctx.events.clone(),
        }))
    }
}

/// A live ACP session driven through the client loop. Holds the `mpsc::Sender`
/// used to enqueue [`ClientRequest`]s, the agent [`SessionId`], and the loop's
/// join handle. Dropping the sender ends the loop.
pub struct AcpSession {
    /// `None` once the session has been ended (sender dropped → loop exits).
    tx: tokio::sync::Mutex<Option<mpsc::Sender<ClientRequest>>>,
    /// Ryuzi's own DB primary key for this session (used for `insert_message`).
    session_pk: String,
    session_id: SessionId,
    store: Arc<Store>,
    /// Broadcast sender for `CoreEvent`s — same channel the sink clones.
    events: tokio::sync::broadcast::Sender<CoreEvent>,
}

#[async_trait]
impl HarnessSession for AcpSession {
    async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
        let tx = {
            let guard = self.tx.lock().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("ACP session already ended"))?
        };

        // Persist the user turn (Spec 1) before driving the prompt, then
        // broadcast it so the bubble appears live (backend-authoritative —
        // there is deliberately no client-side echo). Persist `prompt.display`
        // — the raw text the user typed — not the (possibly
        // attachment-manifest-decorated) `prompt.agent` text, so durable
        // history shows what the user actually typed.
        // Use ryuzi's own session_pk (not the ACP session id) so the frontend
        // can find the row via list_messages(session_pk).
        let payload = serde_json::json!({ "text": prompt.display });
        let user_msg = NewMessage::block(&self.session_pk, "user", "text", payload.clone());
        match self.store.insert_message(user_msg).await {
            Ok(seq) => {
                let _ = self.events.send(CoreEvent::Message {
                    session_pk: self.session_pk.clone(),
                    seq,
                    role: "user".into(),
                    block_type: "text".into(),
                    payload,
                    tool_call_id: None,
                    status: None,
                    tool_kind: None,
                });
            }
            Err(e) => {
                tracing::warn!("send_prompt: failed to persist user turn: {e}");
            }
        }

        let content = vec![ContentBlock::Text(TextContent::new(prompt.agent))];
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(ClientRequest::Prompt {
            content,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow::anyhow!("ACP client loop is unavailable"))?;

        let _stop_reason = reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("ACP prompt cancelled before completion"))??;
        Ok(())
    }

    async fn cancel(&self) -> anyhow::Result<()> {
        let tx = {
            let guard = self.tx.lock().await;
            match guard.as_ref() {
                Some(tx) => tx.clone(),
                None => return Ok(()),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(ClientRequest::Cancel { reply: reply_tx })
            .await
            .is_ok()
        {
            let _ = reply_rx.await;
        }
        Ok(())
    }

    async fn end(&self) -> anyhow::Result<()> {
        // Drop the only retained sender: the client loop's `rx.recv()` returns
        // `None`, the loop exits, and the connection (and, in production, the
        // sidecar child) is torn down. Subsequent `send_prompt` calls then fail.
        let _ = self.tx.lock().await.take();
        Ok(())
    }

    fn agent_session_id(&self) -> Option<String> {
        Some(self.session_id.0.to_string())
    }
}

/// Builds [`AcpHarness`] instances from a host-injected adapter descriptor.
pub struct AcpHarnessFactory {
    descriptor: AcpAdapterDescriptor,
}

impl AcpHarnessFactory {
    pub fn new(descriptor: AcpAdapterDescriptor) -> Self {
        Self { descriptor }
    }
}

impl HarnessFactory for AcpHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(AcpHarness::new(self.descriptor.clone())))
    }
}

/// The `claude-code` integration: plugs into the harness axis only, producing an
/// [`AcpHarnessFactory`] over a host-injected [`AcpAdapterDescriptor`]. The
/// descriptor's `command` is supplied by the host (Task 5 wires the bundled
/// sidecar path; tests inject a stub descriptor).
pub struct ClaudeCodeIntegration {
    factory: Arc<AcpHarnessFactory>,
}

impl ClaudeCodeIntegration {
    pub fn new(descriptor: AcpAdapterDescriptor) -> Self {
        Self {
            factory: Arc::new(AcpHarnessFactory::new(descriptor)),
        }
    }
}

impl crate::integration::Integration for ClaudeCodeIntegration {
    fn id(&self) -> &str {
        "claude-code"
    }

    fn harness(&self) -> Option<Arc<dyn HarnessFactory>> {
        Some(self.factory.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acp_harness_starts_a_session_and_streams_via_the_harness_trait() {
        // Build an AcpHarness on the mock-transport seam, start a session through
        // the Spec 2 Harness trait, send a prompt, and assert the transcript
        // persisted (an assistant text row) plus the user turn.
        let (store, session_pk) = crate::harness::acp::testkit::run_via_harness_trait("hi").await;
        let msgs = store.list_messages(&session_pk).await.unwrap();

        // assistant streamed text row (from the mock's prompt notifications)
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.block_type == "text"),
            "expected an assistant text row, got: {msgs:?}"
        );
        // user turn persisted by send_prompt (Spec 1)
        assert!(
            msgs.iter()
                .any(|m| m.role == "user" && m.block_type == "text" && m.payload["text"] == "hi"),
            "expected the persisted user turn, got: {msgs:?}"
        );
    }

    #[tokio::test]
    async fn factory_creates_a_harness() {
        let factory = AcpHarnessFactory::new(AcpAdapterDescriptor::default());
        let _harness = factory.create().unwrap();
    }

    #[tokio::test]
    async fn agent_fs_requests_hit_the_sandboxed_worktree() {
        let outcome = crate::harness::acp::testkit::run_prompt_with_fs_calls().await;
        assert_eq!(
            outcome.read_back, "hello from agent",
            "read-back content should match what the agent wrote"
        );
        assert!(
            outcome.wrote_inside_worktree,
            "file should exist inside the worktree after write"
        );

        // A (system, status) message row with a REAL seq (≥ 1) must have been
        // persisted after the successful fs/write_text_file (FIX 1).
        let msgs = outcome
            .store
            .list_messages(&outcome.session_pk)
            .await
            .expect("list_messages should not fail");
        let status_row = msgs
            .iter()
            .find(|m| m.role == "system" && m.block_type == "status")
            .expect("expected a (system, status) row for the fs write event");
        assert!(
            status_row.seq >= 1,
            "status row seq must be a real DB seq (≥ 1), got {}",
            status_row.seq
        );
        assert!(
            status_row.payload["summary"]
                .as_str()
                .unwrap_or("")
                .contains("wrote"),
            "status payload summary should mention 'wrote', got: {:?}",
            status_row.payload
        );
    }

    #[tokio::test]
    async fn allow_always_policy_auto_allows_without_hub_registration() {
        // Pre-set an allowAlways policy for "Bash" on project "p-perm-bridge".
        // The PermMockAgent sends a request_permission for tool "Bash" during prompt.
        // The bridge should auto-allow it without touching the hub, so the test
        // completes without needing a hub.resolve() call.
        let outcome = crate::harness::acp::testkit::run_perm_mock_via_harness(
            "p-perm-bridge",
            Some(("Bash", "allowAlways")),
        )
        .await;

        assert!(
            outcome.allowed,
            "mock agent should have received an allow selection"
        );
        assert!(
            outcome.hub_was_never_registered,
            "hub should NOT have been registered when policy is allowAlways"
        );
    }

    #[tokio::test]
    async fn approval_requested_carries_the_ryuzi_session_pk_not_the_acp_session_id() {
        // No per-project tool policy is set, so "Bash" in PermMode::Default
        // falls through to the hub-prompt path (not AutoAllow), which emits
        // CoreEvent::ApprovalRequested. The mock agent's session/new always
        // assigns a fresh random ACP session id (a UUID), which necessarily
        // differs from ryuzi's own session_pk ("perm-bridge-session-pk") —
        // the emitted event must carry the ryuzi pk, not the ACP-assigned id.
        let outcome =
            crate::harness::acp::testkit::run_perm_mock_via_harness("p-approval-pk", None).await;

        assert_eq!(
            outcome.captured_session_pk.as_deref(),
            Some(outcome.session_pk.as_str()),
            "ApprovalRequested.session_pk should be ryuzi's own session_pk ({:?}), \
             not the ACP-assigned session id",
            outcome.session_pk
        );
    }

    #[tokio::test]
    async fn agent_terminal_requests_run_sandboxed_in_worktree() {
        let outcome = crate::harness::acp::testkit::run_prompt_with_terminal_calls().await;
        assert!(
            outcome.output.contains("hello"),
            "expected 'hello' in terminal output, got: {:?}",
            outcome.output
        );
        assert!(
            outcome.ran_in_worktree,
            "pwd output {:?} should contain the worktree path",
            outcome.output
        );
        assert_eq!(
            outcome.exit_code,
            Some(0),
            "expected exit code 0, got: {:?}",
            outcome.exit_code
        );
    }
}

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
