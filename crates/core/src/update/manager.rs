//! `UpdateManager` — orchestrates the self-update lifecycle: on a timer (or
//! on demand via `tick`), checks GitHub Releases for a newer version, then
//! either self-applies (install.sh installs, in `auto` mode, when an
//! `ApplyHook` is wired up) or broadcasts a `Notice` to live sessions via a
//! `NotifyTarget` (`ControlPlane` in production).
//!
//! Port of `apps/cli/src/cli/update-manager.ts` (Spec 4 slice 5, task 8).

use crate::domain::{CoreEvent, Session, SessionStatus};
use crate::settings::SettingsStore;
use crate::update::check::{check_for_update, UpdateCheckResult, UpdateHttp};
use crate::update::install_method::{detect_install_method, InstallInfo, InstallMethod};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

/// TS parity: `update-manager.ts`'s literal `"alfin-efendy/ryuzi"` fallback.
pub const DEFAULT_REPO: &str = "alfin-efendy/ryuzi";
/// TS parity: `update-manager.ts`'s literal `"21600000"` fallback (6h).
pub const DEFAULT_CHECK_INTERVAL_MS: u64 = 21_600_000;

/// TS parity: `update-manager.ts`'s `Mode` union (`"auto" | "notify" | "off"`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpdateMode {
    Auto,
    Notify,
    Off,
}

/// The slice of `ControlPlane` the `UpdateManager` needs — TS parity:
/// `update-manager.ts`'s `NotifyTarget` interface (`ControlPlane` satisfies
/// it; see `impl NotifyTarget for ControlPlane` in `control.rs`).
#[async_trait::async_trait]
pub trait NotifyTarget: Send + Sync {
    async fn list_sessions(&self) -> Vec<Session>;
    fn emit(&self, e: CoreEvent);
}

/// Info handed to an `ApplyHook` for a self-applicable install — TS parity:
/// `update-manager.ts`'s inline `{ repo, tag, version }` `applyUpdate` arg.
#[derive(Debug, Clone)]
pub struct ApplyInfo {
    pub repo: String,
    pub tag: String,
    pub version: String,
}

/// Injectable self-apply implementation — TS parity: `UpdateManagerDeps.applyUpdate`.
#[async_trait::async_trait]
pub trait ApplyHook: Send + Sync {
    async fn apply(&self, info: ApplyInfo);
}

/// Constructor dependencies — TS parity: `update-manager.ts`'s `UpdateManagerDeps`.
pub struct UpdateManagerDeps {
    pub cp: Arc<dyn NotifyTarget>,
    pub settings: SettingsStore,
    pub version: String,
    pub exec_path: String,
    pub compiled: bool,
    pub home: Option<String>,
    pub docker_env: bool,
    pub http: Arc<dyn UpdateHttp>,
    #[allow(clippy::type_complexity)]
    pub log: Option<Box<dyn Fn(&str) + Send + Sync>>,
    pub apply_update: Option<Arc<dyn ApplyHook>>,
}

/// Orchestrates periodic update checks. TS parity: `update-manager.ts`'s
/// `UpdateManager` class (its `setInterval`-based `makeTimer` seam has no
/// Rust equivalent — `start`/`stop` drive a real `tokio::spawn`ed task
/// instead, tracked by `timer`).
pub struct UpdateManager {
    deps: UpdateManagerDeps,
    timer: Mutex<Option<JoinHandle<()>>>,
}

impl UpdateManager {
    pub fn new(deps: UpdateManagerDeps) -> Arc<Self> {
        Arc::new(Self {
            deps,
            timer: Mutex::new(None),
        })
    }

    pub async fn mode(&self) -> UpdateMode {
        match self
            .deps
            .settings
            .get("auto_update")
            .await
            .ok()
            .flatten()
            .as_deref()
        {
            Some("notify") => UpdateMode::Notify,
            Some("off") => UpdateMode::Off,
            _ => UpdateMode::Auto,
        }
    }

    pub async fn check_interval_ms(&self) -> u64 {
        self.deps
            .settings
            .get("auto_update_check_interval_ms")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CHECK_INTERVAL_MS)
    }

    pub async fn tick(&self) {
        if self.mode().await == UpdateMode::Off {
            return;
        }
        let repo = self
            .deps
            .settings
            .get("auto_update_repo")
            .await
            .ok()
            .flatten()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_REPO.to_string());
        let (http, current, repo2) = (
            Arc::clone(&self.deps.http),
            self.deps.version.clone(),
            repo.clone(),
        );
        let res: UpdateCheckResult =
            tokio::task::spawn_blocking(move || check_for_update(&current, &repo2, http.as_ref()))
                .await
                .unwrap_or_else(|_| UpdateCheckResult::none(&self.deps.version));
        if !res.update_available {
            return;
        }
        let Some(latest) = res.latest_version else {
            return;
        };
        let install = detect_install_method(
            &self.deps.exec_path,
            self.deps.compiled,
            self.deps.home.as_deref(),
            self.deps.docker_env,
        );
        if self.mode().await == UpdateMode::Auto && install.self_applicable {
            if let (Some(hook), Some(tag)) = (&self.deps.apply_update, &res.tag) {
                hook.apply(ApplyInfo {
                    repo,
                    tag: tag.clone(),
                    version: latest,
                })
                .await;
                return;
            }
        }
        self.notify(&latest, &install).await;
    }

    async fn notify(&self, version: &str, install: &InstallInfo) {
        if self
            .deps
            .settings
            .get("last_notified_version")
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(version)
        {
            return; // dedupe
        }
        let _ = self
            .deps
            .settings
            .set("last_notified_version", version)
            .await;
        let text = format!(
            "⬆️ ryuzi {version} is available - {}",
            upgrade_hint(install)
        );
        if let Some(log) = &self.deps.log {
            log(&text);
        }
        for s in self.deps.cp.list_sessions().await {
            if !matches!(s.status, SessionStatus::Idle | SessionStatus::Running) {
                continue;
            }
            self.deps.cp.emit(CoreEvent::Notice {
                session_pk: s.session_pk,
                text: text.clone(),
            });
        }
    }

    /// Initial tick on boot, then a real interval loop — no-op (arms no
    /// timer) if `mode()` is `off` at start-up time. TS parity:
    /// `update-manager.ts`'s `start()`.
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            if me.mode().await == UpdateMode::Off {
                return; // TS: off-mode arms no timer
            }
            me.tick().await; // initial check on boot
            let ms = me.check_interval_ms().await.max(1);
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(ms));
            interval.tick().await; // consume the immediate first tick
            loop {
                interval.tick().await;
                me.tick().await;
            }
        });
        *self.timer.lock().unwrap() = Some(handle);
    }

    /// Aborts the timer task. TS parity: `update-manager.ts`'s `stop()`.
    pub fn stop(&self) {
        if let Some(h) = self.timer.lock().unwrap().take() {
            h.abort();
        }
    }
}

/// TS parity: `update-manager.ts`'s module-private `upgradeHint`.
pub fn upgrade_hint(install: &InstallInfo) -> String {
    match install.method {
        InstallMethod::Brew => "run `brew upgrade ryuzi` to update.".into(),
        InstallMethod::Npm => "run `npm i -g ryuzi@latest` to update.".into(),
        InstallMethod::Scoop => "run `scoop update ryuzi` to update.".into(),
        InstallMethod::InstallSh => {
            "run `curl -fsSL https://github.com/alfin-efendy/ryuzi/raw/main/install.sh | sh` to update.".into()
        }
        _ => "see the GitHub release to update.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CoreEvent, Session, SessionStatus};
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use crate::update::check::{HttpResponse, UpdateHttp};
    use std::sync::{Arc, Mutex};

    struct FakeHttp {
        tag: Option<&'static str>,
        seen: Mutex<Vec<String>>,
    }
    impl UpdateHttp for FakeHttp {
        fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
            self.seen.lock().unwrap().push(url.to_string());
            let body = match self.tag {
                Some(t) => format!(r#"{{"tag_name":"{t}"}}"#),
                None => "{}".to_string(),
            };
            Ok(HttpResponse {
                status: 200,
                body: body.into_bytes(),
            })
        }
    }

    struct FakeTarget {
        sessions: Vec<Session>,
        emitted: Mutex<Vec<CoreEvent>>,
    }
    #[async_trait::async_trait]
    impl NotifyTarget for FakeTarget {
        async fn list_sessions(&self) -> Vec<Session> {
            self.sessions.clone()
        }
        fn emit(&self, e: CoreEvent) {
            self.emitted.lock().unwrap().push(e);
        }
    }

    struct CountingHook {
        applied: Mutex<Vec<ApplyInfo>>,
    }
    #[async_trait::async_trait]
    impl ApplyHook for CountingHook {
        async fn apply(&self, info: ApplyInfo) {
            self.applied.lock().unwrap().push(info);
        }
    }

    fn sess(pk: &str, status: SessionStatus) -> Session {
        Session {
            session_pk: pk.into(),
            project_id: "p1".into(),
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status,
            started_by: None,
            created_at: None,
            last_active: None,
            resume_attempts: 0,
        }
    }

    async fn settings() -> (SettingsStore, tempfile::NamedTempFile) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(f.path()).await.unwrap();
        (SettingsStore::new(Arc::new(store)), f)
    }

    struct Rig {
        um: Arc<UpdateManager>,
        target: Arc<FakeTarget>,
        settings: SettingsStore,
        _db: tempfile::NamedTempFile,
    }

    async fn rig(
        tag: Option<&'static str>,
        exec_path: &str,
        apply: Option<Arc<dyn ApplyHook>>,
    ) -> Rig {
        let (settings, _db) = settings().await;
        let target = Arc::new(FakeTarget {
            sessions: vec![
                sess("s1", SessionStatus::Idle),
                sess("s2", SessionStatus::Ended),
            ],
            emitted: Mutex::new(vec![]),
        });
        let um = UpdateManager::new(UpdateManagerDeps {
            cp: target.clone(),
            settings: settings.clone(), // SettingsStore is Arc<Store>-backed; Clone-derived.
            version: "0.2.0".into(),
            exec_path: exec_path.into(),
            compiled: true,
            home: Some("/home/me".into()),
            docker_env: false,
            http: Arc::new(FakeHttp {
                tag,
                seen: Mutex::new(vec![]),
            }),
            log: None,
            apply_update: apply,
        });
        Rig {
            um,
            target,
            settings,
            _db,
        }
    }

    #[tokio::test]
    async fn blank_auto_update_repo_falls_back_to_the_default_repo() {
        // Needs a retained handle to the fake HTTP's `seen` log, so this one
        // is built by hand rather than via `rig` (mirrors the TS test's own
        // inline `fetchImpl`, which likewise bypasses the `mgr()` helper).
        let (settings, _db) = settings().await;
        let target = Arc::new(FakeTarget {
            sessions: vec![
                sess("s1", SessionStatus::Idle),
                sess("s2", SessionStatus::Ended),
            ],
            emitted: Mutex::new(vec![]),
        });
        let http = Arc::new(FakeHttp {
            tag: Some("v0.3.0"),
            seen: Mutex::new(vec![]),
        });
        let um = UpdateManager::new(UpdateManagerDeps {
            cp: target.clone(),
            settings: settings.clone(),
            version: "0.2.0".into(),
            exec_path: "/home/me/.local/bin/ryuzi".into(),
            compiled: true,
            home: Some("/home/me".into()),
            docker_env: false,
            http: http.clone(),
            log: None,
            apply_update: None,
        });
        settings.set("auto_update_repo", "").await.unwrap();
        um.tick().await;
        assert_eq!(
            http.seen.lock().unwrap()[0],
            "https://api.github.com/repos/alfin-efendy/ryuzi/releases/latest"
        );
    }

    #[tokio::test]
    async fn tick_broadcasts_a_notice_to_non_ended_sessions_and_records_last_notified_version() {
        let r = rig(Some("v0.3.0"), "/home/me/.local/bin/ryuzi", None).await;
        r.um.tick().await;
        {
            let emitted = r.target.emitted.lock().unwrap();
            assert_eq!(emitted.len(), 1); // only s1 (s2 is ended)
            match &emitted[0] {
                CoreEvent::Notice { session_pk, text } => {
                    assert_eq!(session_pk, "s1");
                    assert!(text.contains("ryuzi 0.3.0 is available"));
                }
                other => panic!("expected Notice, got {other:?}"),
            }
        }
        assert_eq!(
            r.settings
                .get("last_notified_version")
                .await
                .unwrap()
                .as_deref(),
            Some("0.3.0")
        );
    }

    #[tokio::test]
    async fn tick_dedupes_a_second_tick_for_the_same_version() {
        let r = rig(Some("v0.3.0"), "/home/me/.local/bin/ryuzi", None).await;
        r.um.tick().await;
        r.um.tick().await;
        assert_eq!(r.target.emitted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn no_broadcast_when_there_is_no_newer_version() {
        let r = rig(Some("v0.2.0"), "/home/me/.local/bin/ryuzi", None).await;
        r.um.tick().await;
        assert_eq!(r.target.emitted.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn mode_off_tick_is_a_noop_and_start_spawns_no_work() {
        let (settings, _db) = settings().await;
        let target = Arc::new(FakeTarget {
            sessions: vec![
                sess("s1", SessionStatus::Idle),
                sess("s2", SessionStatus::Ended),
            ],
            emitted: Mutex::new(vec![]),
        });
        let http = Arc::new(FakeHttp {
            tag: Some("v0.3.0"),
            seen: Mutex::new(vec![]),
        });
        let um = UpdateManager::new(UpdateManagerDeps {
            cp: target.clone(),
            settings: settings.clone(),
            version: "0.2.0".into(),
            exec_path: "/home/me/.local/bin/ryuzi".into(),
            compiled: true,
            home: Some("/home/me".into()),
            docker_env: false,
            http: http.clone(),
            log: None,
            apply_update: None,
        });
        settings.set("auto_update", "off").await.unwrap();

        um.tick().await;
        assert_eq!(target.emitted.lock().unwrap().len(), 0);
        assert_eq!(http.seen.lock().unwrap().len(), 0);

        um.start();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(http.seen.lock().unwrap().len(), 0); // start() must skip ticking entirely in off-mode
        assert!(um
            .timer
            .lock()
            .unwrap()
            .as_ref()
            .expect("start() should have recorded a handle")
            .is_finished());
    }

    #[tokio::test]
    async fn upgrade_hint_brew_execpath_produces_brew_upgrade_hint() {
        let r = rig(Some("v0.3.0"), "/opt/homebrew/bin/ryuzi", None).await;
        r.um.tick().await;
        let emitted = r.target.emitted.lock().unwrap();
        match &emitted[0] {
            CoreEvent::Notice { text, .. } => assert!(text.contains("brew upgrade ryuzi")),
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upgrade_hint_npm_execpath_produces_npm_install_hint() {
        let r = rig(
            Some("v0.3.0"),
            "/usr/local/lib/node_modules/ryuzi/bin/ryuzi",
            None,
        )
        .await;
        r.um.tick().await;
        let emitted = r.target.emitted.lock().unwrap();
        match &emitted[0] {
            CoreEvent::Notice { text, .. } => assert!(text.contains("npm i -g ryuzi@latest")),
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upgrade_hint_scoop_execpath_produces_scoop_update_hint() {
        let r = rig(
            Some("v0.3.0"),
            r"C:\Users\me\scoop\apps\ryuzi\current\ryuzi.exe",
            None,
        )
        .await;
        r.um.tick().await;
        let emitted = r.target.emitted.lock().unwrap();
        match &emitted[0] {
            CoreEvent::Notice { text, .. } => assert!(text.contains("scoop update ryuzi")),
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upgrade_hint_installsh_path_produces_curl_install_hint() {
        let r = rig(Some("v0.3.0"), "/home/me/.local/bin/ryuzi", None).await;
        r.um.tick().await;
        let emitted = r.target.emitted.lock().unwrap();
        match &emitted[0] {
            CoreEvent::Notice { text, .. } => {
                assert!(text.contains("https://github.com/alfin-efendy/ryuzi/raw/main/install.sh"))
            }
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upgrade_hint_unknown_execpath_produces_github_release_hint() {
        let r = rig(Some("v0.3.0"), "/some/unknown/path/ryuzi", None).await;
        r.um.tick().await;
        let emitted = r.target.emitted.lock().unwrap();
        match &emitted[0] {
            CoreEvent::Notice { text, .. } => assert!(text.contains("GitHub release")),
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auto_mode_on_a_self_applicable_install_triggers_apply_not_a_notice() {
        let hook = Arc::new(CountingHook {
            applied: Mutex::new(vec![]),
        });
        let r = rig(
            Some("v0.3.0"),
            "/home/me/.local/bin/ryuzi", // installsh → self_applicable
            Some(hook.clone() as Arc<dyn ApplyHook>),
        )
        .await;
        r.um.tick().await;
        let applied = hook.applied.lock().unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].tag, "v0.3.0");
        assert_eq!(r.target.emitted.lock().unwrap().len(), 0); // applied, not announced
    }

    #[tokio::test]
    async fn auto_mode_on_a_non_self_applicable_install_still_notifies() {
        let hook = Arc::new(CountingHook {
            applied: Mutex::new(vec![]),
        });
        let r = rig(
            Some("v0.3.0"),
            "/opt/homebrew/bin/ryuzi", // brew → notify-only
            Some(hook.clone() as Arc<dyn ApplyHook>),
        )
        .await;
        r.um.tick().await;
        assert_eq!(hook.applied.lock().unwrap().len(), 0);
        assert_eq!(r.target.emitted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn notify_mode_never_applies_even_on_self_applicable() {
        let hook = Arc::new(CountingHook {
            applied: Mutex::new(vec![]),
        });
        let r = rig(
            Some("v0.3.0"),
            "/home/me/.local/bin/ryuzi",
            Some(hook.clone() as Arc<dyn ApplyHook>),
        )
        .await;
        r.settings.set("auto_update", "notify").await.unwrap();
        r.um.tick().await;
        assert_eq!(hook.applied.lock().unwrap().len(), 0);
        assert_eq!(r.target.emitted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn interrupted_and_ended_sessions_are_excluded_from_the_broadcast() {
        let (settings, _db) = settings().await;
        let target = Arc::new(FakeTarget {
            sessions: vec![
                sess("s1", SessionStatus::Idle),
                sess("s2", SessionStatus::Running),
                sess("s3", SessionStatus::Interrupted),
                sess("s4", SessionStatus::Ended),
            ],
            emitted: Mutex::new(vec![]),
        });
        let um = UpdateManager::new(UpdateManagerDeps {
            cp: target.clone(),
            settings,
            version: "0.2.0".into(),
            exec_path: "/home/me/.local/bin/ryuzi".into(),
            compiled: true,
            home: Some("/home/me".into()),
            docker_env: false,
            http: Arc::new(FakeHttp {
                tag: Some("v0.3.0"),
                seen: Mutex::new(vec![]),
            }),
            log: None,
            apply_update: None,
        });
        um.tick().await;
        let emitted = target.emitted.lock().unwrap();
        assert_eq!(emitted.len(), 2); // only idle and running are included
        let mut pks: Vec<&str> = emitted
            .iter()
            .map(|e| match e {
                CoreEvent::Notice { session_pk, .. } => session_pk.as_str(),
                other => panic!("expected Notice, got {other:?}"),
            })
            .collect();
        pks.sort();
        assert_eq!(pks, vec!["s1", "s2"]);
    }

    #[tokio::test]
    async fn default_check_interval_is_6h() {
        let r = rig(None, "/home/me/.local/bin/ryuzi", None).await;
        assert_eq!(r.um.check_interval_ms().await, DEFAULT_CHECK_INTERVAL_MS);
        r.settings
            .set("auto_update_check_interval_ms", "1234")
            .await
            .unwrap();
        assert_eq!(r.um.check_interval_ms().await, 1234);
    }
}
