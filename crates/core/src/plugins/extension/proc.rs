//! Spawn, handshake, and shut down one extension subprocess.
//!
//! # Security — `env_clear()` + allowlist
//! Every extension child starts from a *clean* environment
//! (`Command::env_clear()`), not the daemon's full inherited env. It
//! receives only:
//! - a minimal safe base — `PATH`, `HOME`, `LANG` — copied from the
//!   daemon's own environment when present ([`SAFE_BASE_ENV_VARS`]);
//! - exactly the `(key, value)` pairs the resolved `ExtensionSpec.env`
//!   declares (today always empty — see that field's doc in `super`).
//!
//! This is deliberately stricter than the native MCP client
//! (`harness::native::mcp_client::McpConnection::connect_stdio`), which
//! layers `cmd.env(k, v)` onto the process's *inherited* daemon
//! environment and so leaks every daemon secret to any `[[mcp]]`
//! subprocess. The design doc's "Security model" flags this MCP gap;
//! extensions do not repeat it. See [`spawned_child_env_is_cleared_except_the_allowlist`]
//! (this module's tests) for the sentinel-absence proof.
//!
//! # Handshake
//! [`run_initialize`] writes `extension/initialize` through an
//! [`ExtensionIo`] and awaits the matching response via that transport's
//! `request` — [`ExtensionIo::connect`] is generic over the writer/reader
//! types, so both a real subprocess's stdio pipes (production) and an
//! in-memory `tokio::io::duplex` pair (this module's own tests) exercise
//! identical protocol logic. [`ExtensionProc::spawn_and_handshake`] is the
//! only place that combines it with a real `tokio::process::Command`.
//!
//! A handshake failure (bad process, timeout, protocol mismatch, rejection)
//! never surfaces as an `Err` to `spawn_and_handshake`'s caller — it always
//! returns a value, recording the failure in `status` instead (see the
//! design doc's "mismatched/failed init -> extension marked `failed` ...
//! NOT fatal to the daemon").
//!
//! # Concurrent transport
//! [`ExtensionIo`] is a small demultiplexing JSON-RPC client, not a pair of
//! mutexes guarding raw stdin/stdout. A single background reader task
//! (spawned by [`ExtensionIo::connect`]) owns the child's stdout for the
//! whole `Running` lifetime and routes each response line to whichever
//! in-flight `request()` call allocated that response's `id` (a
//! `HashMap<id, oneshot::Sender<..>>` behind one lock — see
//! [`PendingState`]). That is what makes it safe for DT4's `extension/ping`
//! health loop and DT5's `event/<name>` dispatch to have requests in flight
//! on the SAME transport at the SAME time: unlike a naive
//! `stdio_jsonrpc::read_response` loop (which discards every non-matching-id
//! line as it scans, so a concurrent caller can silently steal or drop
//! another caller's response), each `request()` call here gets exactly its
//! own response. A response line with no `id` is a JSON-RPC notification —
//! reserved for DT5 (see [`reader_loop`]'s doc for the seam), safely dropped
//! for now since nothing sends one in this slice. EOF or a read error closes
//! the transport and fails every still-pending `request()` with
//! `TransportError::Closed` immediately, so a caller never hangs on a dead
//! extension — see [`reader_loop`].
//!
//! # Graceful shutdown
//! [`ExtensionProc::shutdown`] sends `extension/shutdown`, gives the
//! process a grace period to exit on its own, then falls back to a hard
//! kill. `kill_on_drop(true)` (set at spawn) is the unconditional backstop
//! if `shutdown` is never called at all.
//!
//! # Supervision (DT4): health ping, restart-with-backoff, give-up
//! [`SupervisedExtension`] is what [`ExtensionHost`] actually stores (one per
//! spawned extension, keyed by owning plugin id): a live [`ExtensionProc`]
//! behind `Arc<Mutex<..>>` (mutated in place on restart — see below) plus a
//! background task running [`supervise`].
//!
//! **Health.** While the proc is `Running`, `supervise` sleeps
//! [`PING_INTERVAL`] and then sends `extension/ping`
//! ([`protocol::ping_request`]) through the SAME concurrency-safe
//! `ExtensionIo::request` transport DT5's future event dispatch will share —
//! `ExtensionProc.io` is `Arc<ExtensionIo>` specifically so the supervisor
//! can clone a handle and issue the ping WITHOUT holding the `ExtensionProc`
//! mutex for the whole [`PING_TIMEOUT`] round trip (a `status()`/`shutdown`
//! caller must never stall behind an in-flight ping). A response that isn't
//! a JSON-RPC error ([`protocol::parse_ping_response`]) is healthy; a
//! timeout, a transport error, or the process having already exited
//! (`TransportError::Closed`, since a dead child's stdout EOF fails every
//! `request()` immediately — see `reader_loop`) is not.
//!
//! **Restart-with-backoff.** On unhealthy (or when `supervise` starts and
//! the proc is *already* not `Running` — an initial spawn/handshake failure
//! is retried exactly like a later crash, so a transient startup problem
//! self-heals instead of being a one-shot permanent `Failed`), the proc is
//! set to [`ExtensionStatus::Restarting`], and `supervise` waits
//! [`backoff_for_attempt`]'s exponential, [`RESTART_BACKOFF_CAP`]-capped
//! delay before re-running [`ExtensionProc::spawn_and_handshake`] (DT3's own
//! spawn+handshake, reused verbatim) against the same [`ExtensionSpec`]. A
//! successful respawn (`Running`) returns to the ping-health loop; a failed
//! one loops back into another backoff round.
//!
//! **Give-up.** Every restart *attempt* (not detection) is timestamped;
//! before each attempt, timestamps older than [`RESTART_WINDOW`] are pruned,
//! and if [`MAX_RESTARTS_IN_WINDOW`] attempts already happened inside the
//! window, `supervise` gives up permanently: the proc is set to
//! `ExtensionStatus::Failed("restart-exhausted: ...")` (no extension-supplied
//! text — safe for `plugin_doctor`/DT8) and the task returns, so a
//! permanently-broken extension stops respawning instead of looping forever.
//! The attempt list (and so the backoff exponent) is cleared once the proc
//! has been continuously `Running` for [`HEALTHY_RESET_AFTER`], so a
//! long-lived extension that later has one bad crash gets a fresh budget
//! rather than inheriting exhaustion from restarts long past.
//!
//! **Independence.** `supervise` owns exactly one proc's state; a give-up in
//! one task never touches another extension's `Arc<Mutex<..>>`, another
//! plugin, or the daemon — see [`ExtensionHost`]'s per-plugin `Vec` of
//! independently-supervised procs.
//!
//! **Shutdown stops supervision, and never races a restart.**
//! [`SupervisedExtension::shutdown`] cancels a [`tokio_util::sync::CancellationToken`]
//! FIRST and awaits the supervisor task's `JoinHandle` before ever touching
//! the proc's `child`/`io` — `supervise` races every wait point (the ping
//! interval, the backoff delay, and the respawn call itself) against that
//! same token via `tokio::select!`, so cancellation always wins a race
//! against "keep waiting" or "restart", never the other way around: once
//! `shutdown` observes the task has finished, it is structurally impossible
//! for that task to still decide to respawn afterward. Only once the
//! supervisor is confirmed stopped does `shutdown` lock the proc and run
//! [`ExtensionProc::shutdown`] (the DT3 graceful-stop primitive) on whatever
//! process is currently live (there may be none, mid-backoff).
//!
//! **Concurrent `shutdown_all`.** [`ExtensionHost::shutdown_all`] stops every
//! `SupervisedExtension` across every plugin via `futures::future::join_all`
//! rather than a sequential loop (DT3's own noted follow-up), so daemon-stop
//! latency is bounded by the single slowest shutdown, not `N × grace`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader, Lines};
use tokio::process::Child;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::domain::Principal;
use crate::harness::native::hooks::HookEvent;
use crate::plugins::host::PluginHost;
use crate::stdio_jsonrpc;

use super::{protocol, ExtensionCtx, ExtensionSpec, ExtensionStatus};

/// Health check cadence for a `Running` extension's supervisor loop
/// ([`supervise`]).
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Bound on a single `extension/ping` round trip. Independent of
/// [`INIT_HANDSHAKE_TIMEOUT`] — a live extension answering its steady-state
/// health probe should be fast; a slow/hung response is itself a health
/// signal.
pub const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// The first restart's backoff delay ([`backoff_for_attempt`]`(0)`).
pub const RESTART_BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Backoff never exceeds this, no matter how many consecutive restarts have
/// happened.
pub const RESTART_BACKOFF_CAP: Duration = Duration::from_secs(60);

/// Give up (see this module's "Give-up" doc above) once this many restart
/// *attempts* have happened inside [`RESTART_WINDOW`].
pub const MAX_RESTARTS_IN_WINDOW: u32 = 5;

/// Sliding window [`MAX_RESTARTS_IN_WINDOW`] is counted over.
pub const RESTART_WINDOW: Duration = Duration::from_secs(5 * 60);

/// How long a proc must be continuously `Running` before its restart-attempt
/// history (and so its backoff exponent and give-up budget) resets.
pub const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(60);

/// Tunable timing knobs for [`supervise`]'s health/backoff/give-up policy.
/// Production code always gets [`SupervisorConfig::default`] (the documented
/// consts above); tests substitute tiny real durations instead.
///
/// This exists specifically so tests do NOT reach for `tokio::time::pause`/
/// `start_paused`: a supervision test spawns a REAL subprocess (the same
/// hermetic `sh` one-liner fakes DT3's own tests use), and a real child's
/// response arrives in genuine wall-clock time no matter what a *paused*
/// clock claims. Pairing a paused clock with a real subprocess is actively
/// dangerous, not just unnecessary — tokio's auto-advance-when-idle behavior
/// (what makes e.g. `spawn_and_handshake_reports_failed_on_timeout`'s 25s
/// wait resolve instantly) fires as soon as the executor finds nothing
/// immediately pollable, which can be BEFORE the OS has even scheduled the
/// freshly-spawned child to run — so it can jump straight to
/// `INIT_HANDSHAKE_TIMEOUT` and fail a handshake that a real, unpaused clock
/// would have let succeed in a few milliseconds. Small-but-real durations
/// sidestep that race entirely: every timer here is real, so a real
/// process's real response always has time to arrive first.
#[derive(Debug, Clone, Copy)]
struct SupervisorConfig {
    ping_interval: Duration,
    ping_timeout: Duration,
    restart_backoff_base: Duration,
    restart_backoff_cap: Duration,
    max_restarts_in_window: u32,
    restart_window: Duration,
    healthy_reset_after: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> SupervisorConfig {
        SupervisorConfig {
            ping_interval: PING_INTERVAL,
            ping_timeout: PING_TIMEOUT,
            restart_backoff_base: RESTART_BACKOFF_BASE,
            restart_backoff_cap: RESTART_BACKOFF_CAP,
            max_restarts_in_window: MAX_RESTARTS_IN_WINDOW,
            restart_window: RESTART_WINDOW,
            healthy_reset_after: HEALTHY_RESET_AFTER,
        }
    }
}

/// The backoff delay before the restart attempt at `attempt_index` (0-based:
/// `attempt_index` is how many restart attempts have already happened inside
/// the current window before this one) — `min(cfg.restart_backoff_base *
/// 2^attempt_index, cfg.restart_backoff_cap)`. `attempt_index` is clamped
/// before exponentiation so this never overflows `Duration`'s internal
/// representation even if called with a very large index.
fn backoff_for_attempt(attempt_index: usize, cfg: &SupervisorConfig) -> Duration {
    let exp = attempt_index.min(10) as u32;
    let scaled = cfg.restart_backoff_base.saturating_mul(1u32 << exp);
    scaled.min(cfg.restart_backoff_cap)
}

/// Environment variables copied from the daemon's own process environment
/// into every extension child, if present there — enough for a
/// well-behaved binary to run (locate shared tools on `PATH`, resolve `~`,
/// pick a sane locale) without inheriting anything else. See the module
/// doc's env_clear model.
const SAFE_BASE_ENV_VARS: &[&str] = &["PATH", "HOME", "LANG"];

/// Overall budget for the one-time `extension/initialize` handshake.
/// Independent of `ExtensionSpec::timeout` (the manifest's PER-EVENT
/// dispatch budget, reused only by DT5's gating dispatch) — an extension
/// may legitimately take longer to boot than its steady-state per-event
/// budget. Mirrors the "25s probe" pattern `stdio_jsonrpc`'s module doc
/// references.
pub const INIT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(25);

/// Grace period [`ExtensionProc::shutdown`] gives a process to exit on its
/// own after `extension/shutdown`, before falling back to a hard kill.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Build the child `Command` for `spec`: `env_clear()` + the safe base +
/// `spec.env`, piped stdin/stdout, stderr discarded (mirrors
/// `McpConnection::connect_stdio`'s choice to null stderr rather than
/// interleave it with the JSON-RPC stdout stream), `kill_on_drop(true)` as
/// the unconditional backstop if `shutdown` is never called.
fn build_command(spec: &ExtensionSpec) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&spec.command);
    cmd.args(&spec.args);
    cmd.env_clear();
    for key in SAFE_BASE_ENV_VARS {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::process_util::no_window(&mut cmd);
    cmd
}

/// The generic core of the initialize handshake: write `extension/initialize`
/// through `io` and await the matching response via its concurrency-safe
/// `request()` — see the module doc's "Concurrent transport" section. `io`
/// is constructed generically over the writer/reader types by
/// [`ExtensionIo::connect`], so both a real subprocess's stdio pipes and (in
/// tests) an in-memory `tokio::io::duplex` half exercise identical protocol
/// logic.
async fn run_initialize(
    io: &ExtensionIo,
    events: &[&str],
    provides_tools: bool,
    timeout: Duration,
) -> Result<protocol::InitializeAck, protocol::InitError> {
    let id = io.alloc_id();
    let req = protocol::initialize_request(id, events, provides_tools);
    let resp = io.request(id, req, timeout).await.map_err(|e| match e {
        TransportError::Closed => protocol::InitError::Closed,
        TransportError::Io(msg) => protocol::InitError::Io(msg),
        TransportError::Timeout => protocol::InitError::Timeout,
    })?;
    protocol::parse_initialize_response(&resp)
}

/// Map an initialize failure to a reason safe to surface/persist — mirrors
/// `control::lifecycle::safe_attach_reason`'s discipline: name the
/// extension and the failure *stage*, never raw extension-supplied text.
/// `Rejected`/`Malformed`/`Io` are collapsed to a generic per-stage message
/// because the extension controls its own JSON-RPC error bodies and could
/// echo back anything, including text that happened to flow through a
/// `${setting:KEY}`/`${auth}` value in its own argv.
fn sanitize_init_error(name: &str, err: &protocol::InitError) -> String {
    match err {
        protocol::InitError::NotOk => format!("{name}: initialize did not report ok"),
        protocol::InitError::ProtocolMismatch => {
            format!("{name}: initialize protocol version mismatch")
        }
        protocol::InitError::Closed => format!("{name}: closed the connection during initialize"),
        protocol::InitError::Timeout => format!("{name}: initialize timed out"),
        protocol::InitError::Rejected => format!("{name}: initialize was rejected"),
        protocol::InitError::Malformed => format!("{name}: initialize response was malformed"),
        protocol::InitError::Io(_) => {
            format!("{name}: a transport error occurred during initialize")
        }
    }
}

/// Why an [`ExtensionIo::request`] call failed.
#[derive(Debug)]
enum TransportError {
    /// The reader task observed EOF or a read error (possibly while
    /// draining this exact request), or the transport was already closed
    /// before this call started.
    Closed,
    /// Writing the request line to stdin failed.
    Io(String),
    /// No response arrived within the caller-supplied budget.
    Timeout,
}

/// In-flight [`ExtensionIo::request`] calls, keyed by the JSON-RPC id
/// `request()` allocated for them, or `Closed` once [`reader_loop`] has
/// observed EOF/a read error. Deliberately ONE `Mutex` guarding both the map
/// and the closed flag together (not a separate `AtomicBool` alongside the
/// map) so a `request()` call's "insert my waiter" and the reader's "drain
/// everyone, mark closed" can never interleave: either the insert lands
/// before the drain (and that waiter gets failed by it) or the drain has
/// already run (and the `Closed` check `request()` makes under the same
/// lock rejects the call immediately) — there is no window where a waiter
/// can be inserted after the drain and then wait forever.
enum PendingState {
    Open(HashMap<i64, oneshot::Sender<Result<Value, TransportError>>>),
    Closed,
}

/// The background demux reader loop: owns `lines` (the child's stdout, or —
/// in tests — the host side of an in-memory duplex) for the whole
/// `ExtensionIo` lifetime, per [`ExtensionIo::connect`].
///
/// For each line: JSON with a numeric `id` is routed to whichever
/// `request()` call is waiting on that id (a response for an id nobody's
/// waiting on — already timed out, or a stray duplicate — is silently
/// dropped, matching JSON-RPC semantics); a line that fails to parse as JSON
/// is skipped and reading continues (mirrors `stdio_jsonrpc::read_response`'s
/// tolerance of interleaved non-response lines). JSON with no `id` is a
/// JSON-RPC *notification* — nothing in this slice sends one (DT5's
/// `event/<name>` dispatch is host -> extension only, and no extension
/// pushes anything unsolicited yet), so **this is the seam DT5 should extend**
/// once an extension needs to push something host-ward outside of a
/// request/response: thread an `mpsc::UnboundedSender<Value>` (or similar
/// sink) into this function and forward the notification's `Value` to it
/// instead of dropping it. The loop must never block on that forward, since
/// a slow/absent consumer must not stall demuxing of ordinary responses.
///
/// On EOF (`next_line` returns `Ok(None)`) or a read error, the loop ends
/// and drains `pending`, failing every still-waiting `request()` call with
/// `TransportError::Closed` — so a caller can never hang forever on a dead
/// transport — and marks the transport closed for every future `request()`
/// call too.
async fn reader_loop<R>(mut lines: Lines<R>, pending: Arc<Mutex<PendingState>>)
where
    R: AsyncBufRead + Unpin,
{
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match value.get("id").and_then(Value::as_i64) {
            Some(id) => {
                let waiter = match &mut *pending.lock().await {
                    PendingState::Open(map) => map.remove(&id),
                    PendingState::Closed => None,
                };
                if let Some(tx) = waiter {
                    let _ = tx.send(Ok(value));
                }
            }
            None => {
                // DT5: route notifications here instead of dropping.
            }
        }
    }
    // `lines.next_line()` returned `Ok(None)` (EOF) or `Err(_)` (a read
    // error) — either way the transport is done. Drain every still-pending
    // request so nothing hangs forever waiting on a dead extension.
    let previous = std::mem::replace(&mut *pending.lock().await, PendingState::Closed);
    if let PendingState::Open(map) = previous {
        for (_, tx) in map {
            let _ = tx.send(Err(TransportError::Closed));
        }
    }
}

/// The live, concurrency-safe transport to a `Running` extension: a
/// demultiplexing JSON-RPC client, not a pair of mutexes over raw
/// stdin/stdout — see the module doc's "Concurrent transport" section.
/// Multiple `request()` calls may be in flight at once (DT4's ping loop
/// alongside DT5's event dispatch on the same proc); each gets exactly its
/// own response.
struct ExtensionIo {
    /// Serializes writes onto the child's stdin. Boxed so both a real
    /// `ChildStdin` (production) and an in-memory `tokio::io::duplex` half
    /// (this module's own tests) share one concrete `ExtensionIo` type —
    /// see [`ExtensionIo::connect`].
    stdin: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    next_id: AtomicI64,
    /// Shared with the background reader task — see [`reader_loop`] and
    /// [`PendingState`].
    pending: Arc<Mutex<PendingState>>,
    /// The background reader task's handle. `Drop` aborts it so a
    /// dropped/shut-down `ExtensionIo` never leaves a stray task blocked
    /// reading a pipe nobody cares about anymore.
    reader_task: Option<JoinHandle<()>>,
}

impl ExtensionIo {
    /// Spawn the background [`reader_loop`] over `lines` and return the live
    /// handle. Generic over the writer/reader types so both a real child's
    /// stdio pipes and (in tests) an in-memory `tokio::io::duplex` pair
    /// construct the identical concurrency-safe transport.
    fn connect<W, R>(writer: W, lines: Lines<R>) -> ExtensionIo
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncBufRead + Unpin + Send + 'static,
    {
        let pending = Arc::new(Mutex::new(PendingState::Open(HashMap::new())));
        let reader_task = tokio::spawn(reader_loop(lines, pending.clone()));
        ExtensionIo {
            stdin: Mutex::new(Box::new(writer)),
            next_id: AtomicI64::new(1),
            pending,
            reader_task: Some(reader_task),
        }
    }

    /// Allocate the next JSON-RPC request id. Callers build their request
    /// `Value` with it (via `protocol::*_request(id, ...)`) before passing
    /// both to [`ExtensionIo::request`].
    fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a pre-built JSON-RPC request (already carrying `id`, allocated
    /// via [`ExtensionIo::alloc_id`]) and await its response, demultiplexed
    /// by id against whatever else is in flight concurrently on this same
    /// transport — see the module doc's "Concurrent transport" section.
    /// Fails immediately with `TransportError::Closed` if the transport is
    /// already closed. `timeout` bounds only the wait for a response (the
    /// caller times out if the extension is alive but silent); on timeout
    /// this request's own pending entry is removed so it does not linger.
    async fn request(
        &self,
        id: i64,
        req: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            match &mut *pending {
                PendingState::Closed => return Err(TransportError::Closed),
                PendingState::Open(map) => {
                    map.insert(id, tx);
                }
            }
        }
        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdio_jsonrpc::write_line(&mut *stdin, &req).await {
                if let PendingState::Open(map) = &mut *self.pending.lock().await {
                    map.remove(&id);
                }
                return Err(TransportError::Io(e.to_string()));
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_sender_dropped)) => Err(TransportError::Closed),
            Err(_elapsed) => {
                if let PendingState::Open(map) = &mut *self.pending.lock().await {
                    map.remove(&id);
                }
                Err(TransportError::Timeout)
            }
        }
    }

    /// Write a JSON-RPC line without registering a pending response — used
    /// only by `extension/shutdown`, which is fire-and-forget: the
    /// extension is expected to exit on its own once it receives this, and
    /// `ExtensionProc::shutdown`'s own grace-period + hard-kill fallback
    /// covers the case where it doesn't reply or exit.
    async fn notify(&self, req: &Value) -> std::io::Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdio_jsonrpc::write_line(&mut *stdin, req).await
    }
}

impl Drop for ExtensionIo {
    fn drop(&mut self) {
        if let Some(handle) = self.reader_task.take() {
            handle.abort();
        }
    }
}

/// One extension subprocess: the spawned [`Child`] (kept alive so
/// `kill_on_drop` reaps it — see [`build_command`]), its open stdin/stdout
/// (once `status` is [`ExtensionStatus::Running`]), and the handshake
/// outcome.
pub struct ExtensionProc {
    pub spec: ExtensionSpec,
    pub status: ExtensionStatus,
    /// The event names the extension confirmed at init (see
    /// `protocol::InitializeAck::events`) — empty unless `status ==
    /// Running`. DT5 dispatch fans an event out only to a proc whose
    /// `confirmed_events` includes it.
    pub confirmed_events: Vec<String>,
    /// Raw tool defs from init, present only when `spec.provides_tools` and
    /// `status == Running`. DT6 wraps these into typed tools.
    pub tools: Vec<Value>,
    child: Option<Child>,
    /// `Arc`, not a bare `ExtensionIo`: [`supervise`]'s health ping clones
    /// this handle and issues `request()` *without* holding the outer
    /// `Arc<Mutex<ExtensionProc>>` lock for the round trip (see this
    /// module's "Supervision" doc) — a `status()` reader or `shutdown` must
    /// never stall behind an in-flight ping.
    io: Option<Arc<ExtensionIo>>,
}

impl ExtensionProc {
    fn failed(spec: ExtensionSpec, reason: String, child: Option<Child>) -> ExtensionProc {
        ExtensionProc {
            spec,
            status: ExtensionStatus::Failed(reason),
            confirmed_events: Vec::new(),
            tools: Vec::new(),
            child,
            io: None,
        }
    }

    /// Spawn `spec.command` as a stdio child (env_clear + allowlist — see
    /// [`build_command`]), then run `extension/initialize` within
    /// [`INIT_HANDSHAKE_TIMEOUT`]. Never returns an error: every failure
    /// mode (spawn failure, handshake rejection/timeout/protocol mismatch,
    /// closed pipe) is recorded as `ExtensionStatus::Failed` on the
    /// returned value — see the module doc.
    pub async fn spawn_and_handshake(spec: ExtensionSpec) -> ExtensionProc {
        let mut cmd = build_command(&spec);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                // A spawn failure (e.g. "No such file or directory") comes
                // from the OS/Rust before any extension-controlled code
                // ever runs — unlike a handshake failure it cannot echo
                // back extension-supplied content, so its text is safe to
                // keep verbatim.
                let reason = format!("{}: failed to start: {e}", spec.name);
                return ExtensionProc::failed(spec, reason, None);
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let lines = BufReader::new(stdout).lines();
        let io = ExtensionIo::connect(stdin, lines);
        let events: Vec<&str> = spec.events.iter().map(HookEvent::as_str).collect();
        let provides_tools = spec.provides_tools;

        match run_initialize(&io, &events, provides_tools, INIT_HANDSHAKE_TIMEOUT).await {
            Ok(ack) => ExtensionProc {
                confirmed_events: ack.events,
                tools: ack.tools,
                status: ExtensionStatus::Running,
                child: Some(child),
                io: Some(Arc::new(io)),
                spec,
            },
            Err(e) => {
                let reason = sanitize_init_error(&spec.name, &e);
                let _ = child.kill().await;
                // `io` (holding the reader task) drops here — its `Drop`
                // impl aborts that task, since this proc never reaches
                // `Running` for anything to read through it.
                ExtensionProc::failed(spec, reason, None)
            }
        }
    }

    /// Ask the extension to stop gracefully: send `extension/shutdown`
    /// (best-effort — a write failure is ignored, since the fallback kill
    /// below covers it), give the process `grace` to exit on its own, then
    /// fall back to a hard kill. `kill_on_drop(true)` (set at spawn) is the
    /// unconditional backstop if `shutdown` is never called at all (e.g.
    /// the daemon itself crashes). Calling this on a proc that never
    /// spawned, already failed, or was already shut down is a no-op beyond
    /// marking `status` `Stopped`.
    pub async fn shutdown(&mut self, grace: Duration) {
        let Some(mut child) = self.child.take() else {
            self.status = ExtensionStatus::Stopped;
            return;
        };
        if let Some(io) = self.io.take() {
            let id = io.alloc_id();
            let req = protocol::shutdown_request(id);
            let _ = io.notify(&req).await;
            // This `Arc<ExtensionIo>` drops at the end of this block. In
            // every real caller this is the last outstanding clone (DT4's
            // `SupervisedExtension::shutdown` cancels+joins the supervisor
            // — the only other place that ever clones this `Arc` — before
            // ever reaching here), so the drop takes the strong count to 0
            // and `ExtensionIo`'s `Drop` impl aborts the background reader
            // task.
        }
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
        self.status = ExtensionStatus::Stopped;
    }
}

/// Overwrite `*state` with a placeholder [`ExtensionStatus::Restarting`]
/// record for `spec`: no live `child`/`io` (the previous ones, if any, are
/// dropped here — `kill_on_drop`/`ExtensionIo::Drop` reap/abort them), empty
/// `confirmed_events`/`tools`. Always followed by either another respawn
/// attempt (replacing this placeholder with a real result) or a give-up
/// (replacing it with `Failed`) — see [`supervise`].
async fn mark_restarting(state: &Arc<Mutex<ExtensionProc>>, spec: &ExtensionSpec) {
    let mut guard = state.lock().await;
    *guard = ExtensionProc {
        spec: spec.clone(),
        status: ExtensionStatus::Restarting,
        confirmed_events: Vec::new(),
        tools: Vec::new(),
        child: None,
        io: None,
    };
}

/// One `extension/ping` health check: clones the current `Arc<ExtensionIo>`
/// under a brief lock (so the round trip itself never holds the
/// `ExtensionProc` mutex — see this module's "Supervision" doc), then issues
/// it. `false` covers every unhealthy case uniformly: not currently
/// `Running`, no transport, a JSON-RPC error reply, a timeout, or the
/// transport already closed because the child exited.
async fn ping_once(state: &Arc<Mutex<ExtensionProc>>, ping_timeout: Duration) -> bool {
    let io = {
        let guard = state.lock().await;
        match (&guard.status, &guard.io) {
            (ExtensionStatus::Running, Some(io)) => io.clone(),
            _ => return false,
        }
    };
    let id = io.alloc_id();
    let req = protocol::ping_request(id);
    match io.request(id, req, ping_timeout).await {
        Ok(resp) => protocol::parse_ping_response(&resp),
        Err(_) => false,
    }
}

/// The result of one [`dispatch_event`] attempt against a single supervised
/// extension (DT5).
#[derive(Debug)]
pub(crate) enum EventDispatchOutcome {
    /// Not currently `Running`, or `Running` but not subscribed to this
    /// event (`confirmed_events` doesn't include it) — the caller must not
    /// count this as having contacted the extension at all (no deny, no
    /// warning, nothing sent over the wire).
    Skipped,
    /// The extension responded without denying
    /// (`protocol::parse_event_response`'s default).
    Allowed,
    /// The extension denied via `{"deny": true, "reason": "..."}`.
    Denied(Option<String>),
    /// No response arrived within `timeout`, or the transport failed/closed
    /// (e.g. the process crashed mid-dispatch). The caller (DT5's gating
    /// dispatch, in `events`) treats this as fail-open (allow) plus a
    /// warning — a broken extension must never brick the agent. See
    /// `events`'s module doc.
    Unreachable,
}

/// Dispatch one `event/<name>` request to the extension behind `state`,
/// bounded by `timeout` — DT5's per-extension primitive, structured exactly
/// like [`ping_once`]: a brief lock only to clone the current
/// `Arc<ExtensionIo>` (or bail out `Skipped` without ever touching the
/// transport if the proc isn't `Running` or hasn't confirmed this event),
/// then the actual round trip happens WITHOUT holding the `ExtensionProc`
/// mutex — a slow/hung extension's event dispatch can never stall a
/// concurrent `status()`/`shutdown()`/ping call against the SAME proc, and
/// vice versa.
pub(crate) async fn dispatch_event(
    state: &Arc<Mutex<ExtensionProc>>,
    timeout: Duration,
    event: HookEvent,
    payload: &Value,
) -> EventDispatchOutcome {
    let io = {
        let guard = state.lock().await;
        if !matches!(guard.status, ExtensionStatus::Running) {
            return EventDispatchOutcome::Skipped;
        }
        if !guard
            .confirmed_events
            .iter()
            .any(|e| e.as_str() == event.as_str())
        {
            return EventDispatchOutcome::Skipped;
        }
        match &guard.io {
            Some(io) => io.clone(),
            None => return EventDispatchOutcome::Skipped,
        }
    };
    let id = io.alloc_id();
    let req = protocol::event_request(id, event.as_str(), payload);
    match io.request(id, req, timeout).await {
        Ok(resp) => {
            let ack = protocol::parse_event_response(&resp);
            if ack.deny {
                EventDispatchOutcome::Denied(ack.reason)
            } else {
                EventDispatchOutcome::Allowed
            }
        }
        Err(_) => EventDispatchOutcome::Unreachable,
    }
}

/// Why a DT6 `tool/call` dispatch failed. Every variant becomes a plain
/// tool-result ERROR at `ExtensionTool::execute` (harness::native::tools) —
/// never a panic or a hang, mirroring [`EventDispatchOutcome::Unreachable`]'s
/// "a broken extension must never brick the agent" discipline, just for a
/// tool call instead of a hook event.
#[derive(Debug)]
pub(crate) enum ToolCallError {
    /// Not currently `Running`, the transport closed, or the child already
    /// exited — covers "was never runnable" and "died mid-call" uniformly.
    Closed,
    /// No response arrived within the extension's per-call timeout budget
    /// (reuses [`ExtensionSpec::timeout`], the same per-event budget DT5's
    /// gating dispatch enforces).
    Timeout,
    /// Writing the request to the extension's stdin failed.
    Io(String),
    /// The extension replied with a JSON-RPC `error` object
    /// (`protocol::parse_tool_call_response`'s stringified body) —
    /// extension-supplied text, so callers must not assume it's secret-free,
    /// unlike [`sanitize_init_error`]'s collapsed per-stage messages.
    Rejected(String),
}

impl std::fmt::Display for ToolCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolCallError::Closed => {
                write!(f, "extension is not running or its transport closed")
            }
            ToolCallError::Timeout => write!(f, "tool call timed out"),
            ToolCallError::Io(e) => write!(f, "transport error: {e}"),
            ToolCallError::Rejected(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ToolCallError {}

/// Dispatch one `tool/call` to the extension behind `state`, bounded by
/// `timeout` — DT6's per-extension primitive, structured exactly like
/// [`dispatch_event`]/[`ping_once`]: a brief lock only to clone the current
/// `Arc<ExtensionIo>` (bailing out immediately, without ever touching the
/// transport, if the proc isn't `Running`), then the actual round trip
/// happens WITHOUT holding the `ExtensionProc` mutex — a slow/hung tool call
/// can never stall a concurrent ping/event-dispatch/shutdown against the SAME
/// proc, and vice versa.
pub(crate) async fn call_tool(
    state: &Arc<Mutex<ExtensionProc>>,
    timeout: Duration,
    tool: &str,
    arguments: Value,
) -> Result<Value, ToolCallError> {
    let io = {
        let guard = state.lock().await;
        match (&guard.status, &guard.io) {
            (ExtensionStatus::Running, Some(io)) => io.clone(),
            _ => return Err(ToolCallError::Closed),
        }
    };
    let id = io.alloc_id();
    let req = protocol::tool_call_request(id, tool, &arguments);
    let resp = io.request(id, req, timeout).await.map_err(|e| match e {
        TransportError::Closed => ToolCallError::Closed,
        TransportError::Io(msg) => ToolCallError::Io(msg),
        TransportError::Timeout => ToolCallError::Timeout,
    })?;
    protocol::parse_tool_call_response(&resp).map_err(ToolCallError::Rejected)
}

/// What a wrapped extension tool (`ExtensionTool`, `harness::native::tools::extension`)
/// calls to dispatch `tool/call` — mirrors `mcp_client::McpCaller`'s shape
/// exactly (`async fn call(&self, tool, arguments) -> anyhow::Result<Value>`)
/// so the two `Tool` impls can share the same `execute`/`render_tool_result`
/// pattern. Defined here rather than reusing `McpCaller` directly so the
/// extension protocol's own error semantics ([`ToolCallError`]) stay
/// independent of MCP's — an extension is not an MCP server, even though the
/// wire shape happens to rhyme.
#[async_trait]
pub(crate) trait ExtensionCaller: Send + Sync {
    async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value>;
}

/// A cheap, `'static`, cloned-out handle to one supervised extension's live
/// dispatch state (DT6) — the tool-call analogue of [`DispatchHandle`].
#[derive(Clone)]
pub(crate) struct ToolCallHandle {
    name: String,
    state: Arc<Mutex<ExtensionProc>>,
    timeout: Duration,
}

#[async_trait]
impl ExtensionCaller for ToolCallHandle {
    async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value> {
        call_tool(&self.state, self.timeout, tool, arguments)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.name))
    }
}

/// The DT4 supervisor: one instance per [`SupervisedExtension`], spawned by
/// [`SupervisedExtension::spawn`] and cancelled by
/// [`SupervisedExtension::shutdown`]. See this module's "Supervision" doc
/// for the full health/backoff/give-up/shutdown-race design; this is the
/// state machine that implements it.
///
/// `cancel` is raced via `tokio::select!` against every wait point (the ping
/// interval, the backoff delay, and the respawn call itself), so a shutdown
/// mid-wait always wins over "keep waiting" or "restart" — see the module
/// doc's "Shutdown stops supervision" section for why this rules out a
/// restart-after-shutdown race.
async fn supervise(
    spec: ExtensionSpec,
    state: Arc<Mutex<ExtensionProc>>,
    cancel: CancellationToken,
    cfg: SupervisorConfig,
) {
    let mut restart_attempts: Vec<tokio::time::Instant> = Vec::new();
    let mut healthy_since = tokio::time::Instant::now();

    'supervise: loop {
        let currently_running = matches!(state.lock().await.status, ExtensionStatus::Running);
        if currently_running {
            // ---- Health loop: ping on an interval while Running. ----
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(cfg.ping_interval) => {}
                }
                if ping_once(&state, cfg.ping_timeout).await {
                    if tokio::time::Instant::now().duration_since(healthy_since)
                        >= cfg.healthy_reset_after
                    {
                        restart_attempts.clear();
                    }
                    continue;
                }
                break; // unhealthy -> fall through to restart handling below
            }
            if cancel.is_cancelled() {
                return;
            }
        }

        // ---- Restart-with-backoff loop. Reached either from an unhealthy
        // Running proc above, or directly on this task's first iteration if
        // the initial spawn/handshake never reached Running at all. ----
        loop {
            let now = tokio::time::Instant::now();
            restart_attempts.retain(|t| now.duration_since(*t) < cfg.restart_window);

            if restart_attempts.len() >= cfg.max_restarts_in_window as usize {
                let reason = format!(
                    "restart-exhausted: {} restarts within {:?}",
                    cfg.max_restarts_in_window, cfg.restart_window
                );
                let mut guard = state.lock().await;
                guard.status = ExtensionStatus::Failed(reason);
                guard.confirmed_events.clear();
                guard.tools.clear();
                guard.child = None;
                guard.io = None;
                return; // give up permanently — this task never restarts again
            }

            mark_restarting(&state, &spec).await;

            let backoff = backoff_for_attempt(restart_attempts.len(), &cfg);
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(backoff) => {}
            }

            restart_attempts.push(tokio::time::Instant::now());

            let new_proc = tokio::select! {
                _ = cancel.cancelled() => return,
                proc = ExtensionProc::spawn_and_handshake(spec.clone()) => proc,
            };
            let became_running = matches!(new_proc.status, ExtensionStatus::Running);
            *state.lock().await = new_proc;
            if became_running {
                healthy_since = tokio::time::Instant::now();
                continue 'supervise;
            }
            // Respawn itself failed — loop back to prune/give-up/backoff.
        }
    }
}

/// A read-only, cloned-out point-in-time view of one supervised extension —
/// what [`ExtensionHost::get`] hands back, since the live
/// `Arc<Mutex<ExtensionProc>>` behind [`SupervisedExtension`] cannot be
/// exposed by reference (DT4's supervisor mutates it concurrently).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionSnapshot {
    pub name: String,
    pub status: ExtensionStatus,
    pub confirmed_events: Vec<String>,
    pub tools: Vec<Value>,
}

/// A cheap, `'static`, cloned-out handle to one supervised extension's live
/// dispatch state (DT5) — everything [`dispatch_event`] needs (the shared
/// `Arc<Mutex<ExtensionProc>>`, the manifest's per-event timeout, and the
/// extension's name for logging) WITHOUT borrowing the owning
/// `SupervisedExtension`/`ExtensionHost`. This is what lets an OBSERVATIONAL
/// dispatch (DT5's `events` module) hand one off to a detached
/// `tokio::spawn` task per extension — a spawned future must be `'static`,
/// so it cannot hold a borrow from `ExtensionHost::dispatch_handles`'s
/// caller. Cloning is cheap: an `Arc` clone, a `String` clone, and a `Copy`
/// `Duration`.
#[derive(Clone)]
pub(crate) struct DispatchHandle {
    name: String,
    state: Arc<Mutex<ExtensionProc>>,
    timeout: Duration,
}

impl DispatchHandle {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Dispatch `event` to this one extension — forwards to
    /// [`dispatch_event`] against this handle's own `state`/`timeout`.
    pub(crate) async fn dispatch(&self, event: HookEvent, payload: &Value) -> EventDispatchOutcome {
        dispatch_event(&self.state, self.timeout, event, payload).await
    }
}

/// One extension subprocess UNDER SUPERVISION: the live [`ExtensionProc`]
/// behind a lock (replaced in place by [`supervise`] on every restart
/// attempt/give-up) plus the machinery to start and, crucially, cleanly stop
/// that background task. This is what [`ExtensionHost`] stores; a bare
/// [`ExtensionProc`] (no supervisor) remains the lower-level primitive DT3's
/// own tests exercise directly.
pub struct SupervisedExtension {
    spec: ExtensionSpec,
    state: Arc<Mutex<ExtensionProc>>,
    cancel: CancellationToken,
    supervisor: Option<JoinHandle<()>>,
}

impl SupervisedExtension {
    /// Spawn+handshake `spec` and start supervision using the production
    /// timing (see [`SupervisorConfig::default`]). This is the entry point
    /// [`ExtensionHost::spawn_all`] uses.
    async fn spawn(spec: ExtensionSpec) -> SupervisedExtension {
        SupervisedExtension::spawn_with_config(spec, SupervisorConfig::default()).await
    }

    /// Spawn+handshake `spec` (via [`ExtensionProc::spawn_and_handshake`] —
    /// DT3's primitive, reused verbatim, including its "never returns an
    /// error" contract) and start its background [`supervise`] task with the
    /// given `cfg`. Whether the initial handshake succeeded or not,
    /// supervision starts immediately — an initial failure is retried with
    /// the same restart-with-backoff policy as a later crash (see the module
    /// doc). Test-only knob: production always goes through [`Self::spawn`]
    /// (`SupervisorConfig::default()`); this module's tests substitute tiny
    /// real durations instead — see [`SupervisorConfig`]'s doc for why.
    async fn spawn_with_config(spec: ExtensionSpec, cfg: SupervisorConfig) -> SupervisedExtension {
        let proc = ExtensionProc::spawn_and_handshake(spec.clone()).await;
        let state = Arc::new(Mutex::new(proc));
        let cancel = CancellationToken::new();
        let supervisor = tokio::spawn(supervise(spec.clone(), state.clone(), cancel.clone(), cfg));
        SupervisedExtension {
            spec,
            state,
            cancel,
            supervisor: Some(supervisor),
        }
    }

    /// The manifest-declared name (`ExtensionSpec::name`), stable across
    /// restarts.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The current lifecycle status — DT4's required accessor so
    /// `extension_status`/`plugin_doctor` (DT8) can read supervised state
    /// without reaching into the lock themselves.
    pub async fn status(&self) -> ExtensionStatus {
        self.state.lock().await.status.clone()
    }

    /// A full point-in-time snapshot — see [`ExtensionSnapshot`].
    pub async fn snapshot(&self) -> ExtensionSnapshot {
        let guard = self.state.lock().await;
        ExtensionSnapshot {
            name: self.spec.name.clone(),
            status: guard.status.clone(),
            confirmed_events: guard.confirmed_events.clone(),
            tools: guard.tools.clone(),
        }
    }

    /// A cheap [`DispatchHandle`] snapshot of this entry's live dispatch
    /// state (DT5) — see that type's doc.
    pub(crate) fn dispatch_handle(&self) -> DispatchHandle {
        DispatchHandle {
            name: self.spec.name.clone(),
            state: self.state.clone(),
            timeout: self.spec.timeout,
        }
    }

    /// Whether this extension's manifest declared `provides_tools` (DT6) —
    /// `tools::ExtensionTools::session_tools` gates gathering on this so an
    /// extension that never opted in contributes zero tools regardless of
    /// what (if anything) it happened to return at init.
    pub(crate) fn provides_tools(&self) -> bool {
        self.spec.provides_tools
    }

    /// A cheap [`ExtensionCaller`] handle for dispatching `tool/call` to this
    /// entry (DT6) — the tool-call analogue of [`Self::dispatch_handle`].
    /// Reuses [`ExtensionSpec::timeout`] as the per-call budget, the same
    /// manifest knob DT5's gating event dispatch enforces.
    pub(crate) fn tool_caller(&self) -> Arc<dyn ExtensionCaller> {
        Arc::new(ToolCallHandle {
            name: self.spec.name.clone(),
            state: self.state.clone(),
            timeout: self.spec.timeout,
        })
    }

    /// Stop supervision, then gracefully shut down whatever subprocess is
    /// currently live (there may be none, mid-backoff, or after a give-up).
    /// Cancels and joins the supervisor task BEFORE touching `child`/`io` —
    /// see the module doc's "Shutdown stops supervision" section for why
    /// this ordering is what rules out a restart-after-shutdown race. Safe
    /// to call more than once (idempotent no-op past the first call).
    pub async fn shutdown(&mut self, grace: Duration) {
        self.cancel.cancel();
        if let Some(handle) = self.supervisor.take() {
            let _ = handle.await;
        }
        self.state.lock().await.shutdown(grace).await;
    }
}

impl Drop for SupervisedExtension {
    /// Best-effort backstop for the drop-without-`shutdown` path: a
    /// `?`-propagated error or a panic on a daemon-init branch after
    /// `spawn_all` would otherwise leave the [`supervise`] task detached,
    /// pinging and restarting forever — and because that task holds a clone of
    /// `state` (the `Arc<Mutex<ExtensionProc>>` owning the live `Child`), the
    /// child's `kill_on_drop(true)` would never fire either. Cancelling the
    /// token and aborting the task drops that clone; together with this
    /// struct's own `state` drop the `ExtensionProc` (and its `Child`) is
    /// released and reaped. The async [`Self::shutdown`] is still preferred —
    /// it stops the child *gracefully*; this only rules out a silent leak.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.supervisor.take() {
            handle.abort();
        }
    }
}

/// Caps concurrently in-flight OBSERVATIONAL event sends (DT5, see
/// `events`'s module doc) across every extension in one [`ExtensionHost`].
/// Gating dispatch is not bounded by this at all — it's awaited
/// synchronously, once, by its single caller, already bounded by each
/// extension's own per-event `timeout`.
const MAX_INFLIGHT_OBSERVATIONAL_SENDS: usize = 32;

/// Owns every spawned extension subprocess, keyed by the plugin id that
/// declared it. Each is independently [`SupervisedExtension`]-supervised
/// (DT4) — one extension restarting, backing off, or giving up never
/// touches any other entry, any other plugin, or the daemon.
///
/// `procs` lives behind a [`tokio::sync::RwLock`] (not a bare `HashMap`) so
/// every method here — including the mutating `spawn_all`/`shutdown_all` —
/// takes `&self`: the whole host is meant to be a single `Arc<ExtensionHost>`
/// shared between the daemon entry (which calls `spawn_all` once at startup
/// and `shutdown_all` once at stop) and every concurrently-running session's
/// `SessionCtx.extension_events`/`SessionCtx.extension_tools` (which only
/// ever call the read-only `dispatch`/`session_tools` — see `events`'s and
/// `tools`'s module docs). Reads (`get`, DT5's `dispatch_handles`, DT6's
/// `tool_provision_entries`) only ever hold the lock briefly to clone out
/// what they need; the actual subprocess I/O (handshakes, event round trips,
/// tool calls, graceful shutdown) always happens OUTSIDE the lock, so a slow
/// extension can never stall an unrelated `get`/dispatch call against a
/// different entry.
pub struct ExtensionHost {
    procs: tokio::sync::RwLock<HashMap<String, Vec<SupervisedExtension>>>,
    /// Plugin id -> resolved [`Principal`] for every plugin `spawn_all` has
    /// ever swept, populated alongside `procs` at spawn time (the only place
    /// a `CorePlugin`'s `manifest.id`/`manifest.name` are definitively known
    /// for a given entry — see `spawn_all`). DT6's `tool_provision_entries`
    /// reads this to attribute each gathered tool binding to its owning
    /// plugin WITHOUT ever parsing an extension/tool name string. A separate
    /// lock from `procs` (not folded into its value type) so every existing
    /// `procs`-only accessor (`get`, `dispatch_handles`, `shutdown_all`)
    /// stays untouched by this slice.
    principals: tokio::sync::RwLock<HashMap<String, Principal>>,
    observational_permits: Arc<tokio::sync::Semaphore>,
}

impl Default for ExtensionHost {
    fn default() -> ExtensionHost {
        ExtensionHost {
            procs: tokio::sync::RwLock::new(HashMap::new()),
            principals: tokio::sync::RwLock::new(HashMap::new()),
            observational_permits: Arc::new(tokio::sync::Semaphore::new(
                MAX_INFLIGHT_OBSERVATIONAL_SENDS,
            )),
        }
    }
}

impl ExtensionHost {
    pub fn new() -> ExtensionHost {
        ExtensionHost::default()
    }

    /// Spawn+handshake every [`ExtensionSpec`] every *enabled*
    /// extension-capable plugin in `host` declares (`PluginHost::is_enabled`
    /// gates it the same way it gates a connector — see `plugins::host`),
    /// and start supervision (health ping + restart-with-backoff + give-up
    /// — DT4, see [`SupervisedExtension::spawn`]) for each. A plugin whose
    /// `ExtensionFactory::extensions` call errors (e.g. a missing required
    /// setting) is logged and skipped — like any other plugin-resolution
    /// failure, it never aborts the rest of the sweep. A per-extension
    /// spawn/handshake failure is recorded as `ExtensionStatus::Failed` on
    /// that one entry and then retried by its own supervisor — also never
    /// fatal to this sweep or any other extension.
    ///
    /// Callers: intended for the daemon's entry path only (real subprocess
    /// spawn). `daemon::build_daemon` does NOT call this, so constructing a
    /// `Registries`/`Daemon` for tests stays hermetic (no real subprocess
    /// spawn) — `crates/runner/src/daemon_cmd.rs` calls it once, as a
    /// detached background task, after the daemon has genuinely started
    /// (see `ControlPlane::spawn_extensions`'s doc for why it must never be
    /// awaited inline on a startup-latency-sensitive path).
    pub async fn spawn_all(&self, host: &PluginHost, ctx: &ExtensionCtx) {
        for plugin in host.list() {
            let Some(factory) = plugin.extension.clone() else {
                continue;
            };
            match host.is_enabled(&ctx.settings, &plugin.manifest.id).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!(
                        "{}: could not determine whether the extension plugin is enabled: {e}",
                        plugin.manifest.id
                    );
                    continue;
                }
            }
            let specs = match factory.extensions(ctx).await {
                Ok(specs) => specs,
                Err(e) => {
                    tracing::warn!("{}: failed to resolve extensions: {e}", plugin.manifest.id);
                    continue;
                }
            };
            let mut procs = Vec::with_capacity(specs.len());
            for spec in specs {
                procs.push(SupervisedExtension::spawn(spec).await);
            }
            // Resolve this plugin's Principal HERE — the only place a
            // spawned entry's owning `CorePlugin.manifest.id`/`.name` are
            // definitively known (mirrors `ControlPlane::attach_plugin_mcp_servers`'s
            // own resolution site) — and record it BEFORE the `procs` entry
            // so a concurrent `tool_provision_entries` read can never observe
            // a plugin id in `procs` with no matching `principals` entry yet.
            self.principals.write().await.insert(
                plugin.manifest.id.clone(),
                Principal {
                    plugin_id: plugin.manifest.id.clone(),
                    plugin_name: plugin.manifest.name.clone(),
                },
            );
            self.procs
                .write()
                .await
                .insert(plugin.manifest.id.clone(), procs);
        }
    }

    /// A snapshot of every spawned extension for `plugin_id`, in spawn
    /// order, or empty if none were spawned (unknown plugin, disabled, or no
    /// extension capability). Async (unlike DT3's `get`) because status is
    /// now DT4-supervised state behind a lock — see [`SupervisedExtension::snapshot`].
    pub async fn get(&self, plugin_id: &str) -> Vec<ExtensionSnapshot> {
        let procs = self.procs.read().await;
        let Some(list) = procs.get(plugin_id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(list.len());
        for proc in list {
            out.push(proc.snapshot().await);
        }
        out
    }

    /// Whether this host has NO spawned entry for ANY plugin — used by
    /// `ControlPlane::start_harness_session` to decide whether a session's
    /// `SessionCtx.extension_events` should be threaded at all: `None` when
    /// nothing was ever spawned keeps every hook fire site's dispatch a true
    /// no-op (see `SessionCtx::extension_events`'s doc), not just an
    /// always-allow round trip through an empty host. Every test
    /// `ControlPlane` (which never calls `spawn_all`) satisfies this
    /// unconditionally.
    pub async fn is_empty(&self) -> bool {
        self.procs.read().await.is_empty()
    }

    /// Gracefully stop every spawned extension across every plugin,
    /// CONCURRENTLY (`futures::future::join_all`, not a sequential loop —
    /// see this module's "Concurrent `shutdown_all`" doc): daemon-stop
    /// latency is bounded by the single slowest shutdown, not `N × grace`.
    /// Each [`SupervisedExtension::shutdown`] call stops that entry's
    /// supervisor before its subprocess, so no restart races this shutdown.
    /// Safe to call on a host nothing was ever spawned into (every test
    /// `ControlPlane`, or a daemon that never reached `spawn_all`) — the
    /// write lock is taken and released immediately with nothing to iterate.
    pub async fn shutdown_all(&self, grace: Duration) {
        let mut procs = self.procs.write().await;
        let all = procs.values_mut().flat_map(|list| list.iter_mut());
        futures::future::join_all(all.map(|proc| proc.shutdown(grace))).await;
    }

    /// Try to reserve one of [`MAX_INFLIGHT_OBSERVATIONAL_SENDS`] slots for
    /// an observational dispatch (DT5's `events` module) — `None` means the
    /// cap is already saturated (a burst of slow/misbehaving extensions),
    /// telling the caller to drop this one send rather than queue behind it:
    /// observational dispatch must never grow an unbounded backlog against a
    /// live agent.
    pub(crate) fn try_acquire_observational_permit(
        &self,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.observational_permits.clone().try_acquire_owned().ok()
    }

    /// A cheap, owned [`DispatchHandle`] for every currently-spawned
    /// extension across every plugin, in no particular order — DT5's
    /// `events` module snapshots these under a brief read lock, then issues
    /// every actual `event/<name>` round trip OUTSIDE the lock (see this
    /// struct's own doc).
    pub(crate) async fn dispatch_handles(&self) -> Vec<DispatchHandle> {
        self.procs
            .read()
            .await
            .values()
            .flat_map(|list| list.iter().map(SupervisedExtension::dispatch_handle))
            .collect()
    }

    /// A read-only snapshot of every spawned extension's DT6-relevant state,
    /// each carrying its owning plugin's resolved [`Principal`] (from
    /// `principals`, populated once per plugin in [`Self::spawn_all`]) —
    /// `tools::ExtensionTools::session_tools`'s gathering point, mirroring
    /// [`Self::dispatch_handles`]'s read-briefly-then-release-the-lock
    /// discipline (both locks are only ever held long enough to clone out
    /// what's needed; no subprocess I/O happens under either). A plugin
    /// entry in `procs` with no matching `principals` entry yet (the narrow
    /// window mid-`spawn_all`, between two separate lock acquisitions) is
    /// skipped rather than panicking — its tools simply appear on the NEXT
    /// `session_tools()` call once `spawn_all` catches up.
    pub(crate) async fn tool_provision_entries(&self) -> Vec<ToolProvisionEntry> {
        let procs = self.procs.read().await;
        let principals = self.principals.read().await;
        let mut out = Vec::new();
        for (plugin_id, list) in procs.iter() {
            let Some(principal) = principals.get(plugin_id) else {
                continue;
            };
            for ext in list {
                let snap = ext.snapshot().await;
                out.push(ToolProvisionEntry {
                    principal: principal.clone(),
                    name: snap.name,
                    provides_tools: ext.provides_tools(),
                    status: snap.status,
                    tools: snap.tools,
                    caller: ext.tool_caller(),
                });
            }
        }
        out
    }
}

/// One spawned extension's DT6-relevant state — everything
/// `tools::ExtensionTools::session_tools` needs to decide whether/how to
/// contribute tools, without exposing [`SupervisedExtension`]/[`ExtensionProc`]
/// internals to the `tools` module. Produced by
/// [`ExtensionHost::tool_provision_entries`].
pub(crate) struct ToolProvisionEntry {
    pub(crate) principal: Principal,
    pub(crate) name: String,
    pub(crate) provides_tools: bool,
    pub(crate) status: ExtensionStatus,
    pub(crate) tools: Vec<Value>,
    pub(crate) caller: Arc<dyn ExtensionCaller>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::host::{CorePlugin, PluginSource};
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use async_trait::async_trait;
    use ryuzi_plugin_sdk::PluginManifest;
    use serial_test::serial;
    use std::sync::Arc;

    /// Generous per-call budget for the in-memory-duplex tests below — long
    /// enough that a correct implementation never gets near it (everything
    /// resolves as soon as the fake extension task writes its response, or
    /// as soon as EOF is observed), so hitting it always means a real
    /// regression, not test flakiness.
    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn spec(name: &str, command: &str, args: &[&str]) -> ExtensionSpec {
        ExtensionSpec {
            name: name.to_string(),
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            events: vec![HookEvent::ToolBefore],
            provides_tools: false,
            timeout: Duration::from_millis(500),
            env: vec![],
        }
    }

    // ---------- run_initialize / ExtensionIo: in-memory duplex, no real subprocess ----------
    // These exercise the protocol logic and the demux transport itself
    // ("in-process fake ... over pipes") — the fake extension's own code
    // runs as a spawned task in the SAME test process, communicating over an
    // in-memory `tokio::io::duplex` pair rather than a real OS pipe.

    #[tokio::test]
    async fn run_initialize_succeeds_against_a_well_behaved_fake() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (ext_read, mut ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            let mut ext_lines = BufReader::new(ext_read).lines();
            let line = ext_lines.next_line().await.unwrap().unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["id"].as_i64().unwrap();
            let resp = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "ok": true, "events": ["tool.before"] }
            });
            stdio_jsonrpc::write_line(&mut ext_write, &resp)
                .await
                .unwrap();
        });

        let host_lines = BufReader::new(host_read).lines();
        let io = ExtensionIo::connect(host_write, host_lines);
        let ack = run_initialize(&io, &["tool.before"], false, TEST_TIMEOUT)
            .await
            .expect("a well-behaved fake should hand back a valid ack");
        assert_eq!(ack.events, vec!["tool.before".to_string()]);
        assert!(ack.tools.is_empty());
    }

    #[tokio::test]
    async fn run_initialize_fails_on_protocol_version_mismatch() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (ext_read, mut ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            let mut ext_lines = BufReader::new(ext_read).lines();
            let line = ext_lines.next_line().await.unwrap().unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["id"].as_i64().unwrap();
            let resp = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "ok": true, "protocolVersion": "some-future-version" }
            });
            stdio_jsonrpc::write_line(&mut ext_write, &resp)
                .await
                .unwrap();
        });

        let host_lines = BufReader::new(host_read).lines();
        let io = ExtensionIo::connect(host_write, host_lines);
        let err = run_initialize(&io, &[], false, TEST_TIMEOUT)
            .await
            .expect_err("a mismatched protocol version must fail the handshake");
        assert!(matches!(err, protocol::InitError::ProtocolMismatch));
    }

    #[tokio::test]
    async fn run_initialize_fails_when_extension_closes_without_responding() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (ext_read, ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            let mut ext_lines = BufReader::new(ext_read).lines();
            let _ = ext_lines.next_line().await; // consume the request
            drop(ext_write); // close without ever responding
        });

        let host_lines = BufReader::new(host_read).lines();
        let io = ExtensionIo::connect(host_write, host_lines);
        let err = run_initialize(&io, &[], false, TEST_TIMEOUT)
            .await
            .expect_err("a closed connection must fail the handshake");
        assert!(matches!(err, protocol::InitError::Closed));
    }

    // ---------- ExtensionIo concurrency: the DT3 fix-wave's core proof ----------
    // DT4's `extension/ping` health loop and DT5's `event/<name>` dispatch
    // will both issue `request()` calls on the SAME `ExtensionIo` at the
    // SAME time. These prove that is safe: each caller gets its own
    // response no matter the wire order, and a dead transport fails every
    // caller promptly instead of leaving one hanging until its timeout.

    #[tokio::test]
    async fn concurrent_requests_each_get_their_own_response_even_when_they_arrive_out_of_order() {
        // The fake extension deliberately answers the SECOND request before
        // the first. This is exactly the scenario the old design (two
        // separate mutexes + `stdio_jsonrpc::read_response`'s "discard every
        // non-matching-id line" scan) got wrong: a caller awaiting id=1
        // would consume and drop id=2's line off the wire, leaving id=2's
        // caller to hang until its own timeout (e.g. a ping stealing an
        // event-dispatch response, or vice versa). The demux client must
        // route each response to its own caller regardless of arrival order.
        let (host_side, ext_side) = tokio::io::duplex(8192);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (ext_read, mut ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            let mut ext_lines = BufReader::new(ext_read).lines();
            let line1 = ext_lines.next_line().await.unwrap().unwrap();
            let line2 = ext_lines.next_line().await.unwrap().unwrap();
            let req1: Value = serde_json::from_str(&line1).unwrap();
            let req2: Value = serde_json::from_str(&line2).unwrap();
            let id1 = req1["id"].as_i64().unwrap();
            let id2 = req2["id"].as_i64().unwrap();

            // Respond to id2 BEFORE id1 — out of order relative to request
            // order.
            let resp2 = serde_json::json!({
                "jsonrpc": "2.0", "id": id2, "result": { "who": "second" }
            });
            stdio_jsonrpc::write_line(&mut ext_write, &resp2)
                .await
                .unwrap();
            let resp1 = serde_json::json!({
                "jsonrpc": "2.0", "id": id1, "result": { "who": "first" }
            });
            stdio_jsonrpc::write_line(&mut ext_write, &resp1)
                .await
                .unwrap();
        });

        let host_lines = BufReader::new(host_read).lines();
        let io = ExtensionIo::connect(host_write, host_lines);

        let id1 = io.alloc_id();
        let id2 = io.alloc_id();
        let req1 = stdio_jsonrpc::build_request(id1, "extension/probe", None);
        let req2 = stdio_jsonrpc::build_request(id2, "extension/probe", None);

        let (r1, r2) = tokio::join!(
            io.request(id1, req1, TEST_TIMEOUT),
            io.request(id2, req2, TEST_TIMEOUT),
        );

        let r1 = r1.expect("caller for id1 must get its own response, not a timeout");
        let r2 = r2.expect("caller for id2 must get its own response, not a timeout");
        assert_eq!(
            r1["result"]["who"], "first",
            "id1's caller must get id1's response even though id2's arrived first on the wire"
        );
        assert_eq!(
            r2["result"]["who"], "second",
            "id2's caller must get id2's response, never id1's"
        );
    }

    #[tokio::test]
    async fn eof_fails_every_pending_request_promptly_instead_of_hanging() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (ext_read, ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            // Consume both request lines, then die without responding to
            // either — simulates the extension process exiting mid-flight.
            let mut ext_lines = BufReader::new(ext_read).lines();
            let _ = ext_lines.next_line().await;
            let _ = ext_lines.next_line().await;
            drop(ext_write);
        });

        let host_lines = BufReader::new(host_read).lines();
        let io = ExtensionIo::connect(host_write, host_lines);

        let id1 = io.alloc_id();
        let id2 = io.alloc_id();
        let req1 = stdio_jsonrpc::build_request(id1, "extension/probe", None);
        let req2 = stdio_jsonrpc::build_request(id2, "extension/probe", None);

        let (r1, r2) = tokio::join!(
            io.request(id1, req1, TEST_TIMEOUT),
            io.request(id2, req2, TEST_TIMEOUT),
        );
        assert!(
            matches!(r1, Err(TransportError::Closed)),
            "expected Closed (from the EOF drain), got {r1:?}"
        );
        assert!(
            matches!(r2, Err(TransportError::Closed)),
            "expected Closed (from the EOF drain), got {r2:?}"
        );

        // A subsequent request on the now-closed transport must fail
        // immediately too, not attempt a doomed write/wait.
        let id3 = io.alloc_id();
        let req3 = stdio_jsonrpc::build_request(id3, "extension/probe", None);
        let r3 = io.request(id3, req3, TEST_TIMEOUT).await;
        assert!(
            matches!(r3, Err(TransportError::Closed)),
            "a request on an already-closed transport must fail immediately: {r3:?}"
        );
    }

    // ---------- spawn_and_handshake / shutdown: real subprocesses ----------
    // env_clear is an OS-process-level fact, and a full spawn -> handshake ->
    // shutdown proof needs a real `Command`/`Child` — these use only
    // universally-available unix coreutils (`env`, `sh`), never a committed
    // script file, and are gated `#[cfg(unix)]` to match this crate's own
    // `cargo test` CI matrix (ubuntu/macos only — see `hooks.rs`'s existing
    // `#[cfg(unix)]` precedent for the same reasoning).

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn spawned_child_env_is_cleared_except_the_allowlist() {
        std::env::set_var("RYUZI_SECRET_SENTINEL", "leak-me-not");
        std::env::set_var("RYUZI_TEST_UNRELATED_VAR", "also-must-not-leak");

        let mut ext_spec = spec("envcheck", "env", &[]);
        ext_spec.env = vec![("EXT_ALLOWED".to_string(), "yes".to_string())];

        let mut cmd = build_command(&ext_spec);
        let output = cmd.output().await.expect("`env` must be spawnable");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("RYUZI_SECRET_SENTINEL"),
            "a non-allowlisted daemon env var must be absent from the child's environment:\n{stdout}"
        );
        assert!(
            !stdout.contains("RYUZI_TEST_UNRELATED_VAR"),
            "env_clear must remove every non-allowlisted var, not just ones that look secret:\n{stdout}"
        );
        assert!(
            stdout.contains("EXT_ALLOWED=yes"),
            "an explicitly allowlisted extension env entry must be present:\n{stdout}"
        );
        if std::env::var("PATH").is_ok() {
            assert!(
                stdout.contains("PATH="),
                "the safe base PATH must survive env_clear:\n{stdout}"
            );
        }

        std::env::remove_var("RYUZI_SECRET_SENTINEL");
        std::env::remove_var("RYUZI_TEST_UNRELATED_VAR");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_and_handshake_then_shutdown_full_lifecycle() {
        // A tiny, real subprocess (no committed script file): reply once
        // with a fixed, valid `extension/initialize` ack (id is always 1 —
        // `spawn_and_handshake` always sends the handshake as request id
        // 1), then block on a second stdin line so the process is still
        // alive for `shutdown()` to negotiate with.
        let fake = spec(
            "lifecycle",
            "sh",
            &[
                "-c",
                "read -r _line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true,\"events\":[\"tool.before\"]}}'; read -r _line2",
            ],
        );

        let mut extproc = ExtensionProc::spawn_and_handshake(fake).await;
        assert_eq!(
            extproc.status,
            ExtensionStatus::Running,
            "a well-behaved real subprocess must hand back Running, got {:?}",
            extproc.status
        );
        assert_eq!(extproc.confirmed_events, vec!["tool.before".to_string()]);

        extproc.shutdown(SHUTDOWN_GRACE).await;
        assert_eq!(extproc.status, ExtensionStatus::Stopped);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_and_handshake_reports_failed_for_a_malformed_response_without_crashing() {
        // `cat` echoes our own request line straight back — valid JSON with
        // a matching `id`, but no `result`/`error` key, so the handshake
        // must fail as `Malformed`, non-fatally.
        let fake = spec("catfake", "cat", &[]);
        let extproc = ExtensionProc::spawn_and_handshake(fake).await;
        match &extproc.status {
            ExtensionStatus::Failed(reason) => {
                assert!(
                    reason.contains("catfake"),
                    "reason should name the extension: {reason}"
                );
                assert!(
                    !reason.to_lowercase().contains("jsonrpc"),
                    "reason must be sanitized, not raw echoed JSON: {reason}"
                );
            }
            other => panic!("expected Failed for a malformed response, got {other:?}"),
        }
        assert!(extproc.confirmed_events.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn spawn_and_handshake_reports_failed_on_timeout() {
        // `sleep` never writes anything back — the handshake must time out.
        // `start_paused` fast-forwards tokio's virtual clock past
        // `INIT_HANDSHAKE_TIMEOUT` without the test actually waiting 25
        // real-world seconds.
        let fake = spec("hangfake", "sleep", &["100"]);
        let extproc = ExtensionProc::spawn_and_handshake(fake).await;
        match &extproc.status {
            ExtensionStatus::Failed(reason) => {
                assert!(
                    reason.contains("timed out"),
                    "reason should say timed out: {reason}"
                );
            }
            other => panic!("expected Failed on timeout, got {other:?}"),
        }
    }

    // ---------- ExtensionHost: gating + aggregate spawn/shutdown ----------

    struct FakeExtensionFactory {
        specs: Vec<ExtensionSpec>,
    }

    #[async_trait]
    impl super::super::ExtensionFactory for FakeExtensionFactory {
        async fn extensions(&self, _ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>> {
            Ok(self.specs.clone())
        }
    }

    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: id.to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        }
    }

    fn extension_only(id: &str, specs: Vec<ExtensionSpec>) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: None,
            extension: Some(Arc::new(FakeExtensionFactory { specs })),
            source: PluginSource::Builtin,
        }
    }

    async fn open_ctx() -> (ExtensionCtx, Arc<Store>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        (ExtensionCtx { settings }, store, tmp)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_all_only_spawns_for_an_enabled_extension_plugin_then_shutdown_all_stops_it() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        host.add(extension_only(
            "disabled-ext",
            vec![spec("noop", "cat", &[])],
        ));
        host.add(extension_only(
            "enabled-ext",
            vec![spec(
                "lifecycle",
                "sh",
                &[
                    "-c",
                    "read -r _line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true,\"events\":[]}}'; read -r _line2",
                ],
            )],
        ));
        store
            .set_setting_raw("plugin.enabled-ext.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        assert!(
            ext_host.get("disabled-ext").await.is_empty(),
            "a disabled extension-capable plugin must not be spawned"
        );
        let running = ext_host.get("enabled-ext").await;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, ExtensionStatus::Running);

        ext_host.shutdown_all(SHUTDOWN_GRACE).await;
        assert_eq!(
            ext_host.get("enabled-ext").await[0].status,
            ExtensionStatus::Stopped
        );
    }

    // ---------- DT4: supervision (health ping, restart-with-backoff, give-up, shutdown races) ----------
    // A real, minimal `sh` subprocess plays the fake extension (same
    // no-committed-script-file, `#[cfg(unix)]` precedent as the DT3 tests
    // above). Deliberately NOT `start_paused`: a real child's response
    // arrives in genuine wall-clock time, and racing that against a PAUSED
    // clock's "jump to the next timer the instant nothing is immediately
    // pollable" behavior can fire `INIT_HANDSHAKE_TIMEOUT` before the OS has
    // even scheduled the freshly-spawned child to run (confirmed empirically
    // — every test below failed with "initialize timed out" under
    // `start_paused` even for a well-behaved fake). Instead, every test here
    // uses [`SupervisorConfig`] to shrink `PING_INTERVAL`/backoff to a few
    // tens of milliseconds of REAL time, and polls for the state transition
    // it cares about (`wait_for_status`/`wait_for_attempt_count`) rather than
    // guessing a fixed sleep — fast, deterministic, and immune to the
    // paused-clock race.

    /// A `sh` loop body: read one line, extract its `"id"`, ack it with
    /// `{"result":{"ok":true}}`, repeat forever. `result.ok == true` with no
    /// `events`/`error` is a valid reply to BOTH `extension/initialize`
    /// (`parse_initialize_response` only requires `ok == true`; absent
    /// `events` defaults to empty) and `extension/ping`
    /// (`parse_ping_response` only requires no `error`), so this one loop
    /// alone is a complete, indefinitely-healthy fake extension — see
    /// [`ack_forever_spec`].
    fn ack_forever_body() -> &'static str {
        "while IFS= read -r line; do id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"ok\":true}}\\n' \"$id\"; done"
    }

    /// A fake extension that acks its `extension/initialize` handshake with
    /// a valid ack, then acks every subsequent request (pings, a repeat
    /// ping, anything) forever — i.e. a well-behaved, indefinitely-healthy
    /// extension that never exits on its own (including when it receives
    /// `extension/shutdown` — it acks that too, then keeps waiting for more
    /// input), so a `shutdown()` against one of these always rides out the
    /// full grace period before the hard-kill fallback — see [`TEST_GRACE`].
    /// Optionally appends a byte to `attempt_log` on every invocation so a
    /// test can prove exactly how many times this command was spawned (each
    /// restart is a fresh OS process) — see [`attempt_count`].
    fn ack_forever_spec(name: &str, attempt_log: Option<&std::path::Path>) -> ExtensionSpec {
        let log_line = attempt_log
            .map(|p| format!("printf 'x' >> '{}'; ", p.display()))
            .unwrap_or_default();
        let body = format!("{log_line}{}", ack_forever_body());
        spec(name, "sh", &["-c", &body])
    }

    /// A fake extension that acks its `extension/initialize` handshake once
    /// and then exits immediately (closing its stdout) — simulates a crash
    /// right after a successful startup. Appends to `attempt_log` like
    /// [`ack_forever_spec`].
    fn dies_after_handshake_spec(name: &str, attempt_log: &std::path::Path) -> ExtensionSpec {
        let body = format!(
            "printf 'x' >> '{}'; IFS= read -r line; id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"ok\":true,\"events\":[]}}}}\\n' \"$id\"",
            attempt_log.display()
        );
        spec(name, "sh", &["-c", &body])
    }

    /// A fake extension that spawns fine, appends one byte to `attempt_log`,
    /// then exits immediately without ever answering the handshake — so
    /// `spawn_and_handshake` always resolves `Failed` (`InitError::Closed`),
    /// never `Running`, on every attempt (initial AND every restart).
    fn always_fails_after_spawning_spec(
        name: &str,
        attempt_log: &std::path::Path,
    ) -> ExtensionSpec {
        let body = format!("printf 'x' >> '{}'; exit 1", attempt_log.display());
        spec(name, "sh", &["-c", &body])
    }

    /// A fake extension whose command does not exist at all — every
    /// `spawn_and_handshake` attempt fails at the OS `spawn()` call itself
    /// (before any subprocess ever runs), simulating a permanently broken
    /// extension.
    fn never_spawns_spec(name: &str) -> ExtensionSpec {
        spec(name, "ryuzi-dt4-test-nonexistent-command-xyz", &[])
    }

    fn attempt_count(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path).map(|s| s.len()).unwrap_or(0)
    }

    /// A small, entirely-real (non-paused) `grace` for these tests'
    /// `shutdown()`/`shutdown_all()` calls — [`ack_forever_spec`] never
    /// exits on its own on receiving `extension/shutdown`, so a shutdown
    /// against one always rides out the full grace period before the
    /// `ExtensionProc::shutdown` hard-kill fallback fires; the production
    /// `SHUTDOWN_GRACE` (5s) would make every such test slow for no
    /// assertion benefit.
    const TEST_GRACE: Duration = Duration::from_millis(200);

    /// Timing knobs fast enough to keep a real-subprocess supervision test
    /// in the tens-to-low-hundreds-of-milliseconds range, while still
    /// leaving comfortable real-time headroom over how long a trivial `sh`
    /// spawn+handshake actually takes.
    fn fast_test_cfg() -> SupervisorConfig {
        SupervisorConfig {
            ping_interval: Duration::from_millis(40),
            ping_timeout: Duration::from_millis(500),
            restart_backoff_base: Duration::from_millis(40),
            restart_backoff_cap: Duration::from_secs(2),
            max_restarts_in_window: MAX_RESTARTS_IN_WINDOW,
            restart_window: Duration::from_secs(30),
            healthy_reset_after: Duration::from_millis(500),
        }
    }

    /// Poll `supervised.status()` every 5ms (real time) until `pred` matches
    /// or `timeout` elapses (panicking with the last-observed status on
    /// timeout) — avoids guessing a fixed sleep for a background task's
    /// state transition.
    async fn wait_for_status(
        supervised: &SupervisedExtension,
        timeout: Duration,
        pred: impl Fn(&ExtensionStatus) -> bool,
    ) -> ExtensionStatus {
        let start = std::time::Instant::now();
        loop {
            let status = supervised.status().await;
            if pred(&status) {
                return status;
            }
            assert!(
                start.elapsed() < timeout,
                "timed out after {timeout:?} waiting for a status transition; last observed: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Poll `attempt_count(path)` every 5ms (real time) until it reaches
    /// `target` or `timeout` elapses (panicking on timeout).
    async fn wait_for_attempt_count(path: &std::path::Path, target: usize, timeout: Duration) {
        let start = std::time::Instant::now();
        loop {
            let n = attempt_count(path);
            if n >= target {
                return;
            }
            assert!(
                start.elapsed() < timeout,
                "timed out after {timeout:?} waiting for attempt_count to reach {target}, currently {n}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_restarts_a_crashed_extension_and_becomes_healthy_again() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        // The command behaves differently by attempt count: dies right after
        // the first handshake, then (from the second invocation on) acks
        // forever. This is what lets ONE `ExtensionSpec`/command model "the
        // process crashed, and a respawn of the same command comes back
        // healthy" without an injectable spawner seam.
        let body = format!(
            "printf 'x' >> '{path}'; n=$(wc -c < '{path}'); IFS= read -r line; id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"ok\":true,\"events\":[]}}}}\\n' \"$id\"; if [ \"$n\" -eq 1 ]; then exit 0; fi; {ack}",
            path = tmp.path().display(),
            ack = ack_forever_body(),
        );
        let spec = spec("flaky", "sh", &["-c", &body]);

        let mut supervised = SupervisedExtension::spawn_with_config(spec, fast_test_cfg()).await;
        assert_eq!(
            supervised.status().await,
            ExtensionStatus::Running,
            "the first invocation must hand back a healthy ack"
        );
        assert_eq!(attempt_count(tmp.path()), 1);

        // The (already-exited) first process's transport is closed, so the
        // next health ping fails immediately -> Restarting -> a short
        // backoff -> a fresh spawn_and_handshake of the SAME command, whose
        // second invocation is the "ack forever" branch.
        wait_for_status(&supervised, Duration::from_secs(5), |s| {
            matches!(s, ExtensionStatus::Restarting)
        })
        .await;
        wait_for_status(&supervised, Duration::from_secs(5), |s| {
            matches!(s, ExtensionStatus::Running)
        })
        .await;

        assert_eq!(
            attempt_count(tmp.path()),
            2,
            "the supervisor must have spawned a NEW process (a second handshake), not reused the dead one"
        );

        supervised.shutdown(TEST_GRACE).await;
        assert_eq!(supervised.status().await, ExtensionStatus::Stopped);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_backoff_grows_exponentially_before_giving_up() {
        // A command that spawns and marks its own attempt, but never
        // completes the handshake — every attempt (the initial one AND
        // every restart) fails, forcing exactly MAX_RESTARTS_IN_WINDOW
        // restart attempts before give-up, with an observable marker per
        // attempt to pin down exactly WHEN each one happened.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let cfg = SupervisorConfig {
            // Big enough that none of the 5 steps below (40*2^4=640ms) get
            // clamped — capping is covered separately by the pure formula
            // test.
            restart_backoff_cap: Duration::from_secs(5),
            ..fast_test_cfg()
        };
        let spec = always_fails_after_spawning_spec("broken", tmp.path());

        let supervised = SupervisedExtension::spawn_with_config(spec, cfg).await;
        assert!(
            matches!(supervised.status().await, ExtensionStatus::Failed(_)),
            "a handshake that's never answered must start out Failed"
        );
        assert_eq!(
            attempt_count(tmp.path()),
            1,
            "the initial spawn is attempt #1"
        );

        // Each step below waits for the NEXT restart attempt's marker and
        // measures the real elapsed time since the previous one — proving
        // the delay before each successive attempt actually grows
        // (1x, 2x, 4x, 8x, 16x the base), not just that 5 restarts
        // eventually happen somewhere within a generous window.
        let mut checkpoint = std::time::Instant::now();
        for attempt_index in 0..cfg.max_restarts_in_window {
            let expected_backoff = backoff_for_attempt(attempt_index as usize, &cfg);
            let target = 2 + attempt_index as usize;
            wait_for_attempt_count(
                tmp.path(),
                target,
                expected_backoff + Duration::from_secs(2),
            )
            .await;
            let elapsed = checkpoint.elapsed();
            assert!(
                elapsed >= expected_backoff.saturating_sub(Duration::from_millis(20)),
                "restart attempt #{} arrived after only {elapsed:?}, before its {expected_backoff:?} backoff could have elapsed",
                attempt_index + 1
            );
            checkpoint = std::time::Instant::now();
        }

        // The 6th would-be attempt is refused: MAX_RESTARTS_IN_WINDOW (5)
        // restarts already happened inside RESTART_WINDOW, so the give-up
        // check fires instead of another backoff+respawn.
        let status = wait_for_status(&supervised, Duration::from_secs(2), |s| {
            matches!(s, ExtensionStatus::Failed(_))
        })
        .await;
        match &status {
            ExtensionStatus::Failed(reason) => {
                assert!(
                    reason.starts_with("restart-exhausted:"),
                    "give-up reason should be the sanitized restart-exhausted marker: {reason}"
                );
            }
            other => panic!("expected Failed(restart-exhausted) after give-up, got {other:?}"),
        }
        assert_eq!(
            attempt_count(tmp.path()),
            6,
            "give-up must happen instead of a 6th respawn attempt"
        );

        // No further restarts after give-up: neither the status nor the
        // attempt count budges even after waiting comfortably longer than
        // another backoff round would have taken (the supervisor task
        // returned for good on give-up).
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(supervised.status().await, status);
        assert_eq!(attempt_count(tmp.path()), 6);
    }

    #[test]
    fn backoff_for_attempt_matches_the_documented_exponential_capped_formula() {
        let cfg = SupervisorConfig::default();
        assert_eq!(backoff_for_attempt(0, &cfg), RESTART_BACKOFF_BASE);
        assert_eq!(backoff_for_attempt(1, &cfg), Duration::from_secs(2));
        assert_eq!(backoff_for_attempt(2, &cfg), Duration::from_secs(4));
        assert_eq!(backoff_for_attempt(3, &cfg), Duration::from_secs(8));
        assert_eq!(backoff_for_attempt(4, &cfg), Duration::from_secs(16));
        assert_eq!(backoff_for_attempt(5, &cfg), Duration::from_secs(32));
        assert_eq!(
            backoff_for_attempt(6, &cfg),
            RESTART_BACKOFF_CAP,
            "64s uncapped must clamp to the 60s cap"
        );
        assert_eq!(
            backoff_for_attempt(1_000, &cfg),
            RESTART_BACKOFF_CAP,
            "a huge attempt index must stay capped, never overflow/panic"
        );
    }

    /// The [`Drop`] backstop: dropping a `SupervisedExtension` WITHOUT calling
    /// `shutdown` (the `?`-error / panic-after-`spawn_all` path) must still
    /// cancel supervision so the detached task can't keep pinging/restarting
    /// forever. We observe the token the supervise task races on: after drop it
    /// must be cancelled, which drives that task to exit (and drop its
    /// `state`/`Child` clone).
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_without_shutdown_cancels_supervision() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let supervised = SupervisedExtension::spawn_with_config(
            ack_forever_spec("orphan", Some(tmp.path())),
            fast_test_cfg(),
        )
        .await;
        // Clone the very token the `supervise` task selects on, then drop the
        // handle without `shutdown`.
        let token = supervised.cancel.clone();
        assert!(!token.is_cancelled(), "sanity: live before drop");
        drop(supervised);
        assert!(
            token.is_cancelled(),
            "Drop must cancel supervision so the task can't run detached"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_while_running_stops_cleanly_with_no_restart() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let mut supervised = SupervisedExtension::spawn_with_config(
            ack_forever_spec("healthy", Some(tmp.path())),
            fast_test_cfg(),
        )
        .await;
        assert_eq!(supervised.status().await, ExtensionStatus::Running);
        assert_eq!(attempt_count(tmp.path()), 1);

        supervised.shutdown(TEST_GRACE).await;

        assert_eq!(supervised.status().await, ExtensionStatus::Stopped);
        // Shutdown while Running must never be preceded by a spurious
        // restart.
        assert_eq!(attempt_count(tmp.path()), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_during_a_backoff_wait_stops_cleanly_with_no_restart_triggered() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        // A big backoff relative to the ping interval: gives this test a
        // wide, comfortable real-time window between "the supervisor has
        // detected the crash and entered its backoff wait" and "the backoff
        // would actually elapse and trigger a respawn" — shutdown must land
        // inside that window without racing it.
        let cfg = SupervisorConfig {
            ping_interval: Duration::from_millis(30),
            restart_backoff_base: Duration::from_secs(3),
            ..fast_test_cfg()
        };
        let mut supervised = SupervisedExtension::spawn_with_config(
            dies_after_handshake_spec("dies-once", tmp.path()),
            cfg,
        )
        .await;
        assert_eq!(supervised.status().await, ExtensionStatus::Running);
        assert_eq!(attempt_count(tmp.path()), 1);

        // The ping interval elapses, the already-exited process's transport
        // is closed so the ping fails immediately, and the supervisor enters
        // its (3s) backoff sleep.
        wait_for_status(&supervised, Duration::from_secs(2), |s| {
            matches!(s, ExtensionStatus::Restarting)
        })
        .await;
        assert_eq!(
            attempt_count(tmp.path()),
            1,
            "must not have respawned yet — still mid-backoff"
        );

        // Shutdown races the supervisor's `cancel.cancelled()` branch
        // against its `sleep(backoff)` branch — cancellation is an
        // immediate event, not gated on the 3s backoff, so it wins.
        supervised.shutdown(TEST_GRACE).await;

        assert_eq!(supervised.status().await, ExtensionStatus::Stopped);
        assert_eq!(
            attempt_count(tmp.path()),
            1,
            "shutdown mid-backoff must never let the queued restart happen"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_give_up_extension_never_affects_a_healthy_sibling() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        host.add(extension_only(
            "broken-plugin",
            vec![never_spawns_spec("broken")],
        ));
        host.add(extension_only(
            "healthy-plugin",
            vec![ack_forever_spec("healthy", None)],
        ));
        store
            .set_setting_raw("plugin.broken-plugin.enabled", "true")
            .await
            .unwrap();
        store
            .set_setting_raw("plugin.healthy-plugin.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        // Checked immediately (no wait): the broken extension's own
        // supervisor is independently retrying/backing off in the
        // background (its status may already be `Failed` from the initial
        // attempt or `Restarting` if its supervisor task got a scheduling
        // turn first — either is "not Running", which is the only thing
        // this test needs), but the healthy sibling must be completely
        // unaffected either way.
        let healthy = ext_host.get("healthy-plugin").await;
        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0].status, ExtensionStatus::Running);
        let broken = ext_host.get("broken-plugin").await;
        assert_eq!(broken.len(), 1);
        assert!(
            matches!(
                broken[0].status,
                ExtensionStatus::Failed(_) | ExtensionStatus::Restarting
            ),
            "the broken extension must be unhealthy, got {:?}",
            broken[0].status
        );

        ext_host.shutdown_all(TEST_GRACE).await;
        assert_eq!(
            ext_host.get("healthy-plugin").await[0].status,
            ExtensionStatus::Stopped
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_all_stops_every_proc_concurrently() {
        // Three procs that never react to `extension/shutdown` themselves
        // (the ack-forever loop just keeps acking anything it reads,
        // including the shutdown notification) so every one of them must
        // ride out the full `grace` hard-kill fallback in
        // `ExtensionProc::shutdown` — sequential would take ~3x `grace`;
        // concurrent (`join_all`) takes ~1x. No `start_paused` here: this
        // test asserts real wall-clock concurrency, so `grace` is kept small
        // to stay fast either way.
        let grace = Duration::from_millis(200);
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        host.add(extension_only(
            "multi",
            vec![
                ack_forever_spec("one", None),
                ack_forever_spec("two", None),
                ack_forever_spec("three", None),
            ],
        ));
        store
            .set_setting_raw("plugin.multi.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        for snap in ext_host.get("multi").await {
            assert_eq!(snap.status, ExtensionStatus::Running);
        }

        let start = std::time::Instant::now();
        ext_host.shutdown_all(grace).await;
        let elapsed = start.elapsed();

        for snap in ext_host.get("multi").await {
            assert_eq!(snap.status, ExtensionStatus::Stopped);
        }
        assert!(
            elapsed < grace * 2,
            "shutdown_all of 3 procs each needing the full grace period took {elapsed:?} \
             (>= 2x grace={grace:?}) — looks sequential, not concurrent"
        );
    }
}
