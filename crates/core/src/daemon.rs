//! Daemon composition root: wires `Store` → settings → telemetry → harness
//! registries → `ControlPlane` → gateways → the outbound `Router` → the
//! approval fan-out, mirroring the retired TypeScript daemon's boot sequence
//! (`packages/daemon/src/main.ts`).
//!
//! [`build_daemon`] is the single entry point; [`Daemon::start`]/`stop` drive
//! the lifecycle. The approval fan-out — the piece that finally consumes
//! `approval_timeout_ms` — is kept in a standalone, unit-testable
//! `pub(crate)` function ([`handle_approval`]) separate from the broadcast
//! loop that spawns it.

use crate::control::ControlPlane;
use crate::domain::{ApprovalDecision, ApprovalRequest, CoreEvent, Surface};
use crate::gateway::{Gateway, GatewayFactory};
use crate::harness::acp::{AcpAdapterDescriptor, ClaudeCodeIntegration};
use crate::integration::Registries;
use crate::policy;
use crate::router::Router;
use crate::settings::{csv, SettingsStore, CATALOG};
use crate::store::Store;
use crate::telemetry::{ConsoleTelemetry, Telemetry};
use futures::FutureExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Inputs to [`build_daemon`].
pub struct BuildDaemonOpts {
    /// Path to the sqlite database (created/migrated by `Store::open`).
    pub db_path: PathBuf,
    /// Lazily resolves the ACP adapter descriptor (e.g. locates/downloads the
    /// bundled sidecar). This is called AT MOST ONCE, and only if
    /// `enabled_runtimes` (a persisted setting) includes `"claude-code"` — a
    /// gateway-only daemon with no runtime enabled never pays the cost of
    /// resolving (or downloading) the sidecar. The caller (CLI: the sidecar
    /// resolver) owns any I/O this closure performs.
    pub adapter: Box<dyn FnOnce() -> anyhow::Result<AcpAdapterDescriptor> + Send>,
    /// Override the telemetry backend (used by tests). `None` selects
    /// Console, or OTLP-behind-a-feature-flag once `otel_endpoint` is set and
    /// Task 6 lands; see [`build_daemon`]'s doc for the current fallback.
    pub telemetry: Option<Arc<dyn Telemetry>>,
    /// Gateway factories available to wire, keyed by the id an entry in the
    /// `enabled_gateways` setting names (e.g. `"discord"`). 4D-b registers
    /// its gateway(s) here (directly or via an `Integration`).
    pub extra_gateway_factories: Vec<(String, Arc<dyn GatewayFactory>)>,
}

/// A fully wired daemon: control plane, shared store handle, and the
/// gateways `build_daemon` constructed. `store` is the SAME `Arc<Store>` the
/// `ControlPlane` holds internally (via `cp.store()`), so callers needing
/// direct DB access (e.g. HTTP read endpoints) share one connection pool.
pub struct Daemon {
    pub cp: Arc<ControlPlane>,
    pub store: Arc<Store>,
    pub gateways: Vec<Arc<dyn Gateway>>,
    telemetry: Arc<dyn Telemetry>,
    stopped: AtomicBool,
}

impl Daemon {
    /// Start every gateway, then fire-and-forget `cp.reconcile()` (resumes
    /// any session a dead process left `Running`). Reconcile runs in the
    /// background so a slow/hanging resume can't block daemon startup.
    pub async fn start(&self) -> anyhow::Result<()> {
        for gw in &self.gateways {
            gw.start().await?;
        }
        let cp = Arc::clone(&self.cp);
        tokio::spawn(async move {
            let _ = cp.reconcile().await;
        });
        Ok(())
    }

    /// Idempotent teardown: stop every gateway (errors swallowed — TS parity)
    /// and flush telemetry. A second call is a no-op.
    pub async fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        for gw in &self.gateways {
            let _ = gw.stop().await;
        }
        self.telemetry.shutdown();
    }
}

/// Build order (TS daemon parity):
/// `Store::open` → settings → telemetry select → `Registries` (installs
/// `ClaudeCodeIntegration` iff `enabled_runtimes` contains `"claude-code"`) →
/// `ControlPlane::new_with_telemetry` → gateways (from `enabled_gateways` +
/// `extra_gateway_factories` + the provider catalog) → the outbound `Router`
/// spawned on one `cp.subscribe()` → the approval fan-out spawned on
/// another.
///
/// Telemetry selection: an explicit `opts.telemetry` override always wins;
/// otherwise an empty/unset `otel_endpoint` setting selects `ConsoleTelemetry`.
/// A non-empty `otel_endpoint` is meant to select an OTLP exporter behind
/// `#[cfg(feature = "otel")]` (Task 6; the feature doesn't exist yet), so
/// today it prints the `[telemetry] OTel init failed — falling back to
/// console` warning and falls back to `ConsoleTelemetry` too.
pub async fn build_daemon(opts: BuildDaemonOpts) -> anyhow::Result<Daemon> {
    let store = Arc::new(Store::open(&opts.db_path).await?);

    let telemetry: Arc<dyn Telemetry> = match opts.telemetry {
        Some(t) => t,
        None => {
            let settings = SettingsStore::new(Arc::clone(&store));
            let endpoint = settings.get("otel_endpoint").await?.unwrap_or_default();
            if endpoint.trim().is_empty() {
                Arc::new(ConsoleTelemetry::new())
            } else {
                // Task 6 slots the real OTLP exporter here, behind
                // `#[cfg(feature = "otel")]` (the feature doesn't exist yet).
                // Until then, every configured endpoint falls back to
                // console with this warning.
                eprintln!("[telemetry] OTel init failed — falling back to console");
                Arc::new(ConsoleTelemetry::new())
            }
        }
    };

    let mut registries = Registries::new();
    {
        let settings = SettingsStore::new(Arc::clone(&store));
        let enabled_runtimes = csv(settings.get("enabled_runtimes").await?.as_deref());
        if enabled_runtimes.iter().any(|r| r == "claude-code") {
            let descriptor = (opts.adapter)()?;
            registries.install(&ClaudeCodeIntegration::new(descriptor));
        }
    }

    // Reclaim exclusive ownership of the store: the two `SettingsStore`
    // clones above are already dropped (block-scoped), so this Arc has
    // exactly one strong reference — ours.
    let store = Arc::try_unwrap(store)
        .map_err(|_| anyhow::anyhow!("build_daemon: store Arc has outstanding references"))?;

    let cp = ControlPlane::new_with_telemetry(store, registries, Arc::clone(&telemetry)).await;
    let store = cp.store();
    let settings = SettingsStore::new(Arc::clone(&store));

    let factories: HashMap<String, Arc<dyn GatewayFactory>> =
        opts.extra_gateway_factories.into_iter().collect();
    let mut gateways: Vec<Arc<dyn Gateway>> = Vec::new();
    for id in csv(settings.get("enabled_gateways").await?.as_deref()) {
        let Some(factory) = factories.get(&id) else {
            continue; // no registered factory for this id — skip silently
        };
        let Some(descriptor) = CATALOG.gateway(&id) else {
            continue; // no catalog entry to build a config from — skip silently
        };
        let mut config = serde_json::Map::new();
        for field in descriptor.fields {
            let value = settings.get(field.key).await?.unwrap_or_default();
            config.insert(field.key.to_string(), serde_json::Value::String(value));
        }
        let gw = factory.create(&serde_json::Value::Object(config))?;
        gateways.push(gw);
    }

    let router = Router::new(Arc::clone(&store), gateways.clone());
    tokio::spawn(router.run(cp.subscribe()));

    spawn_approval_fanout(Arc::clone(&cp), Arc::clone(&store), gateways.clone());

    Ok(Daemon {
        cp,
        store,
        gateways,
        telemetry,
        stopped: AtomicBool::new(false),
    })
}

/// Subscribe to `cp`'s event bus and spawn one [`handle_approval`] task per
/// `ApprovalRequested` event. Runs until the broadcast channel closes.
fn spawn_approval_fanout(
    cp: Arc<ControlPlane>,
    store: Arc<Store>,
    gateways: Vec<Arc<dyn Gateway>>,
) {
    let mut rx = cp.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(CoreEvent::ApprovalRequested {
                    session_pk,
                    request_id,
                    tool,
                    summary,
                }) => {
                    let cp = Arc::clone(&cp);
                    let store = Arc::clone(&store);
                    let gateways = gateways.clone();
                    tokio::spawn(async move {
                        handle_approval(
                            &cp,
                            &store,
                            &gateways,
                            &session_pk,
                            &request_id,
                            &tool,
                            &summary,
                        )
                        .await;
                    });
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Core approval fan-out decision, callable directly (no broadcast loop
/// needed) so it's unit-testable: reads `approval_timeout_ms` /
/// `approver_role_ids` from settings and `started_by` from the session, then
/// resolves via `cp.resolve_approval`.
///
/// - No surfaces bound to the session (after filtering to gateways we know
///   about) → immediate deny.
/// - Otherwise races `gw.request_approval` across every known surface via
///   `futures::future::select_all`; a per-gateway `Err` counts as an instant
///   `RejectOnce` vote. The whole race is wrapped in `tokio::time::timeout`;
///   elapsing denies.
pub(crate) async fn handle_approval(
    cp: &Arc<ControlPlane>,
    store: &Arc<Store>,
    gateways: &[Arc<dyn Gateway>],
    session_pk: &str,
    request_id: &str,
    tool: &str,
    summary: &str,
) {
    let settings = SettingsStore::new(Arc::clone(store));

    let timeout_ms: u64 = settings
        .get("approval_timeout_ms")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000);

    let approver_role_ids = policy::parse_role_ids(
        settings
            .get("approver_role_ids")
            .await
            .ok()
            .flatten()
            .as_deref(),
    );

    let started_by = store
        .get_session(session_pk)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.started_by);

    let known_surfaces: Vec<(Surface, Arc<dyn Gateway>)> = store
        .surfaces(session_pk)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|surface| {
            gateways
                .iter()
                .find(|g| g.id() == surface.gateway)
                .cloned()
                .map(|gw| (surface, gw))
        })
        .collect();

    if known_surfaces.is_empty() {
        cp.resolve_approval(request_id, false);
        return;
    }

    let req = ApprovalRequest {
        request_id: request_id.to_string(),
        tool: tool.to_string(),
        summary: summary.to_string(),
        approver_role_ids,
        started_by,
        timeout_ms: Some(timeout_ms),
    };

    let futs = known_surfaces.into_iter().map(|(surface, gw)| {
        let req = req.clone();
        async move {
            gw.request_approval(&surface, &req)
                .await
                .unwrap_or(ApprovalDecision::RejectOnce)
        }
        .boxed()
    });

    let decision = match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        futures::future::select_all(futs),
    )
    .await
    {
        Ok((decision, _idx, _rest)) => decision,
        Err(_elapsed) => ApprovalDecision::RejectOnce,
    };

    cp.resolve_approval(
        request_id,
        matches!(
            decision,
            ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NewMessage, PermMode, Project, Session, SessionStatus};
    use crate::gateway::MessageRef;
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use crate::telemetry::NoopTelemetry;
    use async_trait::async_trait;
    use serial_test::serial;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    // ---------- shared test plumbing ----------

    fn temp_db_path() -> (tempfile::NamedTempFile, PathBuf) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        (f, path)
    }

    fn capturing_console_telemetry() -> (Arc<Mutex<Vec<String>>>, Arc<dyn Telemetry>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = lines.clone();
        let telemetry = ConsoleTelemetry::with_sink(
            move |line: &str| captured.lock().unwrap().push(line.to_string()),
            || 1_000,
        );
        (lines, Arc::new(telemetry))
    }

    fn parse_telemetry_lines(lines: &Arc<Mutex<Vec<String>>>) -> Vec<serde_json::Value> {
        lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).expect("telemetry line must be valid JSON"))
            .collect()
    }

    async fn control_plane_with_telemetry(
        telemetry: Arc<dyn Telemetry>,
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        let (guard, path) = temp_db_path();
        let store = Store::open(&path).await.unwrap();
        let cp = ControlPlane::new_with_telemetry(store, Registries::new(), telemetry).await;
        let store = cp.store();
        (cp, store, guard)
    }

    async fn seed_project(store: &Store, project_id: &str) {
        store
            .insert_project(Project {
                project_id: project_id.to_string(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                harness: "claude-code".into(),
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(crate::paths::now_ms()),
            })
            .await
            .unwrap();
    }

    async fn seed_session(
        store: &Store,
        session_pk: &str,
        project_id: &str,
        started_by: Option<&str>,
    ) {
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: session_pk.to_string(),
                project_id: project_id.to_string(),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Idle,
                started_by: started_by.map(|s| s.to_string()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
            })
            .await
            .unwrap();
    }

    /// Redirects `dirs::data_dir()`/HOME into a tempdir for the duration of a
    /// test, so worktree creation never touches the real state dir. Process
    /// global env — every test using it must be `#[serial]`.
    struct StateDirGuard {
        _dir: tempfile::TempDir,
    }
    impl StateDirGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("HOME", dir.path());
            StateDirGuard { _dir: dir }
        }
    }

    fn init_repo(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    // ---------- FakeGateway: configurable approval behavior ----------

    #[derive(Clone, Copy)]
    enum GwBehavior {
        Allow,
        SleepThenAllow(u64),
    }

    struct FakeGateway {
        gid: String,
        behavior: GwBehavior,
        calls: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        last_req: Arc<Mutex<Option<ApprovalRequest>>>,
    }

    impl FakeGateway {
        fn new(gid: &str, behavior: GwBehavior) -> Self {
            FakeGateway {
                gid: gid.to_string(),
                behavior,
                calls: Arc::new(AtomicUsize::new(0)),
                stops: Arc::new(AtomicUsize::new(0)),
                last_req: Arc::new(Mutex::new(None)),
            }
        }
    }

    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            &self.gid
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(&self, _w: &str, _t: &str) -> anyhow::Result<String> {
            Ok("conv".into())
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m".into(),
            })
        }
        async fn edit_status(&self, _msg: &MessageRef, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(&self, _surface: &Surface, _chunks: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(&self, _surface: &Surface, _message: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _surface: &Surface,
            req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_req.lock().unwrap() = Some(req.clone());
            match self.behavior {
                GwBehavior::Allow => Ok(ApprovalDecision::AllowOnce),
                GwBehavior::SleepThenAllow(ms) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok(ApprovalDecision::AllowOnce)
                }
            }
        }
    }

    // ---------- (a) build_daemon wiring ----------

    struct CapturingGatewayFactory {
        captured: Arc<Mutex<Option<serde_json::Value>>>,
    }
    impl GatewayFactory for CapturingGatewayFactory {
        fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            *self.captured.lock().unwrap() = Some(config.clone());
            Ok(Arc::new(FakeGateway::new("discord", GwBehavior::Allow)))
        }
    }

    #[tokio::test]
    async fn build_daemon_wires_known_gateways_and_skips_unknown_ids() {
        let (_guard, db_path) = temp_db_path();
        {
            let store = Store::open(&db_path).await.unwrap();
            let settings = SettingsStore::new(Arc::new(store));
            settings
                .set("enabled_gateways", "discord,bogus")
                .await
                .unwrap();
            settings.set("discord.token", "tok-xyz").await.unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let factory: Arc<dyn GatewayFactory> = Arc::new(CapturingGatewayFactory {
            captured: captured.clone(),
        });

        let daemon = build_daemon(BuildDaemonOpts {
            db_path: db_path.clone(),
            adapter: Box::new(|| Ok(AcpAdapterDescriptor::default())),
            telemetry: Some(Arc::new(NoopTelemetry)),
            extra_gateway_factories: vec![("discord".to_string(), factory)],
        })
        .await
        .unwrap();

        assert_eq!(
            daemon.gateways.len(),
            1,
            "only the known 'discord' id should be wired; 'bogus' must be skipped"
        );
        let cfg = captured
            .lock()
            .unwrap()
            .clone()
            .expect("factory must have been called");
        assert_eq!(cfg["discord.token"], "tok-xyz");
        assert_eq!(cfg["discord.app_id"], "");
        assert_eq!(cfg["discord.guild_id"], "");
    }

    // ---------- (b)/(c)/(d) handle_approval unit tests ----------

    #[tokio::test]
    async fn handle_approval_allow_resolves_true() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", Some("starter-1")).await;
        store.add_surface("discord", "chan1", "s1").await.unwrap();
        SettingsStore::new(store.clone())
            .set("approver_role_ids", "role-a, role-b")
            .await
            .unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let last_req = gw.last_req.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        handle_approval(&cp, &store, &gateways, "s1", "req-1", "Bash", "ls -la").await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.allow"),
            "expected an approval.allow count line, got: {parsed:?}"
        );

        let captured = last_req
            .lock()
            .unwrap()
            .clone()
            .expect("gateway must have been asked");
        assert_eq!(captured.started_by.as_deref(), Some("starter-1"));
        assert_eq!(
            captured.approver_role_ids,
            vec!["role-a".to_string(), "role-b".to_string()]
        );
        assert_eq!(captured.timeout_ms, Some(300_000)); // default
        assert_eq!(captured.tool, "Bash");
        assert_eq!(captured.summary, "ls -la");
    }

    #[tokio::test]
    async fn handle_approval_timeout_denies() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        store.add_surface("discord", "chan1", "s1").await.unwrap();
        SettingsStore::new(store.clone())
            .set("approval_timeout_ms", "100")
            .await
            .unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::SleepThenAllow(2_000));
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        handle_approval(&cp, &store, &gateways, "s1", "req-2", "Bash", "sleep").await;

        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
            "a timed-out race must deny, got: {parsed:?}"
        );
    }

    #[tokio::test]
    async fn handle_approval_no_surfaces_denies_immediately() {
        let (lines, telemetry) = capturing_console_telemetry();
        let (cp, store, _guard) = control_plane_with_telemetry(telemetry).await;
        seed_project(&store, "p1").await;
        seed_session(&store, "s1", "p1", None).await;
        // Deliberately no add_surface.

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let calls = gw.calls.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        handle_approval(&cp, &store, &gateways, "s1", "req-3", "Bash", "ls").await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no surfaces means the gateway must never be asked"
        );
        let parsed = parse_telemetry_lines(&lines);
        assert!(
            parsed
                .iter()
                .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
            "no surfaces must deny immediately, got: {parsed:?}"
        );
    }

    // ---------- (b, integration) approval fan-out lets a blocked turn complete ----------

    struct PermFakeSession {
        store: Arc<Store>,
        events: broadcast::Sender<CoreEvent>,
        approvals: Arc<crate::approval::ApprovalHub>,
        session_pk: String,
    }

    #[async_trait]
    impl HarnessSession for PermFakeSession {
        async fn send_prompt(&self, prompt: String) -> anyhow::Result<()> {
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt }),
                ))
                .await;

            let request_id = "perm-req-1".to_string();
            let _ = self.events.send(CoreEvent::ApprovalRequested {
                session_pk: self.session_pk.clone(),
                request_id: request_id.clone(),
                tool: "Bash".into(),
                summary: "ls -la".into(),
            });
            let rx = self.approvals.register(request_id);
            let allow = rx.await.unwrap_or(false);
            if allow {
                let _ = self
                    .store
                    .insert_message(NewMessage::block(
                        &self.session_pk,
                        "assistant",
                        "text",
                        serde_json::json!({ "text": "done" }),
                    ))
                    .await;
            }
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-1".into())
        }
    }

    struct PermFakeHarness;
    #[async_trait]
    impl Harness for PermFakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(PermFakeSession {
                store: ctx.store.clone(),
                events: ctx.events.clone(),
                approvals: ctx.approvals.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }
    struct PermFakeHarnessFactory;
    impl HarnessFactory for PermFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(PermFakeHarness))
        }
    }

    #[tokio::test]
    #[serial]
    async fn approval_fanout_allows_a_blocked_turn_to_complete_end_to_end() {
        let _guard = StateDirGuard::new();
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let mut regs = Registries::new();
        regs.harness
            .register("claude-code", Arc::new(PermFakeHarnessFactory));
        let cp = ControlPlane::new_with_telemetry(store, regs, Arc::new(NoopTelemetry)).await;
        let store = cp.store();

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        let mut rx = cp.subscribe();
        let session = cp
            .start_session(&project.project_id, "go", "test")
            .await
            .unwrap();

        let mut saw_result = false;
        for _ in 0..200 {
            let recv = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
            match recv {
                Ok(Ok(CoreEvent::ApprovalRequested {
                    session_pk,
                    request_id,
                    tool,
                    summary,
                })) => {
                    // Bind the surface now (we know the session_pk is live —
                    // the harness is already blocked awaiting the hub).
                    store
                        .add_surface("discord", "chan1", &session_pk)
                        .await
                        .unwrap();
                    handle_approval(
                        &cp,
                        &store,
                        &gateways,
                        &session_pk,
                        &request_id,
                        &tool,
                        &summary,
                    )
                    .await;
                }
                Ok(Ok(CoreEvent::Result { .. })) => {
                    saw_result = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(
            saw_result,
            "expected the turn to complete (Result event) once the gateway allowed"
        );

        let msgs = store.list_messages(&session.session_pk).await.unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.payload["text"] == "done"),
            "expected the post-approval assistant row, got: {msgs:?}"
        );
    }

    // ---------- (e) Daemon::start fires reconcile ----------

    struct ResumeFakeSession {
        store: Arc<Store>,
        session_pk: String,
        prompts: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl HarnessSession for ResumeFakeSession {
        async fn send_prompt(&self, prompt: String) -> anyhow::Result<()> {
            self.prompts.lock().unwrap().push(prompt.clone());
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt }),
                ))
                .await;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-1".into())
        }
    }
    struct ResumeFakeHarness {
        prompts: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Harness for ResumeFakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(ResumeFakeSession {
                store: ctx.store.clone(),
                session_pk: ctx.session_pk.clone(),
                prompts: self.prompts.clone(),
            }))
        }
    }
    struct ResumeFakeHarnessFactory {
        prompts: Arc<Mutex<Vec<String>>>,
    }
    impl HarnessFactory for ResumeFakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(ResumeFakeHarness {
                prompts: self.prompts.clone(),
            }))
        }
    }

    #[tokio::test]
    async fn daemon_start_fires_reconcile_and_resumes_running_sessions() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut regs = Registries::new();
        regs.harness.register(
            "claude-code",
            Arc::new(ResumeFakeHarnessFactory {
                prompts: prompts.clone(),
            }),
        );
        let cp = ControlPlane::new_with_telemetry(store, regs, Arc::new(NoopTelemetry)).await;
        let store = cp.store();
        seed_project(&store, "p1").await;
        let now = crate::paths::now_ms();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                project_id: "p1".into(),
                agent_session_id: Some("acp-123".into()),
                worktree_path: None,
                branch: None,
                title: Some("seed".into()),
                status: SessionStatus::Running,
                started_by: Some("test".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
            })
            .await
            .unwrap();

        let daemon = Daemon {
            cp,
            store,
            gateways: vec![],
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
        };

        daemon.start().await.unwrap();

        for _ in 0..400 {
            if !prompts.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            prompts.lock().unwrap().first().cloned(),
            Some(crate::control::RESUME_NUDGE.to_string()),
            "Daemon::start must fire reconcile, resuming the Running session with the nudge"
        );
    }

    // ---------- (f) Daemon::stop is idempotent ----------

    #[tokio::test]
    async fn daemon_stop_is_idempotent_and_stops_each_gateway_once() {
        let (_db_guard, db_path) = temp_db_path();
        let store = Store::open(&db_path).await.unwrap();
        let cp =
            ControlPlane::new_with_telemetry(store, Registries::new(), Arc::new(NoopTelemetry))
                .await;
        let store = cp.store();

        let gw = FakeGateway::new("discord", GwBehavior::Allow);
        let stops = gw.stops.clone();
        let gateways: Vec<Arc<dyn Gateway>> = vec![Arc::new(gw)];

        let daemon = Daemon {
            cp,
            store,
            gateways,
            telemetry: Arc::new(NoopTelemetry),
            stopped: AtomicBool::new(false),
        };

        daemon.stop().await;
        daemon.stop().await;

        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "a second stop() must not re-invoke gateway.stop()"
        );
    }
}
