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
//! [`run_initialize`] writes `extension/initialize` and reads back a
//! matching response, generic over any `AsyncWrite`/`AsyncBufRead` pair —
//! real subprocess pipes in production, an in-memory `tokio::io::duplex`
//! pair in this module's own tests — so protocol correctness
//! (accept/reject/malformed/timeout) is exercised without spawning a
//! process. [`ExtensionProc::spawn_and_handshake`] is the only place that
//! combines that generic routine with a real `tokio::process::Command`.
//!
//! A handshake failure (bad process, timeout, protocol mismatch, rejection)
//! never surfaces as an `Err` to `spawn_and_handshake`'s caller — it always
//! returns a value, recording the failure in `status` instead (see the
//! design doc's "mismatched/failed init -> extension marked `failed` ...
//! NOT fatal to the daemon").
//!
//! # Graceful shutdown
//! [`ExtensionProc::shutdown`] sends `extension/shutdown`, gives the
//! process a grace period to exit on its own, then falls back to a hard
//! kill. `kill_on_drop(true)` (set at spawn) is the unconditional backstop
//! if `shutdown` is never called at all.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::harness::native::hooks::HookEvent;
use crate::plugins::host::PluginHost;
use crate::stdio_jsonrpc::{self, ReadError};

use super::{protocol, ExtensionCtx, ExtensionSpec, ExtensionStatus};

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
/// then read/parse/validate the response. Generic over the transport so
/// both a real subprocess's stdio pipes and (in tests) an in-memory
/// `tokio::io::duplex` half exercise identical protocol logic — see the
/// module doc.
async fn run_initialize<W, R>(
    writer: &mut W,
    lines: &mut Lines<R>,
    id: i64,
    events: &[&str],
) -> Result<protocol::InitializeAck, protocol::InitError>
where
    W: AsyncWrite + Unpin,
    R: AsyncBufRead + Unpin,
{
    let req = protocol::initialize_request(id, events);
    stdio_jsonrpc::write_line(writer, &req)
        .await
        .map_err(|e| protocol::InitError::Io(e.to_string()))?;
    let resp = match stdio_jsonrpc::read_response(lines, id).await {
        Ok(v) => v,
        Err(ReadError::Closed) => return Err(protocol::InitError::Closed),
        Err(ReadError::Io(e)) => return Err(protocol::InitError::Io(e.to_string())),
    };
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

/// The live stdio handle to a `Running` extension: its writer/reader pair
/// and the next JSON-RPC request id (1 was consumed by the initialize
/// handshake).
struct ExtensionIo {
    stdin: Mutex<ChildStdin>,
    /// Kept open across the process's whole `Running` lifetime for DT5's
    /// event dispatch (`event/<name>` requests need a response reader, and
    /// DT4's `extension/ping` health probe does too) — neither exists yet
    /// in this slice, so nothing reads through this handle here. Dropping
    /// it instead would lose the only handle to the child's stdout (`Child::stdout`
    /// is a one-shot `.take()`), which would break those later slices.
    #[allow(dead_code)]
    reader: Mutex<Lines<BufReader<ChildStdout>>>,
    next_id: AtomicI64,
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
    io: Option<ExtensionIo>,
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
        let mut writer = stdin;
        let mut lines = BufReader::new(stdout).lines();
        let events: Vec<&str> = spec.events.iter().map(HookEvent::as_str).collect();

        let outcome = tokio::time::timeout(
            INIT_HANDSHAKE_TIMEOUT,
            run_initialize(&mut writer, &mut lines, 1, &events),
        )
        .await;

        match outcome {
            Ok(Ok(ack)) => ExtensionProc {
                confirmed_events: ack.events,
                tools: ack.tools,
                status: ExtensionStatus::Running,
                child: Some(child),
                io: Some(ExtensionIo {
                    stdin: Mutex::new(writer),
                    reader: Mutex::new(lines),
                    next_id: AtomicI64::new(2), // id 1 was the initialize request
                }),
                spec,
            },
            Ok(Err(e)) => {
                let reason = sanitize_init_error(&spec.name, &e);
                let _ = child.kill().await;
                ExtensionProc::failed(spec, reason, None)
            }
            Err(_elapsed) => {
                let reason = format!("{}: initialize timed out", spec.name);
                let _ = child.kill().await;
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
            let id = io.next_id.fetch_add(1, Ordering::SeqCst);
            let req = protocol::shutdown_request(id);
            let mut stdin = io.stdin.into_inner();
            let _ = stdio_jsonrpc::write_line(&mut stdin, &req).await;
        }
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
        self.status = ExtensionStatus::Stopped;
    }
}

/// Owns every spawned extension subprocess, keyed by the plugin id that
/// declared it. Supervision (health/`extension/ping`, restart-with-backoff —
/// DT4) and event dispatch (DT5) build on this; this slice provides
/// spawn-all + shutdown-all only — there is no restart loop yet.
#[derive(Default)]
pub struct ExtensionHost {
    procs: HashMap<String, Vec<ExtensionProc>>,
}

impl ExtensionHost {
    pub fn new() -> ExtensionHost {
        ExtensionHost::default()
    }

    /// Spawn+handshake every [`ExtensionSpec`] every *enabled*
    /// extension-capable plugin in `host` declares (`PluginHost::is_enabled`
    /// gates it the same way it gates a connector — see `plugins::host`).
    /// A plugin whose `ExtensionFactory::extensions` call errors (e.g. a
    /// missing required setting) is logged and skipped — like any other
    /// plugin-resolution failure, it never aborts the rest of the sweep. A
    /// per-extension spawn/handshake failure is recorded as
    /// `ExtensionStatus::Failed` on that one `ExtensionProc` (see
    /// [`ExtensionProc::spawn_and_handshake`]) — also never fatal.
    ///
    /// Callers: intended for the daemon's entry path only (real subprocess
    /// spawn). `daemon::build_daemon` does NOT call this in this slice, so
    /// constructing a `Registries`/`Daemon` for tests stays hermetic (no
    /// real subprocess spawn) — DT5 wires the daemon-entry call once event
    /// dispatch gives spawned extensions something to do.
    pub async fn spawn_all(&mut self, host: &PluginHost, ctx: &ExtensionCtx) {
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
                procs.push(ExtensionProc::spawn_and_handshake(spec).await);
            }
            self.procs.insert(plugin.manifest.id.clone(), procs);
        }
    }

    /// Every spawned extension for `plugin_id`, or `&[]` if none were
    /// spawned (unknown plugin, disabled, or no extension capability).
    pub fn get(&self, plugin_id: &str) -> &[ExtensionProc] {
        self.procs.get(plugin_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Gracefully stop every spawned extension across every plugin (see
    /// [`ExtensionProc::shutdown`]).
    pub async fn shutdown_all(&mut self, grace: Duration) {
        for procs in self.procs.values_mut() {
            for proc in procs.iter_mut() {
                proc.shutdown(grace).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::host::{CorePlugin, PluginSource};
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use async_trait::async_trait;
    use ryuzi_plugin_sdk::PluginManifest;
    use std::sync::Arc;

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

    // ---------- run_initialize: in-memory duplex, no real subprocess ----------
    // These exercise the protocol logic itself ("in-process fake ... over
    // pipes") — the fake extension's own code runs as a spawned task in the
    // SAME test process, communicating over an in-memory
    // `tokio::io::duplex` pair rather than a real OS pipe.

    #[tokio::test]
    async fn run_initialize_succeeds_against_a_well_behaved_fake() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, mut host_write) = tokio::io::split(host_side);
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

        let mut host_lines = BufReader::new(host_read).lines();
        let ack = run_initialize(&mut host_write, &mut host_lines, 1, &["tool.before"])
            .await
            .expect("a well-behaved fake should hand back a valid ack");
        assert_eq!(ack.events, vec!["tool.before".to_string()]);
        assert!(ack.tools.is_empty());
    }

    #[tokio::test]
    async fn run_initialize_fails_on_protocol_version_mismatch() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, mut host_write) = tokio::io::split(host_side);
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

        let mut host_lines = BufReader::new(host_read).lines();
        let err = run_initialize(&mut host_write, &mut host_lines, 1, &[])
            .await
            .expect_err("a mismatched protocol version must fail the handshake");
        assert!(matches!(err, protocol::InitError::ProtocolMismatch));
    }

    #[tokio::test]
    async fn run_initialize_fails_when_extension_closes_without_responding() {
        let (host_side, ext_side) = tokio::io::duplex(4096);
        let (host_read, mut host_write) = tokio::io::split(host_side);
        let (ext_read, ext_write) = tokio::io::split(ext_side);

        tokio::spawn(async move {
            let mut ext_lines = BufReader::new(ext_read).lines();
            let _ = ext_lines.next_line().await; // consume the request
            drop(ext_write); // close without ever responding
        });

        let mut host_lines = BufReader::new(host_read).lines();
        let err = run_initialize(&mut host_write, &mut host_lines, 1, &[])
            .await
            .expect_err("a closed connection must fail the handshake");
        assert!(matches!(err, protocol::InitError::Closed));
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

        let mut ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        assert!(
            ext_host.get("disabled-ext").is_empty(),
            "a disabled extension-capable plugin must not be spawned"
        );
        let running = ext_host.get("enabled-ext");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, ExtensionStatus::Running);

        ext_host.shutdown_all(SHUTDOWN_GRACE).await;
        assert_eq!(
            ext_host.get("enabled-ext")[0].status,
            ExtensionStatus::Stopped
        );
    }
}
