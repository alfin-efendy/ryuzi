//! `__daemon`: the hidden background-process entry point. Spawned detached
//! by the TUI's `s` key (`AppController::start_daemon`, `crates/cli/src/tui/
//! controller.rs`) as `[current_exe, "__daemon"]` — never invoked directly by
//! a user, and deliberately absent from `--help`.
//!
//! Port of the retired TypeScript `runDaemon` / `startWithTimeout` /
//! `makeShutdown` (`apps/cli/src/cli/daemon-process.ts`), minus the
//! `UpdateManager` (deferred — see the 4D-a plan's design notes: "No
//! UpdateManager wiring in `__daemon`").

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ryuzi_core::daemon::{build_daemon, BuildDaemonOpts, Daemon};
use ryuzi_core::daemon_status::{clear_status, write_status, DaemonFileState, DaemonStatusFile};
use ryuzi_core::AcpAdapterDescriptor;

use crate::dispatch::Deps;

/// TS parity: `daemon-process.ts`'s `CONNECT_TIMEOUT_MS`.
const CONNECT_TIMEOUT_MS: u64 = 30_000;

pub fn cmd_daemon(deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_daemon(deps))
}

/// Race an arbitrary future against a `ms`-millisecond deadline — port of TS
/// `startWithTimeout(daemon, ms)`. Generic over the future's success type
/// (rather than tied to `Daemon`) so it's unit-testable in isolation; the
/// production call site races `build_daemon(...).and_then(Daemon::start)`.
pub(crate) async fn start_with_timeout<T, F>(fut: F, ms: u64) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    match tokio::time::timeout(Duration::from_millis(ms), fut).await {
        Ok(result) => result,
        Err(_elapsed) => anyhow::bail!("timed out connecting after {ms}ms"),
    }
}

/// Reentrancy-guarded shutdown — port of TS `makeShutdown`: at most once,
/// await `stop` (best-effort; any error is swallowed, mirroring the TS
/// try/catch around `daemon.stop()`), clear `dir`'s status file, then call
/// `exit(0)`. Generic over `stop`/`exit` so it's unit-testable without a real
/// `Daemon` or a real `std::process::exit`.
pub(crate) async fn shutdown_once<S, E>(dir: &Path, stopping: &AtomicBool, stop: S, exit: E)
where
    S: Future<Output = anyhow::Result<()>>,
    E: FnOnce(i32),
{
    if stopping.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = stop.await;
    clear_status(dir);
    exit(0);
}

async fn run_daemon(deps: &mut Deps) -> u8 {
    let dir: PathBuf = deps
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let started_at = ryuzi_core::paths::now_ms();
    let version = Some(env!("CARGO_PKG_VERSION").to_string());
    let pid = std::process::id() as i32;

    let _ = write_status(
        &dir,
        &DaemonStatusFile {
            pid,
            state: DaemonFileState::Connecting,
            started_at,
            last_error: None,
            version: version.clone(),
        },
    );

    let opts = BuildDaemonOpts {
        db_path: deps.db_path.clone(),
        // Lazily resolves the ACP sidecar (may download). `build_daemon`
        // calls this AT MOST ONCE, and only when the persisted
        // `enabled_runtimes` setting includes "claude-code" — a
        // zero-runtime daemon never touches the resolver or the network.
        adapter: Box::new(|| {
            let resolved = crate::sidecar_host::manager().resolve()?;
            Ok(AcpAdapterDescriptor {
                command: resolved.command,
                args: resolved.args,
                env: vec![],
                // REQUIRED: the adapter refuses to start inside a nested Claude Code session.
                env_remove: vec!["CLAUDECODE".to_string()],
            })
        }),
        telemetry: None,
        extra_gateway_factories: vec![],
        extra_harness_factories: vec![],
    };

    let daemon = match start_with_timeout(build_and_start(opts), CONNECT_TIMEOUT_MS).await {
        Ok(daemon) => daemon,
        Err(e) => {
            let _ = write_status(
                &dir,
                &DaemonStatusFile {
                    pid,
                    state: DaemonFileState::Error,
                    started_at,
                    last_error: Some(e.to_string()),
                    version,
                },
            );
            (deps.err)(&format!("daemon: failed to start: {e}"));
            return 1;
        }
    };

    let _ = write_status(
        &dir,
        &DaemonStatusFile {
            pid,
            state: DaemonFileState::Running,
            started_at,
            last_error: None,
            version,
        },
    );
    (deps.out)("daemon: running");

    // Deliberate delta from TS runDaemon (which registered handlers before connect): a signal during
    // the connect window falls back to default kill; the stale "connecting" file is benign — derive_state
    // treats a dead pid as stopped.
    install_signal_handlers(dir, daemon);

    // Block until a signal handler calls `std::process::exit` from within
    // `shutdown_once` — mirrors TS `runDaemon`'s
    // `await new Promise<never>(() => {})`.
    std::future::pending::<()>().await;
    unreachable!("shutdown_once exits the process before this future can resolve")
}

/// `build_daemon` then `Daemon::start` — the "connecting" phase TS's
/// `startWithTimeout(daemon, ms)` wraps. Unlike TS's synchronous
/// `buildDaemon`, both steps are async in Rust, so both are raced against
/// the single 30s deadline together.
async fn build_and_start(opts: BuildDaemonOpts) -> anyhow::Result<Daemon> {
    let daemon = build_daemon(opts).await?;
    daemon.start().await?;
    Ok(daemon)
}

/// Installs SIGTERM/SIGINT handlers that drive [`shutdown_once`]: best-effort
/// `daemon.stop()`, clear `dir`'s status file, `std::process::exit(0)`. Both
/// signals share one reentrancy guard and one `Daemon` handle so whichever
/// fires first wins and the other is a no-op.
fn install_signal_handlers(dir: PathBuf, daemon: Daemon) {
    use tokio::signal::unix::{signal, SignalKind};

    let daemon = Arc::new(daemon);
    let stopping = Arc::new(AtomicBool::new(false));

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    {
        let daemon = Arc::clone(&daemon);
        let stopping = Arc::clone(&stopping);
        let dir = dir.clone();
        tokio::spawn(async move {
            sigterm.recv().await;
            shutdown_once(
                &dir,
                &stopping,
                async {
                    daemon.stop().await;
                    Ok(())
                },
                |c| std::process::exit(c),
            )
            .await;
        });
    }

    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::spawn(async move {
        sigint.recv().await;
        shutdown_once(
            &dir,
            &stopping,
            async {
                daemon.stop().await;
                Ok(())
            },
            |c| std::process::exit(c),
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::daemon_status::{read_status, DaemonFileState};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    // ---------- start_with_timeout ----------

    #[tokio::test]
    async fn start_with_timeout_times_out_with_exact_message() {
        let fut = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<(), anyhow::Error>(())
        };
        let err = start_with_timeout(fut, 10).await.unwrap_err();
        assert_eq!(err.to_string(), "timed out connecting after 10ms");
    }

    #[tokio::test]
    async fn start_with_timeout_propagates_the_start_error() {
        let fut = async { anyhow::bail!("boom") };
        let err = start_with_timeout::<(), _>(fut, 1_000).await.unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[tokio::test]
    async fn start_with_timeout_resolves_when_start_resolves() {
        let fut = async { Ok::<u8, anyhow::Error>(7) };
        let value = start_with_timeout(fut, 1_000).await.unwrap();
        assert_eq!(value, 7);
    }

    // ---------- shutdown_once ----------

    #[tokio::test]
    async fn shutdown_once_is_reentrant_clears_status_and_exits_zero_even_when_stop_errs() {
        let dir = tempfile::tempdir().unwrap();
        write_status(
            dir.path(),
            &DaemonStatusFile {
                pid: 1,
                state: DaemonFileState::Running,
                started_at: 1,
                last_error: None,
                version: None,
            },
        )
        .unwrap();

        let stopping = AtomicBool::new(false);
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let exit_calls: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        let sc = stop_calls.clone();
        let ec = exit_calls.clone();
        shutdown_once(
            dir.path(),
            &stopping,
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("stop boom")
            },
            move |c| ec.lock().unwrap().push(c),
        )
        .await;

        assert_eq!(stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*exit_calls.lock().unwrap(), vec![0]);
        assert!(
            read_status(dir.path()).is_none(),
            "shutdown must clear the status file even when stop() errs"
        );

        // A second call must be a no-op: neither stop nor exit run again.
        let sc2 = stop_calls.clone();
        let ec2 = exit_calls.clone();
        shutdown_once(
            dir.path(),
            &stopping,
            async move {
                sc2.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            move |c| ec2.lock().unwrap().push(c),
        )
        .await;

        assert_eq!(
            stop_calls.load(Ordering::SeqCst),
            1,
            "a reentrant shutdown call must not re-invoke stop"
        );
        assert_eq!(
            *exit_calls.lock().unwrap(),
            vec![0],
            "a reentrant shutdown call must not call exit again"
        );
    }
}
