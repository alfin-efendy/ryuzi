//! Headless engine-daemon mode for the Cockpit binary: `--engine-daemon`.
//! Mirrors `ryuzi __daemon` (lock → status file → build/start → token →
//! control API → signal wait) minus the self-updater — desktop builds update
//! as whole app bundles, so a canary flow does not apply here.

use ryuzi_core::daemon::{build_daemon, BuildDaemonOpts, Daemon};
use ryuzi_core::daemon_status::{clear_status, write_status, DaemonFileState, DaemonStatusFile};
use ryuzi_core::settings::SettingsStore;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_CONTROL_PORT: u16 = 4483;

/// How long the daemon gets to build and start before this process gives up
/// and reports a "timed out connecting" error — mirrors `ryuzi-runner`'s
/// `daemon_cmd::CONNECT_TIMEOUT_MS`.
const CONNECT_TIMEOUT_MS: u64 = 30_000;

pub fn run() -> i32 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_inner())
}

async fn run_inner() -> i32 {
    let db_path = ryuzi_core::paths::db_path();
    let dir: PathBuf = db_path
        .parent()
        .map(Into::into)
        .unwrap_or_else(|| ".".into());

    let _lock = match ryuzi_core::daemon_lock::DaemonLock::acquire(&dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("engine-daemon: {e}");
            return 1;
        }
    };

    let pid = std::process::id() as i32;
    let started_at = ryuzi_core::paths::now_ms();
    let version = Some(env!("CARGO_PKG_VERSION").to_string());
    let _ = write_status(
        &dir,
        &DaemonStatusFile {
            pid,
            state: DaemonFileState::Connecting,
            started_at,
            last_error: None,
            version: version.clone(),
            port: None,
            scheme: None,
            host: None,
            fingerprint: None,
        },
    );

    let opts = BuildDaemonOpts {
        db_path,
        telemetry: None,
        extra_gateway_factories: ryuzi_core::gateway::discord::factory_entries(),
        // Native-only: production uses the real in-process native harness.
        harness_factory: None,
    };

    let daemon = match tokio::time::timeout(Duration::from_millis(CONNECT_TIMEOUT_MS), async {
        let d = build_daemon(opts).await?;
        // `Daemon::start`'s own rollback already unwinds any gateway it
        // managed to start before hitting the one that failed, so no extra
        // stop() is needed on this branch.
        d.start().await?;
        Ok::<Daemon, anyhow::Error>(d)
    })
    .await
    {
        Ok(Ok(d)) => std::sync::Arc::new(d),
        Ok(Err(e)) => return fail(&dir, pid, started_at, version, &e.to_string()),
        Err(_elapsed) => {
            return fail(
                &dir,
                pid,
                started_at,
                version,
                &format!("timed out connecting after {CONNECT_TIMEOUT_MS}ms"),
            )
        }
    };

    // The daemon has already started (gateways claimed, reconcile() may have
    // fired) by the time we're wiring up the control API below — mirrors
    // `ryuzi-runner`'s `daemon_cmd::run_daemon`: on any failure from here on,
    // stop it before reporting Error so a failed control API doesn't leave
    // an orphaned daemon process with no status reflecting the failure.
    let token = match ryuzi_core::control_token::write_token(&dir) {
        Ok(t) => t,
        Err(e) => {
            return fail_after_start(&dir, pid, started_at, version, &daemon, &e.to_string()).await
        }
    };
    let settings = SettingsStore::new(daemon.store.clone());
    let control_port: u16 = settings
        .get("control_port")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CONTROL_PORT);
    let bound = match ryuzi_core::serve::serve(
        ryuzi_core::serve::ApiState {
            cp: daemon.cp.clone(),
            router_server: daemon.router_server.clone(),
            agents: daemon.agents.clone(),
            agent_knowledge: daemon.agent_knowledge.clone(),
            learning_queue: daemon.learning_queue.clone(),
            control_token: token,
        },
        ryuzi_core::serve::ServeOpts {
            addr: std::net::Ipv4Addr::LOCALHOST.into(),
            port: control_port,
            tls: None,
        },
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return fail_after_start(&dir, pid, started_at, version, &daemon, &e.to_string()).await
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
            port: Some(bound),
            // engine-daemon always binds loopback-only (see the `ServeOpts`
            // above) — no TLS material, so plain `http` and no fingerprint.
            scheme: Some("http".to_string()),
            host: Some(std::net::Ipv4Addr::LOCALHOST.to_string()),
            fingerprint: None,
        },
    );
    println!("engine-daemon: running on 127.0.0.1:{bound}");

    // Signal-driven shutdown (mirror daemon_cmd's shape, without the updater).
    // Nothing is spawned here, so `daemon`/`dir` can be used directly instead
    // of cloning them for a separate task.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    daemon.stop().await;
    clear_status(&dir);
    0
}

fn fail(
    dir: &std::path::Path,
    pid: i32,
    started_at: i64,
    version: Option<String>,
    msg: &str,
) -> i32 {
    let _ = write_status(
        dir,
        &DaemonStatusFile {
            pid,
            state: DaemonFileState::Error,
            started_at,
            last_error: Some(msg.to_string()),
            version,
            port: None,
            scheme: None,
            host: None,
            fingerprint: None,
        },
    );
    eprintln!("engine-daemon: failed to start: {msg}");
    1
}

/// Same as [`fail`], but for failures that occur AFTER `daemon.start()`
/// already succeeded (token write, control-API bind) — stops the daemon
/// first so no gateway is left running behind an Error status. Mirrors
/// `ryuzi-runner`'s `daemon_cmd::run_daemon` control-API failure branch.
async fn fail_after_start(
    dir: &std::path::Path,
    pid: i32,
    started_at: i64,
    version: Option<String>,
    daemon: &Daemon,
    msg: &str,
) -> i32 {
    daemon.stop().await;
    fail(dir, pid, started_at, version, msg)
}
