use super::*;
use crate::domain::{
    ApprovalDecision, ApprovalScope, AttachmentRef, CoreEvent, NewMessage, SessionStatus,
};
use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
use crate::paths::now_ms;
use crate::plugins::Registries;
use crate::settings::SettingsStore;
use async_trait::async_trait;
use serial_test::serial;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Redirect dirs::data_dir() into a tempdir for the duration of a test so
/// worktree creation never touches the real ~/.local/share. Process-global
/// env — every test using it must be #[serial]. Also drops a `.gitconfig`
/// under the redirected `HOME` with a throwaway identity, so real `git
/// commit` subprocesses spawned by `provision_project`'s name-flow (which
/// shells to literal `git ... commit`, needing a resolvable
/// user.name/user.email) succeed without touching the developer's real
/// global git config.
struct StateDirGuard {
    _dir: tempfile::TempDir,
}

impl StateDirGuard {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("HOME", dir.path());
        std::fs::write(
            dir.path().join(".gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.com\n",
        )
        .expect("write .gitconfig");
        StateDirGuard { _dir: dir }
    }
}

/// A fake `HarnessSession` that, on `send_prompt`, persists a user turn and a
/// streamed assistant text row through the `SessionCtx.store`, then records
/// the agent session id — mirroring what the real ACP sink/session does. It
/// blocks until `cancel()` fires if `block_until_cancel` is set, so tests can
/// exercise the "stop while running" transition.
///
/// `send_count`/`ended` are shared counters (via the factory) so tests can
/// assert how many prompts a live session served and whether `end()` was
/// called on it.
struct FakeSession {
    store: Arc<Store>,
    events: broadcast::Sender<CoreEvent>,
    session_pk: String,
    block_until_cancel: bool,
    cancelled: Arc<AtomicBool>,
    send_count: Arc<AtomicUsize>,
    ended: Arc<AtomicBool>,
    /// Every prompt text driven on this (or a sibling) fake session, in
    /// order — lets resume tests assert the exact nudge text sent.
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl HarnessSession for FakeSession {
    async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
        self.send_count.fetch_add(1, Ordering::SeqCst);
        // Record the agent-visible (possibly manifest-decorated) text —
        // tests assert on exactly what the harness/agent was driven with.
        self.prompts.lock().unwrap().push(prompt.agent.clone());
        // Persist the user turn (as the ACP session does in send_prompt),
        // using the RAW display text — not the agent-decorated one — so
        // durable history mirrors what the real ACP session persists.
        let _ = self
            .store
            .insert_message(NewMessage::block(
                &self.session_pk,
                "user",
                "text",
                serde_json::json!({ "text": prompt.display }),
            ))
            .await;

        // Stream an assistant text row + broadcast it (as the sink does).
        if let Ok(seq) = self
            .store
            .insert_message(NewMessage::block(
                &self.session_pk,
                "assistant",
                "text",
                serde_json::json!({ "text": "working" }),
            ))
            .await
        {
            let _ = self.events.send(CoreEvent::Message {
                session_pk: self.session_pk.clone(),
                seq,
                role: "assistant".into(),
                block_type: "text".into(),
                payload: serde_json::json!({ "text": "working" }),
                tool_call_id: None,
                status: None,
                tool_kind: None,
            });
        }

        if self.block_until_cancel {
            // Block until cancel() is observed, so the session stays Running.
            loop {
                if self.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        Ok(())
    }

    async fn cancel(&self) -> anyhow::Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn end(&self) -> anyhow::Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        self.ended.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn agent_session_id(&self) -> Option<String> {
        Some("agent-1".to_string())
    }
}

/// Shared counters so tests can observe the harness/session lifecycle across
/// `start_session` / `continue_session` calls.
#[derive(Clone, Default)]
struct Counters {
    /// Times `Harness::start_session` was invoked (new ACP session created).
    starts: Arc<AtomicUsize>,
    /// Times `send_prompt` was driven on any produced session.
    sends: Arc<AtomicUsize>,
    /// Set once `end()` is called on a produced session.
    ended: Arc<AtomicBool>,
    /// Prompts observed by `send_prompt` across every produced session, in order.
    prompts: Arc<Mutex<Vec<String>>>,
    /// The `SessionCtx.mcp_servers` the most recent `start_session` call was
    /// built with — lets plugin-connector tests assert on exactly what
    /// `start_harness_session` attached, without a bespoke fake per test.
    mcp_servers: Arc<Mutex<Option<Vec<crate::domain::McpServerSpec>>>>,
}

struct FakeHarness {
    block_until_cancel: bool,
    counters: Counters,
}

#[async_trait]
impl Harness for FakeHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        self.counters.starts.fetch_add(1, Ordering::SeqCst);
        *self.counters.mcp_servers.lock().unwrap() = Some(ctx.mcp_servers.clone());
        Ok(Box::new(FakeSession {
            store: ctx.store.clone(),
            events: ctx.events.clone(),
            session_pk: ctx.session_pk.clone(),
            block_until_cancel: self.block_until_cancel,
            cancelled: Arc::new(AtomicBool::new(false)),
            send_count: self.counters.sends.clone(),
            ended: self.counters.ended.clone(),
            prompts: self.counters.prompts.clone(),
        }))
    }
}

struct FakeHarnessFactory {
    block_until_cancel: bool,
    counters: Counters,
}

impl HarnessFactory for FakeHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(FakeHarness {
            block_until_cancel: self.block_until_cancel,
            counters: self.counters.clone(),
        }))
    }
}

/// A harness whose `start_session` parks until `release` is notified —
/// freezes a session's background startup at the harness phase so a test can
/// stop it deterministically mid-startup.
struct GatedHarness {
    release: Arc<tokio::sync::Notify>,
    counters: Counters,
}

#[async_trait]
impl Harness for GatedHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        self.release.notified().await;
        self.counters.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSession {
            store: ctx.store.clone(),
            events: ctx.events.clone(),
            session_pk: ctx.session_pk.clone(),
            block_until_cancel: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            send_count: self.counters.sends.clone(),
            ended: self.counters.ended.clone(),
            prompts: self.counters.prompts.clone(),
        }))
    }
}

struct GatedHarnessFactory {
    release: Arc<tokio::sync::Notify>,
    counters: Counters,
}

impl HarnessFactory for GatedHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(GatedHarness {
            release: self.release.clone(),
            counters: self.counters.clone(),
        }))
    }
}

/// Like `GatedHarness`, but the gate is a latch that releases ALL waiters
/// (current *and* future) once `open` flips true, and `starts` is incremented
/// on EVERY `start_session` attempt *before* the gate. A follow-up issued
/// while startup is in flight must NOT cold-resume into a second harness, so
/// counting each attempt up-front lets a test assert `starts == 1` even while
/// everyone is still parked (a cold-resume shows `2` immediately).
struct LatchGatedHarness {
    open: Arc<AtomicBool>,
    counters: Counters,
}

#[async_trait]
impl Harness for LatchGatedHarness {
    async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        self.counters.starts.fetch_add(1, Ordering::SeqCst);
        while !self.open.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        Ok(Box::new(FakeSession {
            store: ctx.store.clone(),
            events: ctx.events.clone(),
            session_pk: ctx.session_pk.clone(),
            block_until_cancel: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            send_count: self.counters.sends.clone(),
            ended: self.counters.ended.clone(),
            prompts: self.counters.prompts.clone(),
        }))
    }
}

struct LatchGatedHarnessFactory {
    open: Arc<AtomicBool>,
    counters: Counters,
}

impl HarnessFactory for LatchGatedHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(LatchGatedHarness {
            open: self.open.clone(),
            counters: self.counters.clone(),
        }))
    }
}

/// Build a `Registries` with a `native` harness backed by the fake.
fn registries(block_until_cancel: bool) -> Registries {
    registries_with(block_until_cancel, Counters::default())
}

/// Like `registries`, but sharing `counters` so a test can inspect how many
/// times the harness started a session / drove a prompt / ended.
fn registries_with(block_until_cancel: bool, counters: Counters) -> Registries {
    let mut regs = Registries::new();
    regs.harness = Arc::new(FakeHarnessFactory {
        block_until_cancel,
        counters,
    });
    regs
}

/// Init a git repo with one commit (worktrees need a HEAD commit).
fn init_repo(dir: &std::path::Path) {
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

/// Add `name` with `content` to the index and commit on the current HEAD —
/// dirty-tree tests need a TRACKED file to modify.
fn commit_file(repo_dir: &std::path::Path, name: &str, content: &str) {
    let repo = git2::Repository::open(repo_dir).unwrap();
    std::fs::write(repo_dir.join(name), content).unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new(name)).unwrap();
    idx.write().unwrap();
    let tree_id = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "add file", &tree, &parents)
        .unwrap();
}

/// A ControlPlane whose fake harness IS the only harness (the single
/// `Registries.harness` slot), so these tests don't care which harness id
/// `connect_project` assigns.
async fn fake_control_plane_any_harness() -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile)
{
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let mut regs = Registries::new();
    regs.harness = Arc::new(FakeHarnessFactory {
        block_until_cancel: false,
        counters: counters.clone(),
    });
    let cp = ControlPlane::new(store, regs).await;
    let store_ref = cp.store.clone();
    (cp, store_ref, db_guard)
}

/// A fresh sqlite path backed by a `NamedTempFile` guard. The caller must
/// keep the returned guard alive for as long as the path is in use —
/// dropping it deletes the file — mirroring the inline
/// `let tmp = tempfile::NamedTempFile::new()...` pattern `store.rs`'s
/// tests use directly, instead of leaking a `.keep()`ed file into /tmp.
fn temp_db_path() -> (tempfile::NamedTempFile, std::path::PathBuf) {
    let f = tempfile::NamedTempFile::new().unwrap();
    let path = f.path().to_path_buf();
    (f, path)
}

/// A `HarnessFactory` whose `create()` always fails — used to exercise
/// the cold-resume rollback path in `continue_session`.
struct FailingHarnessFactory;
impl HarnessFactory for FailingHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Err(anyhow::anyhow!("boom: harness factory intentionally fails"))
    }
}

/// Build a `ControlPlane` wired to the shared-counter fake harness, plus
/// a clone of its internal `Store` (for seeding/asserting session state
/// directly), the shared prompt log (for asserting exactly what text
/// was driven on a resumed session), and the sqlite temp-file guard the
/// caller must keep alive for the test's duration.
async fn fake_control_plane() -> (
    Arc<ControlPlane>,
    Arc<Store>,
    Arc<Mutex<Vec<String>>>,
    tempfile::NamedTempFile,
) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new(store, registries_with(false, counters.clone())).await;
    let store_ref = cp.store.clone();
    (cp, store_ref, counters.prompts, db_guard)
}

/// Like `fake_control_plane`, but wired via `new_with_telemetry` with an
/// injected telemetry backend — for tests asserting on emitted spans/counts.
async fn fake_control_plane_with_telemetry(
    telemetry: Arc<dyn crate::telemetry::Telemetry>,
) -> (
    Arc<ControlPlane>,
    Arc<Store>,
    Arc<Mutex<Vec<String>>>,
    tempfile::NamedTempFile,
) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new_with_telemetry(
        Arc::new(store),
        registries_with(false, counters.clone()),
        telemetry,
    )
    .await;
    let store_ref = cp.store().clone();
    (cp, store_ref, counters.prompts, db_guard)
}

/// A fake `AttachmentFetcher`: returns the configured bytes for a known
/// URL, or a 404 for anything else — no real network I/O, for
/// `prepare_attachments` tests.
struct FakeAttachmentFetcher {
    bodies: std::collections::HashMap<String, Vec<u8>>,
}

impl FakeAttachmentFetcher {
    fn new(bodies: impl IntoIterator<Item = (&'static str, &'static [u8])>) -> Self {
        Self {
            bodies: bodies
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_vec()))
                .collect(),
        }
    }
}

impl crate::attachments::AttachmentFetcher for FakeAttachmentFetcher {
    fn fetch_capped(
        &self,
        url: &str,
        _max_bytes: u64,
    ) -> anyhow::Result<crate::attachments::FetchOutcome> {
        match self.bodies.get(url) {
            Some(bytes) => Ok(crate::attachments::FetchOutcome::Ok(bytes.clone())),
            None => Ok(crate::attachments::FetchOutcome::HttpError(404)),
        }
    }
}

/// Like `fake_control_plane`, but wired via `new_full` with an injected
/// attachment fetcher — for `prepare_attachments` tests that must not hit
/// the real network.
async fn fake_control_plane_with_fetcher(
    fetcher: Arc<dyn crate::attachments::AttachmentFetcher>,
) -> (
    Arc<ControlPlane>,
    Arc<Store>,
    Arc<Mutex<Vec<String>>>,
    tempfile::NamedTempFile,
) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new_full(
        Arc::new(store),
        registries_with(false, counters.clone()),
        Arc::new(NoopTelemetry),
        fetcher,
    )
    .await;
    let store_ref = cp.store().clone();
    (cp, store_ref, counters.prompts, db_guard)
}

/// A `Console` sink that captures every emitted JSON line, for telemetry
/// assertions below.
fn capturing_console_telemetry() -> (
    Arc<Mutex<Vec<String>>>,
    Arc<dyn crate::telemetry::Telemetry>,
) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let captured = lines.clone();
    let telemetry = crate::telemetry::ConsoleTelemetry::with_sink(
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

#[tokio::test]
#[serial]
async fn start_session_emits_session_run_count_and_harness_run_span() {
    let _guard = StateDirGuard::new();
    let (lines, telemetry) = capturing_console_telemetry();
    let (cp, _store, _prompts, _db_guard) = fake_control_plane_with_telemetry(telemetry).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // Background startup + the prompt turn finish asynchronously — poll for
    // the harness.run span instead of racing a fixed sleep.
    let mut parsed = parse_telemetry_lines(&lines);
    for _ in 0..400 {
        if parsed
            .iter()
            .any(|v| v["kind"] == "span" && v["name"] == "harness.run")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        parsed = parse_telemetry_lines(&lines);
    }
    assert!(
        parsed
            .iter()
            .any(|v| v["kind"] == "count" && v["name"] == "session.run"),
        "expected a session.run count line, got: {parsed:?}"
    );
    let span = parsed
        .iter()
        .find(|v| v["kind"] == "span" && v["name"] == "harness.run")
        .unwrap_or_else(|| panic!("expected a harness.run span line, got: {parsed:?}"));
    assert_eq!(span["attrs"]["session_pk"], session.session_pk);
    assert!(span["durationMs"].is_number());
    assert!(span.get("error").is_none());
}

#[tokio::test]
async fn resolve_approval_counts_allow_and_deny() {
    let (lines, telemetry) = capturing_console_telemetry();
    let (cp, _store, _prompts, _db_guard) = fake_control_plane_with_telemetry(telemetry).await;

    cp.resolve_approval_bool("req-allow", true);
    cp.resolve_approval_bool("req-deny", false);

    let parsed = parse_telemetry_lines(&lines);
    assert!(
        parsed
            .iter()
            .any(|v| v["kind"] == "count" && v["name"] == "approval.allow"),
        "expected an approval.allow count line, got: {parsed:?}"
    );
    assert!(
        parsed
            .iter()
            .any(|v| v["kind"] == "count" && v["name"] == "approval.deny"),
        "expected an approval.deny count line, got: {parsed:?}"
    );
}

#[tokio::test]
async fn resolve_approval_delegates_the_structured_response() {
    let (cp, _store, _prompts, _db_guard) = fake_control_plane().await;
    let rx = cp.approvals.register("req-structured".into());

    let resolved = cp.resolve_approval(
        "req-structured",
        ApprovalResponse {
            decision: ApprovalDecision::AllowAlways,
            scope: Some(ApprovalScope::Session),
            payload: None,
        },
    );

    assert!(resolved, "a registered request must resolve");
    let response = rx.await.unwrap();
    assert_eq!(response.decision, ApprovalDecision::AllowAlways);
    assert_eq!(response.scope, Some(ApprovalScope::Session));
    assert!(response.allowed());
}

/// Like `fake_control_plane`, but the registered harness always fails to
/// start — for testing the cold-resume rollback in `continue_session`.
async fn control_plane_with_failing_factory(
) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let mut regs = Registries::new();
    regs.harness = Arc::new(FailingHarnessFactory);
    let cp = ControlPlane::new(store, regs).await;
    let store_ref = cp.store.clone();
    (cp, store_ref, db_guard)
}

/// Seed a minimal project row (bypassing `connect_project`'s git-repo
/// requirement, which reconcile/resume tests don't need).
async fn seed_project(store: &Store, project_id: &str) {
    store
        .insert_project(Project {
            project_id: project_id.to_string(),
            name: "demo".into(),
            workdir: "/tmp/demo".into(),
            source: None,
            harness: "native".into(),
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(now_ms()),
            is_git: false,
        })
        .await
        .unwrap();
}

/// Seed a session directly at a given status/agent_session_id/resume_attempts.
async fn seed_session(
    store: &Store,
    session_pk: &str,
    project_id: &str,
    status: SessionStatus,
    agent_session_id: Option<&str>,
    resume_attempts: i64,
) {
    let now = now_ms();
    store
        .insert_session(Session {
            session_pk: session_pk.to_string(),
            project_id: project_id.to_string(),
            agent_session_id: agent_session_id.map(|s| s.to_string()),
            worktree_path: None,
            branch: None,
            title: Some("seed".into()),
            status,
            started_by: Some("test".into()),
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: true,
        })
        .await
        .unwrap();
    store
        .update_resume(session_pk, status, resume_attempts)
        .await
        .unwrap();
}

/// Poll the shared prompt log until it has at least `n` entries (or panic
/// after a timeout), then give the fire-and-forget background prompt task
/// (spawned by `resume_session` via `spawn_prompt`, which isn't joinable)
/// a brief grace period to finish its trailing store writes — persisting
/// the turn's messages and `demote_if_running` — before the caller
/// asserts on session/store state.
async fn wait_for_prompts(log: &Arc<Mutex<Vec<String>>>, n: usize) {
    for _ in 0..400 {
        if log.lock().unwrap().len() >= n {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "timed out waiting for {n} prompt(s); got {}",
        log.lock().unwrap().len()
    );
}

/// Poll the `running` map until background startup registers the session's
/// live handle — i.e. startup completed (git prep + harness start done).
async fn wait_for_running_handle(
    cp: &Arc<ControlPlane>,
    session_pk: &str,
) -> Arc<dyn HarnessSession> {
    for _ in 0..400 {
        if let Some(h) = cp.running.lock().unwrap().get(session_pk).cloned() {
            return h;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for the live handle of {session_pk}");
}

/// Poll the session's persisted messages until one matches `pred`.
async fn wait_for_message(
    store: &Store,
    session_pk: &str,
    pred: impl Fn(&Message) -> bool,
) -> Message {
    for _ in 0..400 {
        if let Some(m) = store
            .list_messages(session_pk)
            .await
            .unwrap()
            .into_iter()
            .find(|m| pred(m))
        {
            return m;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for a matching message in {session_pk}");
}

/// Background startup builds the SessionCtx asynchronously — poll until the
/// fake harness has captured it.
async fn wait_for_session_ctx(counters: &Counters) -> Vec<crate::domain::McpServerSpec> {
    for _ in 0..400 {
        if let Some(servers) = counters.mcp_servers.lock().unwrap().clone() {
            return servers;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for the harness SessionCtx");
}

#[tokio::test]
async fn connect_project_defaults_to_the_native_harness() {
    let (cp, _store, _log, _db_guard) = fake_control_plane().await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    assert_eq!(project.harness, "native");
}

#[tokio::test]
async fn connect_project_on_plain_folder_succeeds_with_is_git_false() {
    let (cp, store, _db_guard) = provisioning_control_plane().await;

    // Deliberately NOT a git repo.
    let dir = tempfile::tempdir().unwrap();
    let project = cp.connect_project(dir.path(), "plain").await.unwrap();
    assert!(!project.is_git);
    let got = store
        .get_project(&project.project_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!got.is_git, "read-time recompute must also say non-git");

    // A real repo still connects with is_git=true.
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let gitty = cp.connect_project(repo_dir.path(), "gitty").await.unwrap();
    assert!(gitty.is_git);
}

#[tokio::test]
async fn start_session_on_non_git_project_skips_workspace_prep() {
    let (cp, store, _db_guard) = fake_control_plane_any_harness().await;

    // A plain folder — no repo, so no branch and no worktree.
    let dir = tempfile::tempdir().unwrap();
    let project = cp.connect_project(dir.path(), "plain").await.unwrap();
    assert!(!project.is_git);

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    assert_eq!(session.branch, None, "non-git sessions carry no branch");
    assert_eq!(session.worktree_path, None);
    assert!(!session.branch_owned);

    // Wait for background startup to register the live handle (git prep is
    // skipped, so the harness starts directly in project.workdir), then confirm
    // the persisted row kept the same no-branch/no-worktree shape.
    wait_for_running_handle(&cp, &session.session_pk).await;
    let got = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.branch, None);
    assert_eq!(got.worktree_path, None);
}

#[tokio::test]
#[serial]
async fn stop_immediately_after_start_is_registered() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    // A harness whose session blocks until cancelled, so it stays Running.
    let cp = ControlPlane::new(store, registries(true)).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // Startup now runs in the background — wait for the live handle, then
    // stop; the stop must reach the live session's cancel(). (A stop that
    // lands DURING startup is covered by stop_during_startup_cancels_cleanly.)
    wait_for_running_handle(&cp, &session.session_pk).await;
    cp.stop_session(&session.session_pk).await.unwrap();

    // Give the background task a moment to observe cancellation and exit.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    assert_eq!(stored[0].status, crate::domain::SessionStatus::Interrupted);

    // Under the long-lived-session model the handle intentionally PERSISTS
    // after stop — it is the live session, kept to serve future prompts and
    // torn down only by `end_session`.
    assert!(
        cp.running.lock().unwrap().contains_key(&session.session_pk),
        "stop must NOT drop the live-session handle"
    );

    // end_session is the one place that removes + ends the handle.
    cp.end_session(&session.session_pk).await.unwrap();
    assert!(
        !cp.running.lock().unwrap().contains_key(&session.session_pk),
        "end_session must remove the handle from the running map"
    );
}

#[tokio::test]
#[serial]
async fn stop_session_denies_this_sessions_parked_approvals_only() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    // A harness whose session blocks until cancelled, so the session stays
    // Running with a live handle.
    let cp = ControlPlane::new(store, registries(true)).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    wait_for_running_handle(&cp, &session.session_pk).await;

    // Two approvals parked for this session, one for an unrelated session.
    let rx_a = cp
        .approvals
        .register_for_session(&session.session_pk, "tool-a".into());
    let rx_b = cp
        .approvals
        .register_for_session(&session.session_pk, "tool-b".into());
    let rx_other = cp
        .approvals
        .register_for_session("some-other-session", "tool-c".into());

    cp.stop_session(&session.session_pk).await.unwrap();

    assert!(
        !rx_a.await.unwrap().allowed(),
        "stop must deny this session's parked approval"
    );
    assert!(
        !rx_b.await.unwrap().allowed(),
        "stop must deny this session's parked approval"
    );
    // The unrelated session's approval is untouched and still resolvable.
    assert!(cp.resolve_approval_bool("tool-c", true));
    assert!(rx_other.await.unwrap().allowed());
}

#[tokio::test]
#[serial]
async fn continue_reuses_the_live_session() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let counters = Counters::default();
    // Non-blocking session so each prompt turn completes and the handle
    // stays parked in `running` for reuse on the next turn.
    let cp = ControlPlane::new(store, registries_with(false, counters.clone())).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    // First turn: creates the live ACP session and drives one prompt.
    let session = cp
        .start_session(&project.project_id, "first", "test", &[])
        .await
        .unwrap();
    // Let the background prompt task finish its turn.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Snapshot the live handle so we can prove the SAME one is reused.
    let handle_before = wait_for_running_handle(&cp, &session.session_pk).await;

    // Second turn: MUST reuse the live handle — no new ACP session.
    cp.continue_session(&session.session_pk, "second", &[])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let handle_after = cp
        .running
        .lock()
        .unwrap()
        .get(&session.session_pk)
        .cloned()
        .expect("continue_session must keep the live handle registered");

    // Same Arc → the live session was reused, not replaced.
    assert!(
        Arc::ptr_eq(&handle_before, &handle_after),
        "continue_session must reuse the live handle, not create a new one"
    );
    // A NEW session was started exactly ONCE (only on start_session); the
    // follow-up turn did NOT spawn a fresh adapter / session/load.
    assert_eq!(
        counters.starts.load(Ordering::SeqCst),
        1,
        "only start_session should create an ACP session"
    );
    // send_prompt was driven twice on that one live session.
    assert_eq!(
        counters.sends.load(Ordering::SeqCst),
        2,
        "both turns must run on the same live session"
    );
}

#[tokio::test]
#[serial]
async fn continue_cold_resumes_when_handle_absent() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new(store, registries_with(false, counters.clone())).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "first", "test", &[])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Simulate an app restart that wiped the in-memory running map (after
    // background startup registered the handle).
    wait_for_running_handle(&cp, &session.session_pk).await;
    cp.running.lock().unwrap().remove(&session.session_pk);

    // Continue must fall back to the cold-resume path: start a FRESH session
    // (via session/load) and register a new live handle.
    cp.continue_session(&session.session_pk, "second", &[])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(
        cp.running.lock().unwrap().contains_key(&session.session_pk),
        "cold resume must re-register a live handle"
    );
    // Two ACP sessions created total: the initial start + the cold resume.
    assert_eq!(counters.starts.load(Ordering::SeqCst), 2);
    assert_eq!(counters.sends.load(Ordering::SeqCst), 2);
}

/// Factory that works for the first session but fails every later create —
/// models "the adapter can't come back up" for cold-resume paths.
struct FailingResumeFactory {
    counters: Counters,
}

impl HarnessFactory for FailingResumeFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        if self.counters.starts.load(Ordering::SeqCst) >= 1 {
            anyhow::bail!("adapter unavailable");
        }
        Ok(Arc::new(FakeHarness {
            block_until_cancel: false,
            counters: self.counters.clone(),
        }))
    }
}

#[tokio::test]
async fn failed_cold_resume_rolls_back_the_running_status() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let counters = Counters::default();
    let mut regs = Registries::new();
    regs.harness = Arc::new(FailingResumeFactory {
        counters: counters.clone(),
    });
    let cp = ControlPlane::new(store, regs).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "first", "test", &[])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Simulate a restart, then a resume whose harness can't start.
    wait_for_running_handle(&cp, &session.session_pk).await;
    cp.running.lock().unwrap().remove(&session.session_pk);
    let err = cp
        .continue_session(&session.session_pk, "second", &[])
        .await
        .expect_err("resume must fail when the harness can't start");
    assert!(err.to_string().contains("adapter unavailable"));

    // The optimistic Running write must be rolled back — a wedged
    // "running" session with no live handle is unrecoverable in the UI.
    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    assert_ne!(stored[0].status, crate::domain::SessionStatus::Running);
}

#[tokio::test]
async fn end_session_clears_the_stale_worktree_path() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let cp = ControlPlane::new(store, registries(false)).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // The worktree path is backfilled by background startup — wait for
    // startup to complete, then read the stored row.
    wait_for_running_handle(&cp, &session.session_pk).await;
    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    assert!(stored[0].worktree_path.is_some());

    cp.end_session(&session.session_pk).await.unwrap();
    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    // The path is forgotten, so a later continue cold-resumes into the
    // project workdir instead of the deleted directory.
    assert_eq!(stored[0].worktree_path, None);
    assert_eq!(stored[0].branch, None);
}

#[tokio::test]
#[serial]
async fn end_session_removes_and_ends_the_handle() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new(store, registries_with(false, counters.clone())).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    wait_for_running_handle(&cp, &session.session_pk).await;

    cp.end_session(&session.session_pk).await.unwrap();

    assert!(
        !cp.running.lock().unwrap().contains_key(&session.session_pk),
        "end_session must remove the handle"
    );
    assert!(
        counters.ended.load(Ordering::SeqCst),
        "end_session must call end() on the live handle"
    );
    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    assert_eq!(stored[0].status, crate::domain::SessionStatus::Ended);
}

#[tokio::test]
#[serial]
async fn start_session_streams_events_and_records_agent_id() {
    let _guard = StateDirGuard::new();
    let db = tempfile::NamedTempFile::new().unwrap();
    let store = crate::store::Store::open(db.path()).await.unwrap();
    let cp = ControlPlane::new(store, registries(false)).await;

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let mut rx = cp.subscribe();
    let session = cp
        .start_session(&project.project_id, "do it", "test", &[])
        .await
        .unwrap();

    // Drain events until Result, collecting assistant text payloads.
    let mut texts = Vec::new();
    loop {
        match rx.recv().await.unwrap() {
            CoreEvent::Message {
                role,
                block_type,
                payload,
                ..
            } if role == "assistant" && block_type == "text" => {
                texts.push(payload["text"].as_str().unwrap_or("").to_string());
            }
            CoreEvent::Result { .. } => break,
            _ => {}
        }
    }
    assert!(texts.contains(&"working".to_string()));

    let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].agent_session_id.as_deref(), Some("agent-1"));
    assert_eq!(session.status, crate::domain::SessionStatus::Running);
    // On completion the background task demotes Running → Idle.
    assert_eq!(stored[0].status, crate::domain::SessionStatus::Idle);

    // History is durable: the user prompt + streamed assistant text persist
    // in order. Startup progress rows (system/status) now precede the turn —
    // the user prompt must still be the first non-status row.
    let msgs = cp.list_messages(&session.session_pk).await.unwrap();
    let first_turn = msgs
        .iter()
        .find(|m| m.block_type != "status")
        .expect("expected a non-status row");
    assert_eq!(
        (first_turn.role.as_str(), first_turn.block_type.as_str()),
        ("user", "text")
    );
    assert_eq!(first_turn.payload["text"], "do it");
    assert!(msgs.iter().any(|m| m.role == "assistant"
        && m.block_type == "text"
        && m.payload["text"] == "working"));
    // seq is monotonic and matches insertion order.
    assert!(msgs.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[tokio::test]
#[serial]
async fn start_returns_the_session_before_workspace_prep_and_backfills_it() {
    let _guard = StateDirGuard::new();
    let (cp, store, _prompts, _db_guard) = fake_control_plane().await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // The row is returned BEFORE git prep: workspace columns are provisional.
    assert_eq!(
        session.worktree_path, None,
        "provisional row: no worktree yet"
    );
    assert_eq!(
        session.branch, None,
        "provisional row: engine name unknown yet"
    );
    assert_eq!(session.status, SessionStatus::Running);

    // Background prep backfills the workspace columns…
    wait_for_running_handle(&cp, &session.session_pk).await;
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.worktree_path.is_some(), "worktree path backfilled");
    let branch = stored.branch.clone().unwrap();
    assert!(branch.starts_with("harness/"), "got: {branch}");
    assert!(stored.branch_owned);

    // …and the progress rows landed in order.
    let msgs = store.list_messages(&session.session_pk).await.unwrap();
    let statuses: Vec<String> = msgs
        .iter()
        .filter(|m| m.block_type == "status")
        .map(|m| m.payload["summary"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(statuses[0], "Creating worktree…");
    assert!(
        statuses[1].starts_with("Created and checked out branch harness/"),
        "got: {statuses:?}"
    );
    assert_eq!(statuses[2], "Connecting tools…");
}

#[tokio::test]
#[serial]
async fn git_prep_failure_emits_a_transcript_error_and_keeps_the_session() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db) = fake_control_plane_any_harness().await;
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    commit_file(repo_dir.path(), "a.txt", "one");
    let project = cp.connect_project(repo_dir.path(), "demo").await.unwrap();
    // Unstaged modification to a tracked file = dirty → in-place prep refuses.
    std::fs::write(repo_dir.path().join("a.txt"), "changed").unwrap();
    // Subscribe BEFORE starting: the orchestrator's and scheduler's session
    // watchers finish only on a bus-terminal Result/Error for the session,
    // so a startup failure MUST broadcast CoreEvent::Error — without it they
    // hang to their 2h deadline instead of reporting the real git error.
    let mut rx = cp.subscribe();

    let session = cp
        .start_session_with_prompt(
            &project.project_id,
            TurnPrompt::text("go", "go"),
            "test",
            &[],
            Some(git_opts(false, true, None, None)),
        )
        .await
        .expect("start must succeed; git errors surface in the transcript");

    let err_row = wait_for_message(&store, &session.session_pk, |m| m.block_type == "error").await;
    let message = err_row.payload["message"].as_str().unwrap_or("");
    assert!(message.contains("uncommitted changes"), "got: {message}");

    // The bus-terminal Error reached subscribers with the real git error.
    let bus_message = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(CoreEvent::Error {
                session_pk: pk,
                message,
            })) if pk == session.session_pk => break message,
            Ok(Ok(_)) => continue,
            other => panic!("expected CoreEvent::Error on the bus, got: {other:?}"),
        }
    };
    assert!(
        bus_message.contains("uncommitted changes"),
        "got: {bus_message}"
    );

    // The session persists and is released back to Idle for a retry.
    let mut stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    for _ in 0..400 {
        if stored.status == SessionStatus::Idle {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        stored = store
            .get_session(&session.session_pk)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(stored.status, SessionStatus::Idle);
}

#[tokio::test]
#[serial]
async fn stop_during_startup_cancels_cleanly() {
    let _guard = StateDirGuard::new();
    let (_db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut regs = Registries::new();
    regs.harness = Arc::new(GatedHarnessFactory {
        release: release.clone(),
        counters: counters.clone(),
    });
    let cp = ControlPlane::new(store, regs).await;
    let store = cp.store().clone();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // Git prep is done once "Connecting tools…" lands; the harness is parked
    // on the gate — a genuinely mid-startup stop.
    wait_for_message(&store, &session.session_pk, |m| {
        m.block_type == "status" && m.payload["summary"] == "Connecting tools…"
    })
    .await;

    cp.stop_session(&session.session_pk).await.unwrap();
    release.notify_one();

    // The startup task must observe the cancellation, deregister its token,
    // and finish WITHOUT driving the first prompt.
    for _ in 0..400 {
        if cp.starting.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        cp.starting.lock().unwrap().is_empty(),
        "the startup task must deregister its cancellation token"
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        counters.sends.load(Ordering::SeqCst),
        0,
        "a stopped startup must never drive the first prompt"
    );
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, SessionStatus::Interrupted);
}

// Task 7.2 review fix: `startup_phases`' only pre-harness cancel checkpoint
// used to live INSIDE the `if project.is_git` branch (right after git prep),
// so a non-git session's `else` branch fell straight through to
// `emit_status("Connecting tools…")` + `start_harness_session` with no
// cancel check at all — a stop landing during a non-git startup still spawned
// its harness, unlike a git session with identical timing (caught at the
// git-prep checkpoint). The fix adds an unconditional checkpoint common to
// both paths, right before "Connecting tools…" is emitted.
//
// This can't be pinned via `stop_session()` racing the background task like
// `stop_during_startup_cancels_cleanly` above: the non-git path has NO
// `.await` between registering the cancellation token (in
// `run_session_startup`) and evaluating the new checkpoint, so on this
// crate's current-thread `#[tokio::test]` runtime there is no scheduling
// opportunity for a concurrent `stop_session()` call to ever land inside that
// window — by the time the background task is observably registered, it has
// already run the checkpoint (current-thread tasks run to completion of
// their synchronous prefix, uninterrupted, until their first real
// `Poll::Pending`). Driving `startup_phases` directly with an
// already-cancelled token is the only deterministic way to test it.
#[tokio::test]
#[serial]
async fn non_git_startup_cancelled_before_it_begins_never_starts_the_harness() {
    let _guard = StateDirGuard::new();
    let (_db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let cp = ControlPlane::new(store, registries_with(false, counters.clone())).await;
    let store = cp.store().clone();
    let dir = tempfile::tempdir().unwrap(); // plain temp dir — no git init.
    let project = cp.connect_project(dir.path(), "demo").await.unwrap();
    assert!(!project.is_git);

    // Seed a session row the way `start_session_with_prompt` does, then drive
    // `startup_phases` directly instead of going through the normal
    // `start_session` spawn (see the comment above for why).
    let session_pk = crate::paths::new_id();
    let session = Session {
        session_pk: session_pk.clone(),
        project_id: project.project_id.clone(),
        agent_session_id: None,
        worktree_path: None,
        branch: None,
        title: Some("go".to_string()),
        status: SessionStatus::Running,
        started_by: Some("test".to_string()),
        created_at: Some(now_ms()),
        last_active: Some(now_ms()),
        resume_attempts: 0,
        branch_owned: false,
    };
    store.insert_session(session).await.unwrap();

    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    cp.startup_phases(
        &project,
        &session_pk,
        crate::domain::SessionGitOptions::default(),
        TurnPrompt::text("go", "go"),
        Vec::new(),
        &cancel,
    )
    .await;

    assert_eq!(
        counters.starts.load(Ordering::SeqCst),
        0,
        "a non-git startup already cancelled before it begins must never start the harness"
    );
    assert_eq!(counters.sends.load(Ordering::SeqCst), 0);
    let msgs = store.list_messages(&session_pk).await.unwrap();
    assert!(
        msgs.is_empty(),
        "no status row should be emitted once startup was already cancelled; got: {msgs:?}"
    );
}

#[tokio::test]
#[serial]
async fn end_during_startup_waits_for_the_startup_task_and_cleans_the_worktree() {
    let _guard = StateDirGuard::new();
    let (_db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut regs = Registries::new();
    regs.harness = Arc::new(GatedHarnessFactory {
        release: release.clone(),
        counters: counters.clone(),
    });
    let cp = ControlPlane::new(store, regs).await;
    let store = cp.store().clone();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();
    // Git prep is done (workspace columns backfilled) once "Connecting
    // tools…" lands; the harness is parked on the gate — startup in flight.
    wait_for_message(&store, &session.session_pk, |m| {
        m.block_type == "status" && m.payload["summary"] == "Connecting tools…"
    })
    .await;
    let wt = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap()
        .worktree_path
        .expect("git prep backfilled the worktree path");
    assert!(std::path::Path::new(&wt).exists());

    // End while startup is parked. end_session must WAIT for the startup
    // task to unwind before tearing down — otherwise an end that races git
    // prep reads provisional workspace columns and leaks the worktree.
    let ender = {
        let cp = Arc::clone(&cp);
        let pk = session.session_pk.clone();
        tokio::spawn(async move { cp.end_session(&pk).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !ender.is_finished(),
        "end_session must wait for the in-flight startup task"
    );

    release.notify_one();
    ender.await.unwrap().unwrap();
    assert!(
        !std::path::Path::new(&wt).exists(),
        "the worktree created during startup must be removed by end_session"
    );
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, SessionStatus::Ended);
    assert_eq!(stored.worktree_path, None, "workspace columns cleared");
}

// Scope addition (Task 6.3 review): a follow-up that arrives while a session
// is still in background startup must NOT take the cold-resume path — that
// would spawn a SECOND harness (in project.workdir while worktree_path is
// still provisional), run the follow-up ahead of the first prompt, and orphan
// the handle the startup task later registers. It must WAIT for startup to
// land its live handle, then drive the follow-up on THAT handle.
#[tokio::test]
#[serial]
async fn continue_during_startup_waits_and_reuses_the_startup_handle() {
    let _guard = StateDirGuard::new();
    let (_db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let open = Arc::new(AtomicBool::new(false));
    let mut regs = Registries::new();
    regs.harness = Arc::new(LatchGatedHarnessFactory {
        open: open.clone(),
        counters: counters.clone(),
    });
    let cp = ControlPlane::new(store, regs).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(&project.project_id, "first", "test", &[])
        .await
        .unwrap();
    // Wait until the startup task has reached the parked harness phase — its
    // one and only `start_session` attempt is now counted and blocked on the
    // latch, and the session is still registered as in-flight.
    for _ in 0..400 {
        if counters.starts.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(counters.starts.load(Ordering::SeqCst), 1);
    assert!(
        cp.starting
            .lock()
            .unwrap()
            .contains_key(&session.session_pk),
        "startup must still be in flight"
    );

    // Issue the follow-up while startup is parked.
    let cont = {
        let cp = Arc::clone(&cp);
        let pk = session.session_pk.clone();
        tokio::spawn(async move { cp.continue_session(&pk, "second", &[]).await })
    };
    // Give a buggy cold-resume time to spawn its second harness in the main
    // checkout. The follow-up must instead be parked waiting for startup.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !cont.is_finished(),
        "continue must wait for the in-flight startup, not race ahead"
    );
    assert_eq!(
        counters.starts.load(Ordering::SeqCst),
        1,
        "continue must NOT cold-resume a second harness while startup is in flight"
    );

    // Release: startup registers its live handle, drives the first prompt, and
    // deregisters; the parked continue then lands on that same handle.
    open.store(true, Ordering::SeqCst);
    cont.await.unwrap().unwrap();

    assert_eq!(
        counters.starts.load(Ordering::SeqCst),
        1,
        "the follow-up must reuse the startup's live handle, not start a new one"
    );
    wait_for_prompts(&counters.prompts, 2).await;
    let prompts = counters.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2, "both prompts drove on the one handle");
    assert!(
        prompts.contains(&"first".to_string()) && prompts.contains(&"second".to_string()),
        "both the startup prompt and the follow-up ran; got: {prompts:?}"
    );
}

#[tokio::test]
async fn reconcile_resumes_running_session_with_nudge_and_increments_attempts() {
    let (cp, store, prompt_log, _db_guard) = fake_control_plane().await;
    seed_project(&store, "p1").await;
    seed_session(
        &store,
        "s1",
        "p1",
        SessionStatus::Running,
        Some("acp-123"),
        0,
    )
    .await;

    cp.reconcile().await.unwrap();
    wait_for_prompts(&prompt_log, 1).await;

    assert_eq!(prompt_log.lock().unwrap()[0], RESUME_NUDGE);

    // The fire-and-forget prompt task's trailing `demote_if_running` write
    // (Idle + resume_attempts=0, one UPDATE) lands after the prompt is
    // logged; the fixed grace period in `wait_for_prompts` can lose that
    // race under parallel test load, so poll for the demote itself.
    let mut s = store.get_session("s1").await.unwrap().unwrap();
    for _ in 0..400 {
        if s.status == SessionStatus::Idle {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        s = store.get_session("s1").await.unwrap().unwrap();
    }
    // the resumed turn completed via the fake → demote reset attempts to 0
    assert_eq!(s.resume_attempts, 0);
    assert_eq!(s.status, SessionStatus::Idle);
    // the 🔄 status row was persisted
    let msgs = store.list_messages("s1").await.unwrap();
    assert!(msgs
        .iter()
        .any(|m| m.block_type == "status" && m.payload["summary"] == "🔄 Resumed after restart."));
}

#[tokio::test]
async fn resume_without_agent_session_id_goes_idle_with_warning() {
    let (cp, store, _prompt_log, _db_guard) = fake_control_plane().await;
    seed_project(&store, "p1").await;
    seed_session(&store, "s1", "p1", SessionStatus::Running, None, 0).await;

    cp.resume_session("s1", "restart").await.unwrap();

    let s = store.get_session("s1").await.unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Idle);
    let msgs = store.list_messages("s1").await.unwrap();
    assert!(msgs.iter().any(|m| {
        m.payload["summary"]
        == "⚠️ Interrupted by a restart and could not be auto-resumed — send a message to continue."
    }));
}

#[tokio::test]
async fn resume_gives_up_after_three_attempts() {
    let (cp, store, _prompt_log, _db_guard) = fake_control_plane().await;
    seed_project(&store, "p1").await;
    seed_session(
        &store,
        "s1",
        "p1",
        SessionStatus::Running,
        Some("acp-123"),
        3,
    )
    .await;

    cp.resume_session("s1", "restart").await.unwrap();

    let s = store.get_session("s1").await.unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Idle);
    assert_eq!(s.resume_attempts, 3); // untouched
    let msgs = store.list_messages("s1").await.unwrap();
    assert!(msgs.iter().any(|m| m.payload["summary"]
        == "⚠️ Auto-resume gave up after 3 attempts — send a message to continue."));
}

#[tokio::test]
async fn continue_session_cold_resume_failure_rolls_back_to_idle() {
    let (cp, store, _db_guard) = control_plane_with_failing_factory().await;
    seed_project(&store, "p1").await;
    seed_session(&store, "s1", "p1", SessionStatus::Idle, Some("acp-123"), 0).await;

    assert!(cp.continue_session("s1", "hi", &[]).await.is_err());

    let s = store.get_session("s1").await.unwrap().unwrap();
    assert_eq!(
        s.status,
        SessionStatus::Idle,
        "must not be left stuck in Running"
    );
}

/// A `HarnessSession` whose `send_prompt` always fails — models an upstream
/// LLM error (e.g. quota exhaustion) surfacing from the harness turn. Pure
/// in-process fake: spawns no subprocesses.
struct ErrSendSession;

#[async_trait]
impl HarnessSession for ErrSendSession {
    async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
        anyhow::bail!("upstream quota exhausted")
    }
    async fn cancel(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn end(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn agent_session_id(&self) -> Option<String> {
        None
    }
}

struct ErrSendHarness;

#[async_trait]
impl Harness for ErrSendHarness {
    async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
        Ok(Box::new(ErrSendSession))
    }
}

struct ErrSendHarnessFactory;
impl HarnessFactory for ErrSendHarnessFactory {
    fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
        Ok(Arc::new(ErrSendHarness))
    }
}

#[tokio::test]
async fn failed_turn_persists_a_durable_error_row_and_demotes_before_the_bus_error() {
    let (_db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let mut regs = Registries::new();
    regs.harness = Arc::new(ErrSendHarnessFactory);
    let cp = ControlPlane::new(store, regs).await;
    seed_project(&cp.store, "p1").await;
    seed_session(
        &cp.store,
        "s1",
        "p1",
        SessionStatus::Idle,
        Some("acp-123"),
        0,
    )
    .await;

    // Subscribe BEFORE driving the turn so the bus-terminal Error is caught.
    let mut rx = cp.subscribe();
    // Cold-resume succeeds (the factory works) — the failure happens inside
    // the fire-and-forget `spawn_prompt` turn, so this call itself is Ok.
    cp.continue_session("s1", "hi", &[]).await.unwrap();

    // The turn error must be persisted as a DURABLE transcript row
    // (role=system, block_type=error) — today it is broadcast-only and
    // vanishes on app reload.
    let row = wait_for_message(&cp.store, "s1", |m| m.block_type == "error").await;
    assert_eq!(row.role, "system");
    assert_eq!(
        row.payload["message"].as_str().unwrap_or(""),
        "upstream quota exhausted"
    );

    // Bus order: the durable row's Message event precedes the terminal Error
    // (the UI appends the row on Message, then flips status on Error), and by
    // the time Error is observed the DB row is already demoted Running→Idle
    // (a subscriber that refreshes on Error must never read a stale Running).
    let mut saw_error_row = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(CoreEvent::Message {
                session_pk,
                block_type,
                ..
            })) if session_pk == "s1" && block_type == "error" => saw_error_row = true,
            Ok(Ok(CoreEvent::Error {
                session_pk,
                message,
            })) if session_pk == "s1" => {
                assert!(
                    saw_error_row,
                    "durable error row must be broadcast before the terminal Error"
                );
                assert_eq!(message, "upstream quota exhausted");
                break;
            }
            Ok(Ok(_)) => continue,
            other => panic!("expected CoreEvent::Error on the bus, got: {other:?}"),
        }
    }
    let s = cp.store.get_session("s1").await.unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Idle, "must not be stuck Running");
}

// ---------- Task 3: attachments wired into start/continue_session ----------

fn text_attachment(url: &str) -> AttachmentRef {
    AttachmentRef {
        name: "notes.txt".into(),
        url: url.into(),
        content_type: Some("text/plain".into()),
        size: 5,
    }
}

#[tokio::test]
#[serial]
async fn attachments_manifest_is_appended_to_the_prompt_the_harness_receives() {
    let _guard = StateDirGuard::new();
    let fetcher = Arc::new(FakeAttachmentFetcher::new([(
        "https://cdn.discordapp.com/a",
        &b"hello"[..],
    )]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;

    let workdir_root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", workdir_root.path().to_str().unwrap())
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(
            &project.project_id,
            "please review",
            "test",
            &[text_attachment("https://cdn.discordapp.com/a")],
        )
        .await
        .unwrap();
    wait_for_prompts(&prompts, 1).await;

    let dest = workdir_root
        .path()
        .join(".harness-attachments")
        .join(&session.session_pk)
        .join("notes.txt");
    let expected_manifest = format!(
        "[User attached 1 file — saved to disk, use the Read tool to open them:]\n- {} (text/plain, 5 B)",
        dest.display()
    );
    assert_eq!(
        prompts.lock().unwrap()[0],
        format!("please review\n\n{expected_manifest}"),
        "the harness/agent must see the prompt decorated with the attachment manifest"
    );
    assert!(dest.exists(), "attachment must be written to disk");

    // Durable history (the cockpit UI) must show the RAW prompt the user
    // typed — NOT the manifest-decorated text sent to the agent above.
    let msgs = store.list_messages(&session.session_pk).await.unwrap();
    let user_row = msgs
        .iter()
        .find(|m| m.role == "user" && m.block_type == "text")
        .expect("expected a persisted user turn");
    assert_eq!(
        user_row.payload["text"], "please review",
        "the persisted user row must be the raw prompt only, not manifest-decorated"
    );
}

#[tokio::test]
#[serial]
async fn attachment_max_count_zero_disables_attachments() {
    let _guard = StateDirGuard::new();
    let fetcher = Arc::new(FakeAttachmentFetcher::new([(
        "https://cdn.discordapp.com/a",
        &b"hello"[..],
    )]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;
    SettingsStore::new(store.clone())
        .set("attachment_max_count", "0")
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    // Non-empty prompt: passed through unchanged.
    cp.start_session(
        &project.project_id,
        "hi",
        "test",
        &[text_attachment("https://cdn.discordapp.com/a")],
    )
    .await
    .unwrap();
    wait_for_prompts(&prompts, 1).await;
    assert_eq!(prompts.lock().unwrap()[0], "hi");

    // Empty prompt: the disabled-feature text.
    cp.start_session(
        &project.project_id,
        "",
        "test",
        &[text_attachment("https://cdn.discordapp.com/a")],
    )
    .await
    .unwrap();
    wait_for_prompts(&prompts, 2).await;
    assert_eq!(
        prompts.lock().unwrap()[1],
        "User sent attachments, but attachment support is disabled."
    );
}

/// Forces `materialize_attachments` to return `Err` (contract resolution
/// from the Task 2 review) by putting a plain FILE where the per-session
/// dest dir's parent needs to be created, so `create_dir_all` fails.
#[tokio::test]
#[serial]
async fn materialize_error_produces_the_could_not_process_fallback() {
    let _guard = StateDirGuard::new();
    let fetcher = Arc::new(FakeAttachmentFetcher::new([(
        "https://cdn.discordapp.com/a",
        &b"hello"[..],
    )]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;

    let workdir_root = tempfile::tempdir().unwrap();
    std::fs::write(
        workdir_root.path().join(".harness-attachments"),
        b"not a dir",
    )
    .unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", workdir_root.path().to_str().unwrap())
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    cp.start_session(
        &project.project_id,
        "hello",
        "test",
        &[text_attachment("https://cdn.discordapp.com/a")],
    )
    .await
    .unwrap();
    wait_for_prompts(&prompts, 1).await;

    let got = prompts.lock().unwrap()[0].clone();
    assert!(
        got.starts_with("hello\n\n⚠️ Could not process attachments: "),
        "got: {got}"
    );
}

#[tokio::test]
#[serial]
async fn empty_prompt_with_saved_attachment_gets_the_no_message_text_variant() {
    let _guard = StateDirGuard::new();
    let fetcher = Arc::new(FakeAttachmentFetcher::new([(
        "https://cdn.discordapp.com/a",
        &b"hello"[..],
    )]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;

    let workdir_root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", workdir_root.path().to_str().unwrap())
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    cp.start_session(
        &project.project_id,
        "",
        "test",
        &[text_attachment("https://cdn.discordapp.com/a")],
    )
    .await
    .unwrap();
    wait_for_prompts(&prompts, 1).await;

    let got = prompts.lock().unwrap()[0].clone();
    assert!(
        got.starts_with("User sent attachments with no message text.\n\n"),
        "got: {got}"
    );
}

#[tokio::test]
#[serial]
async fn empty_prompt_with_nothing_saved_gets_the_none_processed_variant() {
    let _guard = StateDirGuard::new();
    // No bodies registered — the fetch 404s, so nothing is saved.
    let fetcher = Arc::new(FakeAttachmentFetcher::new([]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;

    let workdir_root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", workdir_root.path().to_str().unwrap())
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    cp.start_session(
        &project.project_id,
        "",
        "test",
        &[text_attachment("https://cdn.discordapp.com/missing")],
    )
    .await
    .unwrap();
    wait_for_prompts(&prompts, 1).await;

    let got = prompts.lock().unwrap()[0].clone();
    assert!(
        got.starts_with("User sent attachments but none could be processed:\n"),
        "got: {got}"
    );
}

#[tokio::test]
#[serial]
async fn end_session_removes_the_attachments_dest_dir() {
    let _guard = StateDirGuard::new();
    let fetcher = Arc::new(FakeAttachmentFetcher::new([(
        "https://cdn.discordapp.com/a",
        &b"hello"[..],
    )]));
    let (cp, store, prompts, _db_guard) = fake_control_plane_with_fetcher(fetcher).await;

    let workdir_root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", workdir_root.path().to_str().unwrap())
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    let session = cp
        .start_session(
            &project.project_id,
            "hi",
            "test",
            &[text_attachment("https://cdn.discordapp.com/a")],
        )
        .await
        .unwrap();
    wait_for_prompts(&prompts, 1).await;

    let dest_dir = workdir_root
        .path()
        .join(".harness-attachments")
        .join(&session.session_pk);
    assert!(
        dest_dir.exists(),
        "attachment dir should exist before end_session"
    );

    cp.end_session(&session.session_pk).await.unwrap();
    assert!(
        !dest_dir.exists(),
        "end_session must remove the attachments dest dir"
    );
}

// ---------- Task 4: provision_project ----------

/// A minimal `ControlPlane` for provisioning tests: no harness needed
/// (provisioning never starts a session). Returns the sqlite temp-file
/// guard the caller must keep alive.
async fn provisioning_control_plane() -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let cp = ControlPlane::new(store, Registries::new()).await;
    let store_ref = cp.store().clone();
    (cp, store_ref, db_guard)
}

/// Build a bare-bones `ProvisionProjectRequest` — `name`/`git_url` left
/// `None` so each test fills in exactly what it needs.
fn provision_req(gateway: &str, workspace_id: &str, actor: &str) -> ProvisionProjectRequest {
    ProvisionProjectRequest {
        gateway: gateway.to_string(),
        workspace_id: workspace_id.to_string(),
        actor: actor.to_string(),
        actor_role_ids: vec![],
        name: None,
        git_url: None,
        settings: ProvisionSettings::default(),
    }
}

#[tokio::test]
async fn provision_project_errors_when_workdir_root_is_not_set() {
    let (cp, _store, _db_guard) = provisioning_control_plane().await;
    let req = provision_req("fake", "ws1", "u1");
    let err = cp.provision_project(req).await.unwrap_err();
    assert_eq!(err.to_string(), "workdir_root is not set");
}

#[tokio::test]
async fn provision_project_requires_name_or_git_url() {
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();
    let req = provision_req("fake", "ws1", "u1");
    let err = cp.provision_project(req).await.unwrap_err();
    assert_eq!(err.to_string(), "connectProject requires name or gitUrl");
}

#[tokio::test]
#[serial]
async fn provision_project_rejects_invalid_names() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    for bad in ["..", ".hidden", "a b", "."] {
        let mut req = provision_req("fake", "ws1", "u1");
        req.name = Some(bad.to_string());
        let err = cp
            .provision_project(req)
            .await
            .expect_err(&format!("{bad:?} should be rejected"));
        assert_eq!(err.to_string(), format!("invalid project name: {bad}"));
    }
}

/// Like `StateDirGuard`, but deliberately does NOT drop a `.gitconfig`
/// (or any git identity) under the redirected `HOME` — and scrubs the
/// `GIT_AUTHOR_*`/`GIT_COMMITTER_*`/`GIT_CONFIG_*` env vars a caller's
/// shell might otherwise export — so `git commit` (invoked by
/// `provision_project`'s NAME-flow) fails deterministically with "Author
/// identity unknown". Used to exercise the NAME-flow's rollback
/// (`remove_dir_all` on a git failure) without a fake/mocked `run_git`.
/// Process-global env — every test using it must be `#[serial]`.
/// The env vars `NoGitIdentityGuard` mutates; saved and restored so the
/// guard's effect is confined to its own scope (it sets values pointing at
/// an ephemeral tempdir, so leaking them poisons later git-using tests).
const GIT_ENV_VARS: [&str; 6] = [
    "XDG_DATA_HOME",
    "HOME",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
];

struct NoGitIdentityGuard {
    _dir: tempfile::TempDir,
    saved: Vec<(&'static str, Option<String>)>,
}

impl NoGitIdentityGuard {
    fn new() -> Self {
        let saved = GIT_ENV_VARS
            .iter()
            .map(|&k| (k, std::env::var(k).ok()))
            .collect();
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
        // Force `git commit` to hard-fail regardless of any ambient identity.
        // Clearing HOME/env isn't enough: with no configured identity git
        // auto-derives one from getpwuid()+hostname, so a commit would
        // *succeed* on CI runners (observed on macOS). Point GIT_CONFIG_GLOBAL
        // at a config that sets `user.useConfigOnly = true`, which disables
        // that auto-detection and makes the commit fail deterministically;
        // null out the system config so it can't supply an identity either.
        let config_path = dir.path().join("gitconfig");
        std::fs::write(&config_path, "[user]\n    useConfigOnly = true\n")
            .expect("write gitconfig");
        std::env::set_var("GIT_CONFIG_GLOBAL", &config_path);
        std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
        NoGitIdentityGuard { _dir: dir, saved }
    }
}

impl Drop for NoGitIdentityGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[tokio::test]
#[serial]
async fn provision_project_name_flow_rolls_back_the_dir_on_git_commit_failure() {
    let _guard = NoGitIdentityGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("rollback-me".to_string());
    let err = cp.provision_project(req).await.unwrap_err();

    assert!(
        err.to_string().contains("commit") && err.to_string().contains("identity"),
        "expected a git commit identity failure, got: {err}"
    );
    assert!(
        !root.path().join("rollback-me").exists(),
        "a failed git init/commit must roll back (remove) the created dir"
    );
}

#[tokio::test]
#[serial]
async fn provision_project_name_flow_creates_a_real_repo_with_head_and_binds_it() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("demo".to_string());
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(project.name, "demo");
    assert_eq!(project.workdir, root.path().join("demo").to_string_lossy());
    assert_eq!(project.harness, "native");
    assert_eq!(project.perm_mode, crate::domain::PermMode::Default);

    // A real repo with a HEAD commit (worktrees need one).
    let repo = git2::Repository::open(&project.workdir).unwrap();
    assert!(repo.head().is_ok());

    // Inserted + bound to the gateway workspace.
    assert!(store
        .get_project(&project.project_id)
        .await
        .unwrap()
        .is_some());
    let bound = store
        .resolve_project_by_workspace("fake", "ws1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound.project_id, project.project_id);
}

#[tokio::test]
#[serial]
async fn provision_project_git_url_flow_derives_name_and_records_source() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    // A local bare-ish repo to clone from (a real HEAD commit). Named
    // explicitly (not the raw tempdir path) since `tempfile` defaults to
    // a dot-prefixed name, which `validate_project_name` would reject.
    let upstream_root = tempfile::tempdir().unwrap();
    let upstream_dir = upstream_root.path().join("upstream-repo");
    std::fs::create_dir_all(&upstream_dir).unwrap();
    init_repo(&upstream_dir);

    let git_url = format!("{}/.git", upstream_dir.display());
    let mut req = provision_req("fake", "ws1", "u1");
    req.git_url = Some(git_url.clone());
    // A trailing "/.git" strips to the parent dir name ("upstream-repo") via basename.
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(project.name, "upstream-repo");
    assert_eq!(project.source.as_deref(), Some(git_url.as_str()));
    let repo = git2::Repository::open(&project.workdir).unwrap();
    assert!(repo.head().is_ok());
}

#[tokio::test]
#[serial]
async fn provision_project_git_clone_failure_rolls_back_the_dir() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.git_url = Some("/no/such/upstream/repo.git".to_string());
    let err = cp.provision_project(req).await.unwrap_err();
    assert!(err.to_string().contains("git"), "got: {err}");
    assert!(
        !root.path().join("repo").exists(),
        "a failed clone must not leave a partial dir behind"
    );
}

/// Regression for git-CLI option-injection hardening: the clone argv
/// now inserts `--` before the untrusted `git_url` (and workdir), so a
/// URL beginning with `-` can never be parsed by `git` as an option.
///
/// `"--upload-pack"` (rather than the more illustrative
/// `"--upload-pack=evil"`) is used deliberately: it both (a) starts with
/// `-`, exercising exactly the option-injection shape this hardens
/// against, and (b) is accepted by `validate_project_name` (ASCII
/// alphanumeric + `-` only) so the test actually reaches `run_git`'s
/// clone call instead of failing earlier on name validation (an `=`
/// character, as in `"--upload-pack=evil"`, is rejected by
/// `validate_project_name` before `run_git` is ever invoked).
///
/// Without the `--` separator, git would instead parse `--upload-pack`
/// as the option requiring a value, consume the destination workdir as
/// that value, and fail with a "You must specify a repository to
/// clone." usage dump — this test's second assertion fails if that
/// regresses.
#[tokio::test]
#[serial]
async fn provision_project_git_clone_treats_option_like_url_as_a_literal_repo_path() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    SettingsStore::new(store.clone())
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.git_url = Some("--upload-pack".to_string());

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        cp.provision_project(req),
    )
    .await
    .expect("provision_project must not hang on an option-like git_url")
    .unwrap_err();

    assert!(
        !err.to_string().contains("You must specify a repository"),
        "git must not have parsed the url as the --upload-pack option; got: {err}"
    );
    assert!(
        !root.path().join("--upload-pack").exists(),
        "a failed clone must not leave a partial dir behind"
    );
}

#[tokio::test]
async fn clone_project_derives_name_records_source_and_needs_no_settings() {
    // Gateway-free: no workdir_root, no gateway binding — a bare store.
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let dest = tempfile::tempdir().unwrap();

    let upstream_root = tempfile::tempdir().unwrap();
    let upstream_dir = upstream_root.path().join("upstream-repo");
    std::fs::create_dir_all(&upstream_dir).unwrap();
    init_repo(&upstream_dir);

    // Forward slashes on purpose: `basename_of` splits on `/` only, and git
    // accepts forward-slash local paths on Windows too. (The `\`-separated
    // form is exactly what makes provision_project_git_url_flow_* fail on
    // Windows dev boxes.)
    let git_url = format!("{}/.git", upstream_dir.display()).replace('\\', "/");
    let project = cp.clone_project(&git_url, dest.path()).await.unwrap();

    assert_eq!(project.name, "upstream-repo");
    assert_eq!(project.source.as_deref(), Some(git_url.as_str()));
    assert_eq!(project.harness, "native");
    assert!(project.is_git);
    assert_eq!(
        project.workdir,
        dest.path().join("upstream-repo").to_string_lossy()
    );
    let repo = git2::Repository::open(&project.workdir).unwrap();
    assert!(repo.head().is_ok(), "clone must produce a repo with a HEAD");
    assert!(store
        .get_project(&project.project_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn clone_project_rolls_back_on_failure_and_refuses_existing_dest() {
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let dest = tempfile::tempdir().unwrap();

    let err = cp
        .clone_project("/no/such/upstream/repo.git", dest.path())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("git"), "got: {err}");
    assert!(
        !dest.path().join("repo").exists(),
        "a failed clone must not leave a partial dir behind"
    );
    assert!(
        store.list_projects().await.unwrap().is_empty(),
        "no project row on failure"
    );

    // A pre-existing destination is refused BEFORE any git call — the
    // rollback below must never be able to delete user data.
    std::fs::create_dir_all(dest.path().join("taken")).unwrap();
    let err = cp
        .clone_project("https://example.invalid/taken.git", dest.path())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn provision_project_gates_bypass_permissions_for_non_admin() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(store.clone());
    settings
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();
    settings.set("admin_role_ids", "admin-role").await.unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("gated".to_string());
    req.actor_role_ids = vec![]; // not an admin
    req.settings.perm_mode = Some(crate::domain::PermMode::BypassPermissions);
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(
        project.perm_mode,
        crate::domain::PermMode::Default,
        "non-admin bypassPermissions request must be gated down"
    );
}

#[tokio::test]
#[serial]
async fn provision_project_admin_keeps_bypass_permissions() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(store.clone());
    settings
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();
    settings.set("admin_role_ids", "admin-role").await.unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("admin-project".to_string());
    req.actor_role_ids = vec!["admin-role".to_string()];
    req.settings.perm_mode = Some(crate::domain::PermMode::BypassPermissions);
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(
        project.perm_mode,
        crate::domain::PermMode::BypassPermissions
    );
}

#[tokio::test]
#[serial]
async fn provision_project_uses_settings_defaults_when_none_given() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(store.clone());
    settings
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();
    settings.set("default_model", "opus").await.unwrap();
    settings.set("default_effort", "high").await.unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("defaulted".to_string());
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(project.harness, "native");
    assert_eq!(project.model.as_deref(), Some("opus"));
    assert_eq!(project.effort.as_deref(), Some("high"));
}

#[tokio::test]
#[serial]
async fn provision_project_explicit_settings_override_defaults() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db_guard) = provisioning_control_plane().await;
    let root = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(store.clone());
    settings
        .set("workdir_root", root.path().to_str().unwrap())
        .await
        .unwrap();

    let mut req = provision_req("fake", "ws1", "u1");
    req.name = Some("overridden".to_string());
    req.settings.harness = Some("other-harness".to_string());
    req.settings.model = Some("sonnet".to_string());
    req.settings.effort = Some("low".to_string());
    let project = cp.provision_project(req).await.unwrap();

    assert_eq!(project.harness, "other-harness");
    assert_eq!(project.model.as_deref(), Some("sonnet"));
    assert_eq!(project.effort.as_deref(), Some("low"));
}

#[tokio::test]
async fn drain_resolves_immediately_when_nothing_is_running() {
    let (_db, path) = temp_db_path();
    let store = Store::open(&path).await.unwrap();
    let cp = ControlPlane::new(store, registries(false)).await;
    let t0 = std::time::Instant::now();
    cp.drain(1000).await;
    assert!(t0.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(cp.running_count(), 0);
}

#[tokio::test]
#[serial]
async fn start_and_continue_reject_once_draining() {
    let _guard = StateDirGuard::new();
    let (_db, path) = temp_db_path();
    let store = Store::open(&path).await.unwrap();
    let cp = ControlPlane::new(store, registries(false)).await;
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();

    cp.drain(10).await; // sets the latch; nothing running so returns fast

    let err = cp
        .start_session(&project.project_id, "x", "test", &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("draining"), "{err}");
    // Gate fires BEFORE the unknown-session lookup — the draining check is
    // continue_session's first statement.
    let err = cp.continue_session("nope", "x", &[]).await.unwrap_err();
    assert!(err.to_string().contains("draining"), "{err}");
}

#[tokio::test]
#[serial]
async fn drain_waits_for_an_in_flight_turn_up_to_the_timeout() {
    let _guard = StateDirGuard::new();
    let (_db, path) = temp_db_path();
    let store = Store::open(&path).await.unwrap();
    let cp = ControlPlane::new(store, registries(true)).await; // send_prompt blocks until cancel
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    let session = cp
        .start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();

    for _ in 0..400 {
        if cp.running_count() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(cp.running_count(), 1, "the blocked turn must be in flight");

    let t0 = std::time::Instant::now();
    cp.drain(200).await;
    assert!(t0.elapsed() >= std::time::Duration::from_millis(200));
    assert_eq!(
        cp.running_count(),
        1,
        "drain timed out — it must never kill the turn"
    );

    // cleanup: cancel the blocked turn and wait for the guard to release
    cp.stop_session(&session.session_pk).await.unwrap();
    for _ in 0..400 {
        if cp.running_count() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(cp.running_count(), 0);
}

// ---------- plugin connector MCP servers attach to sessions ----------

/// A connector-capable declarative plugin: one `[[mcp]]` stdio server named
/// `server_name`, whose only env var is `${auth}` — so `connector.mcp_servers()`
/// succeeds when `plugin.<id>.token` is set and fails (unresolved
/// placeholder) when it isn't. Exercises `attach_plugin_mcp_servers`'s
/// "broken connector" path with a real, not a fake, `Connector`.
fn declarative_test_plugin(id: &str, server_name: &str) -> crate::plugins::CorePlugin {
    use ryuzi_plugin_sdk::{AuthKind, AuthSpec, McpServerDef, McpTransportDef, PluginManifest};

    let manifest = PluginManifest {
        contract: 1,
        id: id.to_string(),
        name: format!("Test Plugin {id}"),
        version: String::new(),
        publisher: String::new(),
        description: String::new(),
        homepage: None,
        icon: None,
        categories: vec![],
        verified: false,
        experimental: false,
        auth: Some(AuthSpec {
            kind: AuthKind::Token,
            setting: Some(format!("plugin.{id}.token")),
            ..Default::default()
        }),
        settings: vec![],
        mcp: vec![McpServerDef {
            name: server_name.to_string(),
            transport: McpTransportDef::Stdio,
            command: Some("acme-mcp".to_string()),
            args: vec![],
            env: std::collections::BTreeMap::from([("TOKEN".to_string(), "${auth}".to_string())]),
            url: None,
            headers: std::collections::BTreeMap::new(),
        }],
        skills: vec![],
        provider: None,
    };
    crate::plugins::declarative::declarative_plugin(manifest, crate::plugins::PluginSource::Builtin)
        .expect("test manifest must validate")
}

/// Build a `ControlPlane` wired like `fake_control_plane`, but whose
/// `Registries` also carries `plugin` — for the plugin-connector
/// session-attach tests below. Returns the full `Counters` (not just its
/// `prompts` field) so a test can inspect `counters.mcp_servers`.
async fn fake_control_plane_with_plugin(
    plugin: crate::plugins::CorePlugin,
) -> (
    Arc<ControlPlane>,
    Arc<Store>,
    Counters,
    tempfile::NamedTempFile,
) {
    let (db_guard, db_path) = temp_db_path();
    let store = crate::store::Store::open(&db_path).await.unwrap();
    let counters = Counters::default();
    let mut regs = registries_with(false, counters.clone());
    regs.add_plugin(plugin);
    let cp = ControlPlane::new(store, regs).await;
    let store_ref = cp.store().clone();
    (cp, store_ref, counters, db_guard)
}

#[tokio::test]
#[serial]
async fn enabled_declarative_plugins_mcp_server_attaches_to_the_session() {
    let _guard = StateDirGuard::new();
    let (cp, store, counters, _db_guard) =
        fake_control_plane_with_plugin(declarative_test_plugin("task7-lc-attach", "acme")).await;
    store
        .set_setting_raw("plugin.task7-lc-attach.token", "sekret")
        .await
        .unwrap();
    store
        .set_setting_raw("plugin.task7-lc-attach.enabled", "true")
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    cp.start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();

    let servers = wait_for_session_ctx(&counters).await;
    assert!(
        servers.iter().any(|s| s.name == "acme"),
        "expected the enabled plugin's mcp server to attach, got: {servers:?}"
    );
}

#[tokio::test]
#[serial]
async fn disabled_declarative_plugins_mcp_server_does_not_attach() {
    let _guard = StateDirGuard::new();
    let (cp, store, counters, _db_guard) =
        fake_control_plane_with_plugin(declarative_test_plugin("task7-lc-disabled", "acme")).await;
    // Configured, but never enabled — `plugin.<id>.enabled` defaults to false.
    store
        .set_setting_raw("plugin.task7-lc-disabled.token", "sekret")
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    cp.start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();

    let servers = wait_for_session_ctx(&counters).await;
    assert!(
        !servers.iter().any(|s| s.name == "acme"),
        "a disabled plugin's mcp server must not attach, got: {servers:?}"
    );
}

#[tokio::test]
#[serial]
async fn broken_plugin_connector_is_skipped_and_never_fails_session_start() {
    let _guard = StateDirGuard::new();
    let (cp, store, counters, _db_guard) =
        fake_control_plane_with_plugin(declarative_test_plugin("task7-lc-broken", "acme")).await;
    // Enabled, but never configured — `${auth}` has nothing to resolve from,
    // so `connector.mcp_servers()` returns `Err`.
    store
        .set_setting_raw("plugin.task7-lc-broken.enabled", "true")
        .await
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    // The whole point: a broken connector must never prevent session start.
    cp.start_session(&project.project_id, "go", "test", &[])
        .await
        .expect("a broken plugin connector must not fail session start");

    let servers = wait_for_session_ctx(&counters).await;
    assert!(
        !servers.iter().any(|s| s.name == "acme"),
        "a connector that failed to resolve must not contribute a server, got: {servers:?}"
    );
}

#[tokio::test]
#[serial]
async fn db_configured_server_wins_over_a_same_named_plugin_server() {
    let _guard = StateDirGuard::new();
    let (cp, store, counters, _db_guard) =
        fake_control_plane_with_plugin(declarative_test_plugin("task7-lc-collide", "acme")).await;
    store
        .set_setting_raw("plugin.task7-lc-collide.token", "sekret")
        .await
        .unwrap();
    store
        .set_setting_raw("plugin.task7-lc-collide.enabled", "true")
        .await
        .unwrap();

    // A DB-configured server sharing the plugin's server name ("acme").
    crate::mcp::upsert_server(
        &store,
        crate::mcp::McpServerRow {
            id: "acme".into(),
            name: "Acme (DB)".into(),
            kind: "MCP server".into(),
            color: "#000000".into(),
            description: String::new(),
            transport: "stdio".into(),
            command: Some("db-acme-mcp".into()),
            args: vec![],
            env: vec![],
            url: None,
            scope: "global".into(),
            scope_gateways: vec![],
            version: None,
            publisher: None,
            status: "unknown".into(),
            status_detail: None,
            auth_kind: "none".into(),
            auth_detail: None,
        },
    )
    .await
    .unwrap();

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let project = cp.connect_project(repo.path(), "demo").await.unwrap();
    cp.start_session(&project.project_id, "go", "test", &[])
        .await
        .unwrap();

    let servers = wait_for_session_ctx(&counters).await;
    let acme: Vec<_> = servers.iter().filter(|s| s.name == "acme").collect();
    assert_eq!(
        acme.len(),
        1,
        "exactly one \"acme\" server must attach, got: {servers:?}"
    );
    match &acme[0].transport {
        crate::domain::McpTransport::Stdio { command, .. } => {
            assert_eq!(
                command, "db-acme-mcp",
                "the DB-configured server must win over the plugin's same-named one"
            );
        }
        other => panic!("expected a stdio transport, got: {other:?}"),
    }
}

fn git_opts(
    use_worktree: bool,
    create_branch: bool,
    branch_name: Option<&str>,
    base_branch: Option<&str>,
) -> crate::domain::SessionGitOptions {
    crate::domain::SessionGitOptions {
        use_worktree,
        create_branch,
        branch_name: branch_name.map(str::to_string),
        base_branch: base_branch.map(str::to_string),
    }
}

#[tokio::test]
#[serial]
async fn user_named_branch_survives_end_session() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db) = fake_control_plane_any_harness().await;
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let project = cp.connect_project(repo_dir.path(), "demo").await.unwrap();

    let session = cp
        .start_session_with_prompt(
            &project.project_id,
            TurnPrompt::text("go", "go"),
            "test",
            &[],
            Some(git_opts(true, true, Some("keep/me"), None)),
        )
        .await
        .unwrap();
    assert_eq!(session.branch.as_deref(), Some("keep/me"));
    assert!(
        !session.branch_owned,
        "user-named branch is not engine-owned"
    );

    // Let background startup finish so teardown has a real worktree to clean.
    wait_for_running_handle(&cp, &session.session_pk).await;
    cp.end_session(&session.session_pk).await.unwrap();

    let repo = git2::Repository::open(repo_dir.path()).unwrap();
    assert!(
        repo.find_branch("keep/me", git2::BranchType::Local).is_ok(),
        "a user-named branch must survive teardown"
    );
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.worktree_path.is_none(), "worktree path cleared");
}

#[tokio::test]
#[serial]
async fn engine_named_branch_is_deleted_on_end_session() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db) = fake_control_plane_any_harness().await;
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let project = cp.connect_project(repo_dir.path(), "demo").await.unwrap();

    // git: None => Default => exact legacy behavior.
    let session = cp
        .start_session_with_prompt(
            &project.project_id,
            TurnPrompt::text("go", "go"),
            "test",
            &[],
            None,
        )
        .await
        .unwrap();
    assert!(session.branch_owned);
    // The engine-generated name is backfilled by background startup.
    wait_for_running_handle(&cp, &session.session_pk).await;
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    let branch = stored.branch.clone().unwrap();
    assert!(branch.starts_with("harness/"));

    cp.end_session(&session.session_pk).await.unwrap();

    let repo = git2::Repository::open(repo_dir.path()).unwrap();
    assert!(
        repo.find_branch(&branch, git2::BranchType::Local).is_err(),
        "the engine-named branch must be deleted with its worktree"
    );
}

#[tokio::test]
#[serial]
async fn no_worktree_session_runs_in_place_and_teardown_leaves_checkout_alone() {
    let _guard = StateDirGuard::new();
    let (cp, store, _db) = fake_control_plane_any_harness().await;
    let repo_dir = tempfile::tempdir().unwrap();
    init_repo(repo_dir.path());
    let project = cp.connect_project(repo_dir.path(), "demo").await.unwrap();
    let repo = git2::Repository::open(repo_dir.path()).unwrap();
    let head_before = repo.head().unwrap().shorthand().unwrap().to_string();

    let session = cp
        .start_session_with_prompt(
            &project.project_id,
            TurnPrompt::text("go", "go"),
            "test",
            &[],
            Some(git_opts(false, false, None, None)),
        )
        .await
        .unwrap();
    assert!(session.worktree_path.is_none(), "no worktree for this cell");
    // The branch (current checkout) is resolved during background prep.
    wait_for_running_handle(&cp, &session.session_pk).await;
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.worktree_path.is_none(), "no worktree for this cell");
    assert_eq!(stored.branch.as_deref(), Some(head_before.as_str()));
    // In-place cell: the startup copy must not claim anything was created
    // (no worktree, no branch; this same-branch cell checks nothing out).
    let msgs = store.list_messages(&session.session_pk).await.unwrap();
    let statuses: Vec<String> = msgs
        .iter()
        .filter(|m| m.block_type == "status")
        .map(|m| m.payload["summary"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(statuses[0], "Preparing workspace…");
    assert_eq!(statuses[1], format!("Using branch {head_before}"));

    cp.end_session(&session.session_pk).await.unwrap();

    assert_eq!(
        repo.head().unwrap().shorthand().unwrap(),
        head_before,
        "teardown must never switch the user's checkout"
    );
    let stored = store
        .get_session(&session.session_pk)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, SessionStatus::Ended);
}
