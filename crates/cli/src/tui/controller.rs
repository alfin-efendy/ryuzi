//! `AppController` — the TUI's single behavior facade.
//!
//! Owns the entire behavior surface behind the ratatui TUI (Tasks 5-7):
//! validated settings via `SettingsStore`, the provider catalog, daemon.json
//! based start/stop/status, `daemon.log` tailing, and session listing.
//! Spawn/kill/detect are injectable seams (`ControllerDeps`) so daemon
//! lifecycle and environment detection can be faked in tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ryuzi_core::daemon_status::{
    clear_status, derive_state, is_alive, read_status, send_sigterm, write_status, DaemonFileState,
    DaemonState, DaemonStatusFile,
};
use ryuzi_core::settings::{
    csv, find_field, is_secret, ConfigField, GatewayDescriptor, RuntimeDescriptor, SettingsStore,
    CATALOG, GLOBAL_FIELDS,
};
use ryuzi_core::Store;

use crate::detect::Detected;

/// A session row projected for the TUI's session list.
pub struct SessionRow {
    pub session_pk: String,
    pub project_id: String,
    pub status: String,
    pub title: Option<String>,
    pub started_by: Option<String>,
}

/// Spawns the detached daemon process: `(cmd, log_path) -> pid`. `None` in
/// `ControllerDeps` means "really spawn one" via `crate::daemon_cmd::spawn_detached`.
pub type SpawnDaemon = Box<dyn Fn(&[String], &Path) -> std::io::Result<u32> + Send + Sync>;
/// Sends SIGTERM to a pid. `None` means "really `kill(2)`" via
/// `ryuzi_core::daemon_status::send_sigterm`.
pub type KillFn = Box<dyn Fn(i32) + Send + Sync>;
/// A sync environment-detector function pointer (`detect_git`/`detect_claude`).
pub type DetectFn = fn() -> Detected;

pub struct ControllerDeps {
    pub store: Arc<Store>,
    pub data_dir: PathBuf,
    pub detect_git: DetectFn,
    pub detect_claude: DetectFn,
    pub spawn_daemon: Option<SpawnDaemon>,
    pub kill_daemon: Option<KillFn>,
}

pub struct AppController {
    settings: SettingsStore,
    pub(crate) deps: ControllerDeps,
}

impl AppController {
    pub fn new(deps: ControllerDeps) -> Self {
        let settings = SettingsStore::new(deps.store.clone());
        Self { settings, deps }
    }

    // ---- settings surface ----

    /// The persisted or schema-default value; any store error is treated as
    /// "unset" — `get` never fails, so render code needs no error path.
    pub async fn get(&self, key: &str) -> Option<String> {
        self.settings.get(key).await.ok().flatten()
    }

    /// Validate then persist a setting; validation errors propagate.
    pub async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.settings.set(key, value).await
    }

    pub fn is_secret(&self, key: &str) -> bool {
        is_secret(key)
    }

    pub async fn missing_required(&self) -> Vec<&'static str> {
        self.settings.missing_required().await.unwrap_or_default()
    }

    pub async fn is_configured(&self) -> bool {
        self.settings.is_configured().await.unwrap_or(false)
    }

    pub fn field(&self, key: &str) -> Option<&'static ConfigField> {
        find_field(key)
    }

    /// Global settings fields, excluding internal `control` ones (e.g.
    /// `enabled_gateways`) that are managed by the provider picker.
    pub fn general_fields(&self) -> Vec<&'static ConfigField> {
        GLOBAL_FIELDS.iter().filter(|f| !f.control).collect()
    }

    pub fn gateway_descriptors(&self) -> &'static [GatewayDescriptor] {
        CATALOG.gateways
    }

    pub fn runtime_descriptors(&self) -> &'static [RuntimeDescriptor] {
        CATALOG.runtimes
    }

    pub fn gateway_fields(&self, id: &str) -> &'static [ConfigField] {
        CATALOG.gateway(id).map(|g| g.fields).unwrap_or(&[])
    }

    pub fn runtime_fields(&self, id: &str) -> &'static [ConfigField] {
        CATALOG.runtime(id).map(|r| r.fields).unwrap_or(&[])
    }

    pub async fn enabled_gateways(&self) -> Vec<String> {
        csv(self.get("enabled_gateways").await.as_deref())
    }

    pub async fn enabled_runtimes(&self) -> Vec<String> {
        csv(self.get("enabled_runtimes").await.as_deref())
    }

    pub async fn default_runtime(&self) -> String {
        self.get("default_runtime").await.unwrap_or_default()
    }

    /// Empty slice stores `""` (join of nothing) — "none enabled" is an
    /// empty string, not a missing key.
    pub async fn set_enabled_gateways(&self, ids: &[String]) -> anyhow::Result<()> {
        self.settings.set("enabled_gateways", &ids.join(",")).await
    }

    /// Empty slice stores `""` (join of nothing) — "none enabled" is an
    /// empty string, not a missing key.
    pub async fn set_enabled_runtimes(&self, ids: &[String]) -> anyhow::Result<()> {
        self.settings.set("enabled_runtimes", &ids.join(",")).await
    }

    pub async fn set_default_runtime(&self, id: &str) -> anyhow::Result<()> {
        self.settings.set("default_runtime", id).await
    }

    pub async fn required_missing_fields(&self) -> Vec<&'static ConfigField> {
        self.missing_required()
            .await
            .into_iter()
            .filter_map(find_field)
            .collect()
    }

    /// `claude-code` -> the injected `detect_claude`; `native` is always
    /// available (in-process, no external binary); any other id -> not found.
    pub fn detect_runtime(&self, id: &str) -> Detected {
        match id {
            "claude-code" => (self.deps.detect_claude)(),
            "native" => Detected {
                found: true,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
            _ => Detected {
                found: false,
                version: None,
            },
        }
    }

    pub fn check_env(&self) -> (Detected, Detected) {
        ((self.deps.detect_git)(), (self.deps.detect_claude)())
    }

    // ---- daemon surface ----

    /// Reads `daemon.json` and derives the running/starting/error state.
    /// Side effect: a stale, non-error status file whose pid is dead is
    /// cleared — error files persist so the message stays visible.
    pub fn daemon(&self) -> DaemonState {
        let s = read_status(&self.deps.data_dir);
        let st = derive_state(s.as_ref(), &is_alive);
        if let Some(ref file) = s {
            if !st.running && !st.starting && file.state != DaemonFileState::Error {
                clear_status(&self.deps.data_dir);
            }
        }
        st
    }

    /// Non-empty lines of `daemon.log`, last 200.
    pub fn logs(&self) -> Vec<String> {
        let path = self.deps.data_dir.join("daemon.log");
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        let start = lines.len().saturating_sub(200);
        lines[start..].to_vec()
    }

    pub async fn sessions(&self) -> Vec<SessionRow> {
        self.deps
            .store
            .list_sessions(None)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|s| SessionRow {
                session_pk: s.session_pk,
                // Chat-first sessions (Phase 2) have no bound project; the
                // TUI dashboard is project-centric today, so render "-".
                project_id: s.project_id.unwrap_or_else(|| "-".to_string()),
                status: s.status.as_str().to_string(),
                title: s.title,
                started_by: s.started_by,
            })
            .collect()
    }

    /// No-ops when already running/starting. Blocked (missing required
    /// settings) writes a synthetic error status without spawning. An empty
    /// `enabled_gateways` is fine — the daemon is the always-on engine host
    /// (control API, sessions, scheduler) regardless of whether any gateway
    /// is enabled. Otherwise clears any stale status and spawns the daemon
    /// detached, logging to `{data_dir}/daemon.log`.
    pub async fn start_daemon(&self) -> anyhow::Result<()> {
        let cur = self.daemon();
        if cur.running || cur.starting {
            return Ok(());
        }
        let missing = self.missing_required().await;
        if !missing.is_empty() {
            let why = format!("missing settings: {}", missing.join(", "));
            let _ = write_status(
                &self.deps.data_dir,
                &DaemonStatusFile {
                    pid: -1,
                    state: DaemonFileState::Error,
                    started_at: ryuzi_core::paths::now_ms(),
                    last_error: Some(why),
                    version: None,
                    port: None,
                },
            );
            return Ok(());
        }
        clear_status(&self.deps.data_dir);
        let exe = std::env::current_exe()?;
        let cmd = vec![exe.to_string_lossy().into_owned(), "__daemon".to_string()];
        let log_path = self.deps.data_dir.join("daemon.log");
        match &self.deps.spawn_daemon {
            Some(f) => {
                f(&cmd, &log_path)?;
            }
            None => {
                crate::daemon_cmd::spawn_detached(&cmd, &[], &log_path)?;
            }
        }
        Ok(())
    }

    /// Kills the daemon only when a status file exists, its pid is positive,
    /// and it's alive; always best-effort (never errors).
    pub fn stop_daemon(&self) {
        if let Some(s) = read_status(&self.deps.data_dir) {
            if s.pid > 0 && is_alive(s.pid) {
                match &self.deps.kill_daemon {
                    Some(f) => f(s.pid),
                    None => send_sigterm(s.pid),
                }
            }
        }
    }

    /// Running -> stop; otherwise -> start (which itself no-ops while
    /// starting, so a repeated toggle can't spawn a second daemon).
    pub async fn toggle_daemon(&self) -> anyhow::Result<()> {
        if self.daemon().running {
            self.stop_daemon();
            Ok(())
        } else {
            self.start_daemon().await
        }
    }
}

/// Shared test fixture — reused by Tasks 5-7's view/app tests, not just this
/// module's.
#[cfg(test)]
pub(crate) async fn controller_in(dir: &Path) -> AppController {
    let store = Arc::new(
        ryuzi_core::Store::open(&dir.join("db.sqlite"))
            .await
            .unwrap(),
    );
    AppController::new(ControllerDeps {
        store,
        data_dir: dir.to_path_buf(),
        detect_git: || Detected {
            found: true,
            version: Some("2.45.0".into()),
        },
        detect_claude: || Detected {
            found: true,
            version: Some("2.1.0".into()),
        },
        spawn_daemon: None,
        kill_daemon: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn start_daemon_spawns_detached_daemon_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let spawns: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let mut c = controller_in(dir.path()).await;
        let log = spawns.clone();
        c.deps.spawn_daemon = Some(Box::new(move |cmd, _log_path| {
            log.lock().unwrap().push(cmd.to_vec());
            Ok(4242)
        }));
        for (k, v) in [
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
            ("workdir_root", "/repos"),
        ] {
            c.set(k, v).await.unwrap();
        }
        c.start_daemon().await.unwrap();
        let spawned = spawns.lock().unwrap();
        assert_eq!(spawned.len(), 1);
        assert_eq!(
            spawned[0][0],
            std::env::current_exe().unwrap().to_string_lossy()
        );
        assert_eq!(spawned[0].last().map(String::as_str), Some("__daemon"));
    }

    #[tokio::test]
    async fn start_daemon_records_error_without_spawning_when_missing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let spawns: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let mut c = controller_in(dir.path()).await;
        let log = spawns.clone();
        c.deps.spawn_daemon = Some(Box::new(move |cmd, _| {
            log.lock().unwrap().push(cmd.to_vec());
            Ok(1)
        }));
        c.start_daemon().await.unwrap(); // seeds enable discord, but discord.token unset
        assert!(spawns.lock().unwrap().is_empty());
        let err = c.daemon().last_error.unwrap();
        assert!(err.to_lowercase().contains("missing"), "{err}");
    }

    /// The daemon is the always-on engine host regardless of gateways: with
    /// `enabled_gateways` cleared to empty (previously refused with "no
    /// gateways enabled") and no required setting missing, `start_daemon`
    /// now proceeds to spawn.
    #[tokio::test]
    async fn start_daemon_spawns_even_with_no_gateways_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let spawns: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let mut c = controller_in(dir.path()).await;
        let log = spawns.clone();
        c.deps.spawn_daemon = Some(Box::new(move |cmd, _log_path| {
            log.lock().unwrap().push(cmd.to_vec());
            Ok(4242)
        }));
        c.set_enabled_gateways(&[]).await.unwrap();
        c.set("workdir_root", "/repos").await.unwrap();
        c.start_daemon().await.unwrap();
        assert_eq!(spawns.lock().unwrap().len(), 1);
        assert!(c.daemon().last_error.is_none());
    }

    #[tokio::test]
    async fn daemon_reflects_status_file_states() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        assert!(!c.daemon().running);
        let me = std::process::id() as i32;
        write_status(
            dir.path(),
            &DaemonStatusFile {
                pid: me,
                state: DaemonFileState::Connecting,
                started_at: 1,
                last_error: None,
                version: None,
                port: None,
            },
        )
        .unwrap();
        assert!(c.daemon().starting);
        write_status(
            dir.path(),
            &DaemonStatusFile {
                pid: me,
                state: DaemonFileState::Running,
                started_at: 1,
                last_error: None,
                version: None,
                port: None,
            },
        )
        .unwrap();
        assert!(c.daemon().running);
        write_status(
            dir.path(),
            &DaemonStatusFile {
                pid: me,
                state: DaemonFileState::Error,
                started_at: 1,
                last_error: Some("boom".into()),
                version: None,
                port: None,
            },
        )
        .unwrap();
        assert_eq!(c.daemon().last_error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn stop_daemon_sigterms_the_running_pid() {
        let dir = tempfile::tempdir().unwrap();
        let kills: Arc<Mutex<Vec<i32>>> = Arc::default();
        let mut c = controller_in(dir.path()).await;
        let log = kills.clone();
        c.deps.kill_daemon = Some(Box::new(move |pid| log.lock().unwrap().push(pid)));
        let me = std::process::id() as i32;
        write_status(
            dir.path(),
            &DaemonStatusFile {
                pid: me,
                state: DaemonFileState::Running,
                started_at: 1,
                last_error: None,
                version: None,
                port: None,
            },
        )
        .unwrap();
        c.stop_daemon();
        assert_eq!(*kills.lock().unwrap(), vec![me]);
    }

    #[tokio::test]
    async fn set_persists_and_invalid_value_errors() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        c.set("workdir_root", "/repos").await.unwrap();
        assert_eq!(c.get("workdir_root").await.as_deref(), Some("/repos"));
        assert!(c.set("max_concurrent_runs", "abc").await.is_err());
        assert!(c.set("default_perm_mode", "bogus").await.is_err());
    }

    #[tokio::test]
    async fn is_configured_progresses_as_required_fields_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        assert!(!c.is_configured().await);
        c.set("workdir_root", "/repos").await.unwrap();
        assert!(!c.is_configured().await);
        c.set("discord.token", "t").await.unwrap();
        c.set("discord.app_id", "a").await.unwrap();
        assert!(!c.is_configured().await);
        c.set("discord.guild_id", "g").await.unwrap();
        assert!(c.is_configured().await);
    }

    #[tokio::test]
    async fn setting_keys_contain_discord_token_and_is_secret() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let mut keys: Vec<&str> = c.general_fields().iter().map(|f| f.key).collect();
        for gw in c.gateway_descriptors() {
            keys.extend(c.gateway_fields(gw.id).iter().map(|f| f.key));
        }
        for rt in c.runtime_descriptors() {
            keys.extend(c.runtime_fields(rt.id).iter().map(|f| f.key));
        }
        assert!(keys.contains(&"discord.token"));
        assert!(c.is_secret("discord.token"));
        assert!(!c.is_secret("workdir_root"));
    }

    #[tokio::test]
    async fn check_env_uses_injected_detectors() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        let (git, claude) = c.check_env();
        assert!(git.found);
        assert_eq!(git.version.as_deref(), Some("2.45.0"));
        assert!(claude.found);
        assert_eq!(claude.version.as_deref(), Some("2.1.0"));
        assert_eq!(
            c.detect_runtime("claude-code").version.as_deref(),
            Some("2.1.0")
        );
        assert!(!c.detect_runtime("unknown-runtime").found);
    }
}
