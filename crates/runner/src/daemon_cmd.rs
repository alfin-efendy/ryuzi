//! `__daemon` [`--canary`]: the hidden background-process entry points.
//! Spawned by the self-updater's `ProdApplierHost::spawn_canary` as
//! `[canary_path, "__daemon", "--canary"]` and respawned after a swap as
//! `[install_path, "__daemon"]` — never invoked directly by a user, and
//! deliberately absent from `--help`. The user-facing foreground entry
//! point is `ryuzi start` (same code path, see `dispatch.rs`).
//!
//! Owns the daemon process lifecycle: timed connect, reentrancy-guarded
//! shutdown on SIGTERM/SIGINT, the canary probe/promote flow, and the
//! production `UpdateManager` / apply+canary host wiring.

use anyhow::Context;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ryuzi_core::daemon::{build_daemon, BuildDaemonOpts, Daemon};
use ryuzi_core::daemon_status::{
    clear_status, send_sigterm, write_status, DaemonFileState, DaemonStatusFile,
};
use ryuzi_core::settings::SettingsStore;
use ryuzi_core::update::{
    apply_update, canary_target_version, canary_timeout_ms, clear_handoff, handle_apply_outcome,
    read_handoff, run_canary_with, stage_canary, write_handoff, ApplierCfg, ApplierHost, ApplyHook,
    ApplyInfo, CanaryCfg, CanaryHost, CanaryOutcome, Handoff, NotifyTarget, StageOpts, StageResult,
    TarStageHost, UpdateManager, UpdateManagerDeps, UreqHttp,
};

use crate::dispatch::Deps;

/// How long the daemon gets to build and start before the process gives up
/// and exits with a "timed out connecting" error.
const CONNECT_TIMEOUT_MS: u64 = 30_000;

/// Default localhost control-API port; overridden by the `control_port`
/// setting. `ryuzi_core::serve::serve` falls back to an ephemeral port if
/// this one is already busy — see its doc.
pub(crate) const DEFAULT_CONTROL_PORT: u16 = 4483;

pub fn cmd_daemon(args: &[String], deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if is_canary(args) {
        rt.block_on(run_canary(deps))
    } else {
        rt.block_on(run_daemon(deps))
    }
}

/// `--canary` positional-flag detection for `__daemon`'s argv.
pub(crate) fn is_canary(args: &[String]) -> bool {
    args.iter().any(|a| a == "--canary")
}

/// Race an arbitrary future against a `ms`-millisecond deadline. Generic
/// over the future's success type (rather than tied to `Daemon`) so it's
/// unit-testable in isolation; the production call site races
/// `build_daemon(...).and_then(Daemon::start)`.
pub(crate) async fn start_with_timeout<T, F>(fut: F, ms: u64) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    match tokio::time::timeout(Duration::from_millis(ms), fut).await {
        Ok(result) => result,
        Err(_elapsed) => anyhow::bail!("timed out connecting after {ms}ms"),
    }
}

/// Reentrancy-guarded shutdown: at most once, await `stop` (best-effort;
/// any error is swallowed so the shutdown always completes), clear `dir`'s
/// status file, then call `exit(0)`. Generic over `stop`/`exit` so it's
/// unit-testable without a real `Daemon` or a real `std::process::exit`.
// Non-test callers are the unix-only signal handlers; on Windows only the
// unit tests reach this, so the lib build sees it as dead.
#[cfg_attr(not(unix), allow(dead_code))]
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

/// Factored out of `run_daemon` so `run_canary` can build an identical
/// `BuildDaemonOpts` (same telemetry/gateway wiring) for its own
/// `build_daemon` call.
fn daemon_opts(deps: &Deps) -> BuildDaemonOpts {
    BuildDaemonOpts {
        db_path: deps.db_path.clone(),
        telemetry: None,
        // `factory_entries()` is gated INSIDE `ryuzi-core` on ITS OWN
        // `discord` feature (see `gateway::discord::mod`'s doc on why the
        // gate can't live here: `#[cfg(feature = "discord")]` in THIS crate
        // would check a feature `ryuzi-runner` doesn't declare, since its
        // `Cargo.toml` requests `ryuzi-core`'s `discord` feature directly
        // rather than exposing its own toggle). Empty under
        // `not(feature = "discord")`; populated for every real `ryuzi-runner`
        // build (its `Cargo.toml` always requests `ryuzi-core/discord`).
        extra_gateway_factories: ryuzi_core::gateway::discord::factory_entries(),
        harness_factory: None,
    }
}

/// The bound control-API port plus the scheme/host/fingerprint metadata
/// [`start_control_api`]'s two callers (`run_daemon`, the canary's
/// `promote()`) write into `daemon.json`'s `Running` status — see
/// [`ryuzi_core::daemon_status::DaemonStatusFile`]. Kept as one struct
/// (rather than a 4-tuple) so both call sites read the same field names.
struct ControlApiBinding {
    port: u16,
    scheme: String,
    host: String,
    fingerprint: Option<String>,
}

/// Bring up the control API for a started daemon: resolve the bearer token
/// (reused across same-port restarts, or freshly generated — see
/// [`ryuzi_core::control_token::write_token`]), read `control_port` (default
/// [`DEFAULT_CONTROL_PORT`]) and `listen_addr` (schema default loopback —
/// see the `listen_addr` `ConfigField`), resolve the bind IP / TLS material /
/// scheme / fingerprint via [`ryuzi_core::tls::resolve_bind`] — which
/// refuses (`Err`) a non-loopback address whose TLS material can't be built,
/// so this never silently serves plaintext on a public interface — and
/// serve. Returns the bound port plus scheme/host/fingerprint. Shared by
/// `run_daemon` and the canary's `promote()` so the two entry points cannot
/// drift.
async fn start_control_api(dir: &Path, daemon: &Daemon) -> anyhow::Result<ControlApiBinding> {
    let token = ryuzi_core::control_token::write_token(dir)?;
    let settings = SettingsStore::new(daemon.store.clone());
    let control_port: u16 = settings
        .get("control_port")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CONTROL_PORT);
    let listen_addr = settings
        .get("listen_addr")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| std::net::Ipv4Addr::LOCALHOST.to_string());

    let (addr, tls, scheme, fingerprint) = ryuzi_core::tls::resolve_bind(&listen_addr, dir)
        .context("failed to build TLS material for non-loopback bind")?;

    let state = ryuzi_core::serve::ApiState {
        cp: daemon.cp.clone(),
        router_server: daemon.router_server.clone(),
        control_token: token,
    };
    let opts = ryuzi_core::serve::ServeOpts {
        addr,
        port: control_port,
        tls,
    };
    let port = ryuzi_core::serve::serve(state, opts).await?;
    Ok(ControlApiBinding {
        port,
        scheme: scheme.to_string(),
        host: addr.to_string(),
        fingerprint,
    })
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

    // Real mutual exclusion (daemon.json's pid is only advisory — see its
    // doc). Acquired before any status is written so a second `__daemon`
    // invocation in the same state dir fails fast instead of clobbering the
    // first one's "connecting" file.
    let _lock = match ryuzi_core::daemon_lock::DaemonLock::acquire(&dir) {
        Ok(l) => l,
        Err(e) => {
            (deps.err)(&format!("daemon: {e}"));
            return 1;
        }
    };

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

    let opts = daemon_opts(deps);

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
                    port: None,
                    scheme: None,
                    host: None,
                    fingerprint: None,
                },
            );
            (deps.err)(&format!("daemon: failed to start: {e}"));
            return 1;
        }
    };

    // Control API: bearer token, then the bound port, written into daemon.json
    // BEFORE the daemon is reported running so clients that poll the status
    // file never observe a "running" daemon with no reachable control API.
    let bound = match start_control_api(&dir, &daemon).await {
        Ok(p) => p,
        Err(e) => {
            // The daemon already started (gateways claimed, reconcile() may
            // have fired) — stop it before reporting Error so a failed
            // control API doesn't leave an orphaned daemon process with no
            // status reflecting the failure.
            daemon.stop().await;
            let _ = write_status(
                &dir,
                &DaemonStatusFile {
                    pid,
                    state: DaemonFileState::Error,
                    started_at,
                    last_error: Some(e.to_string()),
                    version,
                    port: None,
                    scheme: None,
                    host: None,
                    fingerprint: None,
                },
            );
            (deps.err)(&format!("daemon: control api failed to bind: {e}"));
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
            port: Some(bound.port),
            scheme: Some(bound.scheme),
            host: Some(bound.host),
            fingerprint: bound.fingerprint,
        },
    );
    (deps.out)("daemon: running");

    let daemon = Arc::new(daemon);
    let updater = build_updater(Arc::clone(&daemon), dir.clone());
    updater.start();

    let catalog_mgr = ryuzi_core::plugins::remote_catalog::RemoteCatalogManager::new(
        daemon.store.clone(),
        SettingsStore::new(daemon.store.clone()),
        daemon.cp.clone(),
        Arc::new(ryuzi_core::plugins::remote_catalog::ReqwestCatalogHttp::new()),
    );
    catalog_mgr.start();

    // Signal handlers are deliberately installed only AFTER connect succeeds: a signal during
    // the connect window falls back to default kill; the stale "connecting" file is benign — derive_state
    // treats a dead pid as stopped.
    install_signal_handlers(dir, Arc::clone(&daemon), Some(updater));

    // Block forever: the process only exits via a signal handler calling
    // `std::process::exit` from within `shutdown_once`.
    std::future::pending::<()>().await;
    unreachable!("shutdown_once exits the process before this future can resolve")
}

/// `build_daemon` then `Daemon::start` — the "connecting" phase. Both steps
/// are async, so both are raced against the single 30s deadline together.
///
/// Review note: `Daemon::start` already rolls back any gateway it managed to
/// start before hitting the one that failed, and aborts the router/fan-out
/// handles (see its doc). The `daemon.stop()` below is belt-and-braces on
/// top of that: it's a safe no-op in the rollback case (which already marks
/// the daemon stopped) and still flushes telemetry / covers any future
/// failure path that reaches here without having rolled back itself.
async fn build_and_start(opts: BuildDaemonOpts) -> anyhow::Result<Daemon> {
    let daemon = build_daemon(opts).await?;
    // Real daemon startup: run the one-time install-ledger backfill +
    // crash-leftover sweep here — NOT inside `build_daemon`, which
    // `ryuzi-core`'s own daemon unit tests call (and which don't set a
    // hermetic config root), so wiring maintenance there would make those
    // tests touch/delete the developer's real `$HOME`. See
    // `ControlPlane::run_startup_maintenance`.
    daemon.cp.run_startup_maintenance().await;
    if let Err(e) = daemon.start().await {
        daemon.stop().await;
        return Err(e);
    }
    Ok(daemon)
}

/// Builds the production `UpdateManager`: real HTTP, real settings, and —
/// only on platforms `detect_platform` recognizes — a real self-apply hook
/// (`ProdApplyHook`). Unsupported platforms get `apply_update: None`, so
/// `UpdateManager::tick` falls back to its notify-only path. Since 0.7.0,
/// update assets use the platform-tag naming scheme (target triple minus
/// `-unknown`); the updaters in <= 0.6.0 binaries match nothing and
/// silently no-op — see `ryuzi_core::update::asset`.
fn build_updater(daemon: Arc<Daemon>, dir: PathBuf) -> Arc<UpdateManager> {
    let exec_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let apply: Option<Arc<dyn ApplyHook>> = ryuzi_core::update::detect_platform().map(|platform| {
        Arc::new(ProdApplyHook {
            daemon: Arc::clone(&daemon),
            dir: dir.clone(),
            platform,
        }) as Arc<dyn ApplyHook>
    });
    UpdateManager::new(UpdateManagerDeps {
        cp: daemon.cp.clone() as Arc<dyn NotifyTarget>,
        settings: SettingsStore::new(daemon.store.clone()),
        version: crate::meta::version().to_string(),
        exec_path,
        compiled: !cfg!(debug_assertions), // dev builds never self-apply
        home: dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
        docker_env: std::path::Path::new("/.dockerenv").exists(),
        http: Arc::new(UreqHttp),
        log: Some(Box::new(|m| println!("{m}"))),
        apply_update: apply,
    })
}

/// Production `ApplyHook`: stages a canary binary, spawns it,
/// drains/swaps/hands over (or rolls back) via [`apply_update`], then
/// respawns or exits per [`handle_apply_outcome`].
struct ProdApplyHook {
    daemon: Arc<Daemon>,
    dir: PathBuf,
    platform: ryuzi_core::update::Platform,
}

#[async_trait::async_trait]
impl ApplyHook for ProdApplyHook {
    async fn apply(&self, info: ApplyInfo) {
        let Ok(install_path) = std::env::current_exe() else {
            println!("update: cannot resolve current executable; skipping update");
            return;
        };
        let Ok(tmp) = tempfile::tempdir() else {
            println!("update: cannot create staging tempdir; skipping update");
            return;
        };
        let settings = SettingsStore::new(self.daemon.store.clone());
        let get_ms = |v: Option<String>, d: u64| v.and_then(|s| s.parse().ok()).unwrap_or(d);
        let cfg = ApplierCfg {
            version: info.version.clone(),
            drain_timeout_ms: get_ms(
                settings
                    .get("auto_update_drain_timeout_ms")
                    .await
                    .ok()
                    .flatten(),
                300_000,
            ),
            canary_timeout_ms: get_ms(
                settings
                    .get("auto_update_canary_timeout_ms")
                    .await
                    .ok()
                    .flatten(),
                60_000,
            ),
        };
        let host = ProdApplierHost {
            daemon: Arc::clone(&self.daemon),
            dir: self.dir.clone(),
            install_path: install_path.clone(),
            info,
            platform: self.platform,
            tmp_dir: tmp.path().to_path_buf(),
            canary_wait_ms: cfg.drain_timeout_ms + cfg.canary_timeout_ms,
        };
        let result = apply_update(&cfg, &host).await;
        // Explicit early drop: `handle_apply_outcome` below calls
        // `std::process::exit` for the Promoted/RolledBack outcomes, and
        // `process::exit` runs no destructors, so `tmp`'s Drop (staging dir
        // removal) would never fire if left to run at end-of-scope. Drop it
        // here instead, so the staging dir is always cleaned up before the
        // process can exit.
        drop(tmp);
        match result {
            Ok(outcome) => handle_apply_outcome(
                outcome,
                || {
                    let cmd = vec![
                        install_path.to_string_lossy().into_owned(),
                        "__daemon".to_string(),
                    ];
                    let _ = spawn_detached(&cmd, &[], &self.dir.join("daemon.log"));
                },
                |c| std::process::exit(c),
                |m| println!("{m}"),
            ),
            Err(e) => {
                let msg =
                    format!("update: apply failed mid-swap: {e} (update.json left for inspection)");
                println!("{msg}");
                write_update_failure_status(&self.dir, &msg);
                std::process::exit(1);
            }
        }
    }
}

/// Production `ApplierHost` — real staging (HTTP + tar), real spawn/rename,
/// real gateway drain/stop. Backs [`ProdApplyHook::apply`].
struct ProdApplierHost {
    daemon: Arc<Daemon>,
    dir: PathBuf,
    install_path: PathBuf,
    info: ApplyInfo,
    platform: ryuzi_core::update::Platform,
    tmp_dir: PathBuf,
    canary_wait_ms: u64,
}

#[async_trait::async_trait]
impl ApplierHost for ProdApplierHost {
    async fn stage(&self) -> StageResult {
        let opts = StageOpts {
            repo: self.info.repo.clone(),
            tag: self.info.tag.clone(),
            version: self.info.version.clone(),
            install_path: self.install_path.clone(),
        };
        let (platform, tmp) = (self.platform, self.tmp_dir.clone());
        tokio::task::spawn_blocking(move || {
            stage_canary(&opts, platform, &tmp, &UreqHttp, &TarStageHost)
        })
        .await
        .unwrap_or_else(|e| StageResult {
            ok: false,
            canary_path: None,
            error: Some(e.to_string()),
        })
    }
    fn spawn_canary(&self, canary_path: &Path) -> anyhow::Result<i32> {
        let cmd = vec![
            canary_path.to_string_lossy().into_owned(),
            "__daemon".to_string(),
            "--canary".to_string(),
        ];
        let env = canary_spawn_env(&self.info.version, self.canary_wait_ms);
        Ok(spawn_detached(&cmd, &env, &self.dir.join("daemon.log"))? as i32)
    }
    fn read_handoff(&self) -> Option<Handoff> {
        read_handoff(&self.dir)
    }
    fn write_handoff(&self, h: &Handoff) {
        let _ = write_handoff(&self.dir, h);
    }
    fn clear_handoff(&self) {
        clear_handoff(&self.dir);
    }
    async fn drain(&self, timeout_ms: u64) {
        self.daemon.cp.drain(timeout_ms).await;
    }
    fn backup(&self) -> anyhow::Result<()> {
        Ok(std::fs::rename(
            &self.install_path,
            bak_path(&self.install_path),
        )?)
    }
    fn swap(&self) -> anyhow::Result<()> {
        let canary = self
            .install_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".ryuzi.canary");
        Ok(std::fs::rename(canary, &self.install_path)?)
    }
    fn restore(&self) -> anyhow::Result<()> {
        Ok(std::fs::rename(
            bak_path(&self.install_path),
            &self.install_path,
        )?)
    }
    fn kill_canary(&self, pid: i32) {
        send_sigterm(pid);
    }
    async fn stop_gateways(&self) {
        self.daemon.stop().await;
    }
    fn now(&self) -> i64 {
        ryuzi_core::paths::now_ms()
    }
    async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    fn log(&self, m: &str) {
        println!("{m}");
    }
}

/// The rollback backup path for the live binary: `{install_path}.bak`.
fn bak_path(install_path: &Path) -> PathBuf {
    let mut s = install_path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Env for the spawned canary: the version it must claim, and a promote-wait
/// that outlives the applier's ENTIRE post-health window (drain + watchdog).
/// A shorter wait would let a long (>60s) drain outlive the canary's promote
/// window, guaranteeing a "promote timeout" → disruptive swap→rollback cycle.
pub(crate) fn canary_spawn_env(version: &str, wait_ms: u64) -> Vec<(String, String)> {
    vec![
        ("RYUZI_CANARY_TARGET".to_string(), version.to_string()),
        ("RYUZI_CANARY_TIMEOUT_MS".to_string(), wait_ms.to_string()),
    ]
}

/// A failed apply AFTER the drain latch was set must not leave a zombie
/// "Running" daemon that rejects every turn — record an Error status so the
/// failure is visible instead of a silently wedged daemon.
fn write_update_failure_status(dir: &Path, message: &str) {
    let _ = write_status(
        dir,
        &DaemonStatusFile {
            pid: std::process::id() as i32,
            state: DaemonFileState::Error,
            started_at: ryuzi_core::paths::now_ms(),
            last_error: Some(message.to_string()),
            version: Some(crate::meta::version().to_string()),
            port: None,
            scheme: None,
            host: None,
            fingerprint: None,
        },
    );
}

/// Spawn `cmd` detached: stdin null, stdout/stderr appended to `log_path`,
/// extra `env` vars applied, and on unix in its own process group so it
/// survives the spawning process exiting. Shared by the TUI's daemon-start
/// and the self-updater's fresh-daemon/canary respawns.
pub(crate) fn spawn_detached(
    cmd: &[String],
    env: &[(String, String)],
    log_path: &Path,
) -> std::io::Result<u32> {
    use std::fs::File;
    use std::process::{Command, Stdio};

    let stdout = File::options().append(true).create(true).open(log_path)?;
    let stderr = File::options().append(true).create(true).open(log_path)?;
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    Ok(child.id())
}

/// The canary entry point: probe this binary's DB on the target version,
/// wait for the applier's `promote` handoff signal, then either become the
/// live daemon (promoted) or exit 1 (failed) so the applier's watchdog
/// rolls back.
async fn run_canary(deps: &mut Deps) -> u8 {
    let dir: PathBuf = deps
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let version = crate::meta::version().to_string();
    let cfg = CanaryCfg {
        target_version: canary_target_version(&version, std::env::var("RYUZI_CANARY_TARGET").ok()),
        timeout_ms: canary_timeout_ms(std::env::var("RYUZI_CANARY_TIMEOUT_MS").ok()),
        pid: std::process::id() as i32,
        version: version.clone(),
    };
    let host = ProdCanaryHost {
        dir: dir.clone(),
        opts: std::sync::Mutex::new(Some(daemon_opts(deps))),
        daemon: tokio::sync::Mutex::new(None),
        version,
    };
    if run_canary_with(&cfg, &host).await == CanaryOutcome::Failed {
        return 1;
    }
    // Promoted → become the live daemon: reuse run_daemon's signal handling
    // and block. NOTE: the promoted canary runs WITHOUT its own
    // UpdateManager until the next restart.
    let daemon = host
        .daemon
        .lock()
        .await
        .take()
        .expect("promote() built the daemon");

    // The old applier process is still alive (and still holding
    // `DaemonLock`) at the instant `promote()` returns — the
    // applier/canary handoff, not the lock, is what excludes the two during
    // the swap (see `ProdCanaryHost::promote`'s doc). Acquiring the lock
    // inline here would deadlock against that still-live process, so poll
    // for it in the background instead: once the old process exits and its
    // flock is released, forget the guard so this process holds the lock
    // for the rest of its lifetime, matching `run_daemon`'s singleton-lock
    // invariant. A wedged old process just means the lock is never taken —
    // logged once, but not fatal to the now-promoted canary.
    spawn_lock_acquire_retry(dir.clone());

    install_signal_handlers(dir, Arc::new(daemon), None);
    std::future::pending::<()>().await;
    unreachable!("shutdown_once exits the process before this future can resolve")
}

/// How often [`spawn_lock_acquire_retry`] retries
/// [`ryuzi_core::daemon_lock::DaemonLock::acquire`].
const LOCK_RETRY_INTERVAL_MS: u64 = 500;

/// How long [`spawn_lock_acquire_retry`] retries before giving up and
/// logging a warning.
const LOCK_RETRY_WINDOW_MS: u64 = 120_000;

/// Background task: retry [`ryuzi_core::daemon_lock::DaemonLock::acquire`]
/// for `dir` every [`LOCK_RETRY_INTERVAL_MS`] until it succeeds or
/// [`LOCK_RETRY_WINDOW_MS`] elapses. On success the guard is
/// `std::mem::forget`-ten so the lock is held for the rest of the process
/// lifetime, exactly like [`run_daemon`]'s own lock. On timeout, logs one
/// warning and returns — the promoted canary keeps running either way.
fn spawn_lock_acquire_retry(dir: PathBuf) {
    tokio::spawn(async move {
        let mut waited_ms = 0u64;
        loop {
            match ryuzi_core::daemon_lock::DaemonLock::acquire(&dir) {
                Ok(lock) => {
                    std::mem::forget(lock);
                    return;
                }
                Err(_) if waited_ms < LOCK_RETRY_WINDOW_MS => {
                    tokio::time::sleep(Duration::from_millis(LOCK_RETRY_INTERVAL_MS)).await;
                    waited_ms += LOCK_RETRY_INTERVAL_MS;
                }
                Err(e) => {
                    println!(
                        "daemon: canary could not acquire the daemon lock within {LOCK_RETRY_WINDOW_MS}ms after promotion ({e}); the old process may be wedged"
                    );
                    return;
                }
            }
        }
    });
}

/// Production `CanaryHost` — opens a real `Daemon` (deferring `start()`
/// until `promote`, so gateway ports aren't claimed until the applier signals
/// go-ahead), writes real handoff files, sleeps for real.
struct ProdCanaryHost {
    dir: PathBuf,
    opts: std::sync::Mutex<Option<BuildDaemonOpts>>,
    daemon: tokio::sync::Mutex<Option<Daemon>>,
    version: String,
}

#[async_trait::async_trait]
impl CanaryHost for ProdCanaryHost {
    async fn open_db(&self) -> anyhow::Result<()> {
        let opts = self
            .opts
            .lock()
            .unwrap()
            .take()
            .expect("open_db called once");
        let daemon = build_daemon(opts).await?;
        // Real (canary) daemon startup — same one-time maintenance as
        // `build_and_start`, kept out of `build_daemon` for the same reason.
        daemon.cp.run_startup_maintenance().await;
        *self.daemon.lock().await = Some(daemon);
        Ok(())
    }
    // NOTE: does NOT acquire `DaemonLock` — the old applier process still
    // holds it at this point in the handoff (the applier/canary handshake,
    // not the flock, is what excludes the two during the swap). `run_canary`
    // acquires the lock itself, in the background, once `promote` returns —
    // see `spawn_lock_acquire_retry`.
    async fn promote(&self) -> anyhow::Result<()> {
        let guard = self.daemon.lock().await;
        let daemon = guard.as_ref().expect("open_db succeeded before promote");
        daemon.start().await?; // claims gateways + fires reconcile() for interrupted sessions

        // On failure here, just propagate `Err`: the canary flow's applier
        // (see `apply_update`/`ApplierHost`) handles rollback of a failed
        // promote, so no status write is needed on this path.
        let bound = start_control_api(&self.dir, daemon).await?;

        let _ = write_status(
            &self.dir,
            &DaemonStatusFile {
                pid: std::process::id() as i32,
                state: DaemonFileState::Running,
                started_at: ryuzi_core::paths::now_ms(),
                last_error: None,
                version: Some(self.version.clone()),
                port: Some(bound.port),
                scheme: Some(bound.scheme),
                host: Some(bound.host),
                fingerprint: bound.fingerprint,
            },
        );
        Ok(())
    }
    fn write_handoff(&self, h: &Handoff) {
        let _ = ryuzi_core::update::write_handoff(&self.dir, h);
    }
    fn read_handoff(&self) -> Option<Handoff> {
        ryuzi_core::update::read_handoff(&self.dir)
    }
    fn now(&self) -> i64 {
        ryuzi_core::paths::now_ms()
    }
    async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
}

/// Installs SIGTERM/SIGINT handlers that drive [`shutdown_once`]: stop the
/// updater first (if any — so no update tick can race the teardown),
/// then best-effort `daemon.stop()`, clear `dir`'s status file,
/// `std::process::exit(0)`. Both signals share one reentrancy guard and one
/// `Daemon`/`UpdateManager` handle so whichever fires first wins and the
/// other is a no-op.
///
/// Unix-only: `tokio::signal::unix` does not exist on Windows, and an
/// unconditional `use` used to break `cargo check --workspace` on Windows
/// dev machines. The non-unix variant below drives the same teardown from
/// `ctrl_c` instead.
#[cfg(unix)]
fn install_signal_handlers(dir: PathBuf, daemon: Arc<Daemon>, updater: Option<Arc<UpdateManager>>) {
    use tokio::signal::unix::{signal, SignalKind};

    let stopping = Arc::new(AtomicBool::new(false));

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    {
        let daemon = Arc::clone(&daemon);
        let updater = updater.clone();
        let stopping = Arc::clone(&stopping);
        let dir = dir.clone();
        tokio::spawn(async move {
            sigterm.recv().await;
            shutdown_once(
                &dir,
                &stopping,
                async {
                    if let Some(u) = &updater {
                        u.stop();
                    }
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
                if let Some(u) = &updater {
                    u.stop();
                }
                daemon.stop().await;
                Ok(())
            },
            |c| std::process::exit(c),
        )
        .await;
    });
}

/// Non-unix (Windows): there is no SIGTERM; Ctrl-C / console-close both
/// surface through `tokio::signal::ctrl_c`, driving the same
/// [`shutdown_once`] teardown so a native Windows daemon still cleans up.
#[cfg(not(unix))]
fn install_signal_handlers(dir: PathBuf, daemon: Arc<Daemon>, updater: Option<Arc<UpdateManager>>) {
    let stopping = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_once(
            &dir,
            &stopping,
            async {
                if let Some(u) = &updater {
                    u.stop();
                }
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

    // ---------- DEFAULT_CONTROL_PORT ----------

    #[test]
    fn control_port_default_is_4483() {
        // Documented default; EngineClient and docs reference it.
        assert_eq!(super::DEFAULT_CONTROL_PORT, 4483);
    }

    // ---------- is_canary ----------

    #[test]
    fn canary_flag_is_detected_positionally() {
        assert!(super::is_canary(&["--canary".to_string()]));
        assert!(!super::is_canary(&[]));
        assert!(!super::is_canary(&["--other".to_string()]));
    }

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
                port: None,
                scheme: None,
                host: None,
                fingerprint: None,
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

    // ---------- bak_path ----------

    #[test]
    fn bak_path_appends_dot_bak_to_the_full_path() {
        assert_eq!(
            bak_path(Path::new("/home/me/.local/bin/ryuzi")),
            PathBuf::from("/home/me/.local/bin/ryuzi.bak")
        );
        assert_eq!(bak_path(Path::new("ryuzi")), PathBuf::from("ryuzi.bak"));
    }

    // ---------- canary_spawn_env ----------

    #[test]
    fn canary_spawn_env_carries_target_and_summed_timeout() {
        let env = canary_spawn_env("0.4.0", 360_000);
        assert!(env.contains(&("RYUZI_CANARY_TARGET".to_string(), "0.4.0".to_string())));
        assert!(env.contains(&("RYUZI_CANARY_TIMEOUT_MS".to_string(), "360000".to_string())));
    }

    // ---------- write_update_failure_status ----------

    #[test]
    fn update_failure_status_is_an_error_with_the_message() {
        let dir = tempfile::tempdir().unwrap();
        write_update_failure_status(dir.path(), "apply failed mid-swap: boom");
        let s = ryuzi_core::daemon_status::read_status(dir.path()).unwrap();
        assert_eq!(s.state, ryuzi_core::daemon_status::DaemonFileState::Error);
        assert_eq!(s.last_error.as_deref(), Some("apply failed mid-swap: boom"));
        assert_eq!(s.pid, std::process::id() as i32);
    }

    // ---------- backup/swap/restore fs semantics ----------

    #[test]
    fn backup_swap_restore_round_trip_lands_content_where_expected() {
        let dir = tempfile::tempdir().unwrap();
        let install_path = dir.path().join("ryuzi");
        let canary_path = dir.path().join(".ryuzi.canary");
        std::fs::write(&install_path, b"old-content").unwrap();
        std::fs::write(&canary_path, b"new-content").unwrap();

        // backup: install_path -> install_path.bak
        std::fs::rename(&install_path, bak_path(&install_path)).unwrap();
        assert!(!install_path.exists());
        assert_eq!(
            std::fs::read(bak_path(&install_path)).unwrap(),
            b"old-content"
        );

        // swap: .ryuzi.canary -> install_path
        std::fs::rename(&canary_path, &install_path).unwrap();
        assert!(!canary_path.exists());
        assert_eq!(std::fs::read(&install_path).unwrap(), b"new-content");

        // restore: install_path.bak -> install_path (undoes the swap)
        std::fs::rename(bak_path(&install_path), &install_path).unwrap();
        assert_eq!(std::fs::read(&install_path).unwrap(), b"old-content");
        assert!(!bak_path(&install_path).exists());
    }
}
