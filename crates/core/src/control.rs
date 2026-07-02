use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, Message, PermMode, Project, Session, SessionStatus};
use crate::harness::{HarnessSession, SessionCtx};
use crate::integration::Registries;
use crate::paths::{new_id, now_ms, worktree_path_for};
use crate::store::Store;
use crate::worktree;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Nudge prompt used when re-driving a turn interrupted by a restart (TS parity).
pub const RESUME_NUDGE: &str = "Your previous turn was interrupted by a daemon restart or update. \
    Continue the task from where you left off. If it was already complete, briefly summarize what you did.";

pub struct ControlPlane {
    store: Arc<Store>,
    /// Extension registries (harness/gateway/connector). `Project.harness` is
    /// resolved against `registries.harness`.
    registries: Registries,
    events: broadcast::Sender<CoreEvent>,
    /// Shared approval hub — handed to each `SessionCtx` so the ACP permission
    /// bridge can route tool-permission prompts back to the UI.
    approvals: Arc<ApprovalHub>,
    /// Live sessions keyed by `session_pk`. Each value is the harness session
    /// handle returned by `Harness::start_session`, used to drive prompts and to
    /// `cancel`/`end` the session.
    running: Mutex<HashMap<String, Arc<dyn HarnessSession>>>,
}

impl ControlPlane {
    pub async fn new(store: Store, registries: Registries) -> Arc<ControlPlane> {
        let (events, _) = broadcast::channel(1024);
        Arc::new(ControlPlane {
            store: Arc::new(store),
            registries,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            running: Mutex::new(HashMap::new()),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    pub fn resolve_approval(&self, request_id: &str, allow: bool) -> bool {
        self.approvals.resolve(request_id, allow)
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.store.list_projects().await
    }

    pub async fn list_sessions(&self, project_id: Option<&str>) -> anyhow::Result<Vec<Session>> {
        self.store.list_sessions(project_id).await
    }

    /// Persist per-project preferences (host-supplied model/effort/perm overrides).
    /// `None` leaves the corresponding column untouched.
    pub async fn set_project_prefs(
        &self,
        project_id: &str,
        model: Option<&str>,
        effort: Option<&str>,
        perm_mode: Option<PermMode>,
    ) -> anyhow::Result<()> {
        self.store
            .update_project_prefs(project_id, model, effort, perm_mode)
            .await
    }

    pub async fn connect_project(&self, workdir: &Path, name: &str) -> anyhow::Result<Project> {
        // Must be an existing git repo (worktrees need a HEAD commit).
        git2::Repository::open(workdir)
            .map_err(|_| anyhow::anyhow!("not a git repository: {}", workdir.display()))?;
        let project = Project {
            project_id: new_id(),
            name: name.to_string(),
            workdir: workdir.to_string_lossy().into_owned(),
            source: None,
            harness: "claude-code".into(),
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(now_ms()),
        };
        self.store.insert_project(project.clone()).await?;
        Ok(project)
    }

    pub async fn start_session(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        started_by: &str,
    ) -> anyhow::Result<Session> {
        let project = self
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?;

        let session_pk = new_id();
        let short: String = session_pk.chars().take(8).collect();
        let branch = format!("harness/{short}");
        let worktree_path = worktree_path_for(&project.project_id, &session_pk);
        worktree::create(Path::new(&project.workdir), &short, &branch, &worktree_path)?;

        let now = now_ms();
        let title: String = prompt.chars().take(80).collect();
        let session = Session {
            session_pk: session_pk.clone(),
            project_id: project.project_id.clone(),
            agent_session_id: None,
            worktree_path: Some(worktree_path.to_string_lossy().into_owned()),
            branch: Some(branch),
            title: Some(title),
            status: SessionStatus::Running,
            started_by: Some(started_by.to_string()),
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
        };
        self.store.insert_session(session.clone()).await?;
        let _ = self.events.send(CoreEvent::SessionCreated {
            session_pk: session_pk.clone(),
            project_id: project.project_id.clone(),
        });

        // Resolve + start the harness session synchronously so an immediate
        // `stop_session` finds a live handle. The prompt is then driven in the
        // background so the cockpit can juggle many sessions concurrently.
        let handle = self
            .start_harness_session(&project, &session_pk, &worktree_path, None)
            .await?;
        self.spawn_prompt(handle, session_pk.clone(), prompt.to_string());

        Ok(session)
    }

    /// Send a follow-up prompt on an existing session.
    ///
    /// The ACP session built by `start_harness_session` is long-lived: its
    /// handle holds an mpsc `ClientRequest` channel whose client loop stays
    /// connected to serve many prompts on ONE session. So the fast (normal)
    /// path here REUSES the live handle from the `running` map — no new adapter
    /// process, no `session/load` replay, because the live adapter already holds
    /// the full conversation context.
    ///
    /// Only when the handle is ABSENT — e.g. the in-memory `running` map was
    /// wiped by an app restart — do we start a FRESH session that resumes via
    /// `session/load` (passing `session.agent_session_id` as `resume`). That
    /// cold-resume path is the single place `session/load` is needed.
    pub async fn continue_session(
        self: &Arc<Self>,
        session_pk: &str,
        prompt: &str,
    ) -> anyhow::Result<()> {
        let session = self
            .store
            .get_session(session_pk)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown session: {session_pk}"))?;

        self.store
            .update_status(session_pk, SessionStatus::Running, None)
            .await?;

        // Fast path: reuse the live ACP session if its handle is still in the
        // `running` map. The live adapter already holds context, so no new
        // adapter is spawned and no `session/load` replay happens.
        let existing = self.running.lock().unwrap().get(session_pk).cloned();
        let handle = match existing {
            Some(handle) => handle,
            None => {
                // Cold-resume path: the in-memory handle is gone (e.g. after an
                // app restart). Start a FRESH session that resumes the prior
                // conversation via `session/load` using the persisted agent id.
                let project = self
                    .store
                    .get_project(&session.project_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("unknown project: {}", session.project_id))?;
                let work_dir = session
                    .worktree_path
                    .clone()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(&project.workdir));
                match self
                    .start_harness_session(
                        &project,
                        session_pk,
                        &work_dir,
                        session.agent_session_id.clone(),
                    )
                    .await
                {
                    Ok(handle) => handle,
                    Err(e) => {
                        // Roll back the eager Running transition — don't leave the row stuck.
                        let _ = self
                            .store
                            .update_status(session_pk, SessionStatus::Idle, None)
                            .await;
                        return Err(e);
                    }
                }
                // `start_harness_session` already persists any resolved
                // agent_session_id, and `spawn_prompt` persists it post-turn,
                // so no redundant `update_agent_session_id` is needed here.
            }
        };
        self.spawn_prompt(handle, session_pk.to_string(), prompt.to_string());
        Ok(())
    }

    /// Persist a user-visible status row (role=system, block_type=status) and
    /// broadcast it — the Rust equivalent of TS's ephemeral status events.
    async fn emit_status(&self, session_pk: &str, text: &str) {
        let payload = serde_json::json!({ "summary": text });
        let msg = crate::domain::NewMessage::block(session_pk, "system", "status", payload.clone());
        if let Ok(seq) = self.store.insert_message(msg).await {
            let _ = self.events.send(CoreEvent::Message {
                session_pk: session_pk.to_string(),
                seq,
                role: "system".to_string(),
                block_type: "status".to_string(),
                payload,
                tool_call_id: None,
                status: None,
                tool_kind: None,
            });
        }
    }

    /// Re-drive an interrupted turn after a restart, guarded by the attempts
    /// cap so a session that reliably crashes the daemon cannot loop forever.
    pub async fn resume_session(
        self: &Arc<Self>,
        session_pk: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let Some(session) = self.store.get_session(session_pk).await? else {
            return Ok(());
        };
        let Some(project) = self.store.get_project(&session.project_id).await? else {
            return Ok(());
        };
        if session.agent_session_id.is_none() {
            self.store
                .update_status(session_pk, SessionStatus::Idle, None)
                .await?;
            self.emit_status(session_pk, "⚠️ Interrupted by a restart and could not be auto-resumed — send a message to continue.").await;
            return Ok(());
        }
        if session.resume_attempts >= 3 {
            self.store
                .update_status(session_pk, SessionStatus::Idle, None)
                .await?;
            self.emit_status(
                session_pk,
                "⚠️ Auto-resume gave up after 3 attempts — send a message to continue.",
            )
            .await;
            return Ok(());
        }
        self.store
            .update_resume(
                session_pk,
                SessionStatus::Running,
                session.resume_attempts + 1,
            )
            .await?;
        self.emit_status(session_pk, &format!("🔄 Resumed after {reason}."))
            .await;
        let work_dir = session
            .worktree_path
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(&project.workdir));
        match self
            .start_harness_session(
                &project,
                session_pk,
                &work_dir,
                session.agent_session_id.clone(),
            )
            .await
        {
            Ok(handle) => {
                self.spawn_prompt(handle, session_pk.to_string(), RESUME_NUDGE.to_string());
                Ok(())
            }
            Err(e) => {
                let _ = self
                    .store
                    .update_status(session_pk, SessionStatus::Idle, None)
                    .await;
                Err(e)
            }
        }
    }

    /// On boot: resume every session a dead process left in Running. Each
    /// resume is isolated so one bad session can't block the rest.
    pub async fn reconcile(self: &Arc<Self>) -> anyhow::Result<()> {
        for s in self
            .store
            .list_sessions_by_status(SessionStatus::Running)
            .await?
        {
            let _ = self.resume_session(&s.session_pk, "restart").await;
        }
        Ok(())
    }

    /// Resolve `project.harness` in the registry, create the harness, build a
    /// `SessionCtx`, and start the session. Records the returned handle in the
    /// `running` map and returns a clone for driving the first prompt.
    async fn start_harness_session(
        self: &Arc<Self>,
        project: &Project,
        session_pk: &str,
        work_dir: &Path,
        resume: Option<String>,
    ) -> anyhow::Result<Arc<dyn HarnessSession>> {
        let factory = self
            .registries
            .harness
            .get(&project.harness)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown harness '{}' (registered: {:?})",
                    project.harness,
                    self.registries.harness.names()
                )
            })?;
        let harness = factory.create()?;

        let ctx = SessionCtx {
            session_pk: session_pk.to_string(),
            work_dir: work_dir.to_path_buf(),
            perm_mode: project.perm_mode,
            model: project.model.clone(),
            effort: project.effort.clone(),
            resume,
            mcp_servers: vec![],
            events: self.events.clone(),
            approvals: self.approvals.clone(),
            store: self.store.clone(),
        };

        let handle: Arc<dyn HarnessSession> = Arc::from(harness.start_session(ctx).await?);

        // Persist the agent session id the harness established (for later resume).
        if let Some(sid) = handle.agent_session_id() {
            let _ = self.store.update_agent_session_id(session_pk, &sid).await;
        }

        self.running
            .lock()
            .unwrap()
            .insert(session_pk.to_string(), handle.clone());
        Ok(handle)
    }

    /// Drive a prompt on `handle` in the background. `send_prompt` blocks until
    /// the turn completes (ACP `EndTurn`); on completion we atomically demote
    /// `Running → Idle` (unless the session was already Interrupted/Ended) and
    /// broadcast a `Result`. Errors surface as `CoreEvent::Error`.
    ///
    /// The `handle` is the PERSISTENT live session — the same handle inserted
    /// into `running` by `start_harness_session` and reused across
    /// `continue_session` turns. This method therefore does NOT remove it from
    /// `running` on turn completion: the session stays alive to serve the next
    /// prompt. It is removed and `end()`ed only by `end_session`.
    fn spawn_prompt(
        self: &Arc<Self>,
        handle: Arc<dyn HarnessSession>,
        session_pk: String,
        prompt: String,
    ) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            match handle.send_prompt(prompt).await {
                Ok(()) => {
                    // Persist any agent session id resolved during the turn.
                    if let Some(sid) = handle.agent_session_id() {
                        let _ = me.store.update_agent_session_id(&session_pk, &sid).await;
                    }
                    let _ = me.store.demote_if_running(&session_pk, now_ms()).await;
                    let _ = me.events.send(CoreEvent::Result {
                        session_pk: session_pk.clone(),
                    });
                }
                Err(e) => {
                    let _ = me.events.send(CoreEvent::Error {
                        session_pk: session_pk.clone(),
                        message: e.to_string(),
                    });
                    let _ = me.store.demote_if_running(&session_pk, now_ms()).await;
                }
            }
        });
    }

    pub async fn stop_session(&self, session_pk: &str) -> anyhow::Result<()> {
        let handle = self.running.lock().unwrap().get(session_pk).cloned();
        if let Some(handle) = handle {
            let _ = handle.cancel().await;
        }
        self.store
            .update_status(session_pk, SessionStatus::Interrupted, Some(now_ms()))
            .await?;
        Ok(())
    }

    /// Tear down a session. This is the ONLY place the persistent live-session
    /// handle is removed from `running` and `end()`ed (graceful ACP teardown),
    /// after which the worktree is cleaned up and the session marked `Ended`.
    pub async fn end_session(&self, session_pk: &str) -> anyhow::Result<()> {
        let handle = self.running.lock().unwrap().remove(session_pk);
        if let Some(handle) = handle {
            let _ = handle.end().await;
        }
        if let Some(session) = self.store.get_session(session_pk).await? {
            if let Some(project) = self.store.get_project(&session.project_id).await? {
                if let Some(wt) = &session.worktree_path {
                    let short: String = session_pk.chars().take(8).collect();
                    let _ = worktree::remove(Path::new(&project.workdir), &short, Path::new(wt));
                }
            }
        }
        self.store
            .update_status(session_pk, SessionStatus::Ended, Some(now_ms()))
            .await?;
        let _ = self.events.send(CoreEvent::SessionEnded {
            session_pk: session_pk.to_string(),
        });
        Ok(())
    }

    pub async fn list_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        self.store.list_messages(session_pk).await
    }

    /// Retrieve the persisted tool policy for `(project_id, tool)`, if any.
    pub async fn get_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
    ) -> anyhow::Result<Option<String>> {
        self.store.get_tool_policy(project_id, tool).await
    }

    /// Persist (or update) a tool policy for `(project_id, tool)`.
    pub async fn set_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        self.store.set_tool_policy(project_id, tool, decision).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CoreEvent, NewMessage};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use crate::integration::Registries;
    use async_trait::async_trait;
    use serial_test::serial;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Redirect dirs::data_dir() into a tempdir for the duration of a test so
    /// worktree creation never touches the real ~/.local/share. Process-global
    /// env — every test using it must be #[serial].
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
        async fn send_prompt(&self, prompt: String) -> anyhow::Result<()> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            self.prompts.lock().unwrap().push(prompt.clone());
            // Persist the user turn (as the ACP session does in send_prompt).
            let _ = self
                .store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt }),
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
    }

    struct FakeHarness {
        block_until_cancel: bool,
        counters: Counters,
    }

    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            self.counters.starts.fetch_add(1, Ordering::SeqCst);
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

    /// Build a `Registries` with a `claude-code` harness backed by the fake.
    fn registries(block_until_cancel: bool) -> Registries {
        registries_with(block_until_cancel, Counters::default())
    }

    /// Like `registries`, but sharing `counters` so a test can inspect how many
    /// times the harness started a session / drove a prompt / ended.
    fn registries_with(block_until_cancel: bool, counters: Counters) -> Registries {
        let mut regs = Registries::new();
        regs.harness.register(
            "claude-code",
            Arc::new(FakeHarnessFactory {
                block_until_cancel,
                counters,
            }),
        );
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

    /// Like `fake_control_plane`, but the registered harness always fails to
    /// start — for testing the cold-resume rollback in `continue_session`.
    async fn control_plane_with_failing_factory(
    ) -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
        let (db_guard, db_path) = temp_db_path();
        let store = crate::store::Store::open(&db_path).await.unwrap();
        let mut regs = Registries::new();
        regs.harness
            .register("claude-code", Arc::new(FailingHarnessFactory));
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
                harness: "claude-code".into(),
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(now_ms()),
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

    #[tokio::test]
    #[serial]
    async fn unknown_harness_errors_cleanly() {
        let _guard = StateDirGuard::new();
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(db.path()).await.unwrap();
        // Empty registry → no harness registered under "claude-code".
        let cp = ControlPlane::new(store, Registries::new()).await;
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        let err = cp
            .start_session(&project.project_id, "go", "test")
            .await
            .expect_err("start_session should fail without a registered harness");
        assert!(
            err.to_string().contains("unknown harness"),
            "expected a clear unknown-harness error, got: {err}"
        );
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
            .start_session(&project.project_id, "go", "test")
            .await
            .unwrap();
        // The harness handle must be registered synchronously by start_session,
        // BEFORE the background prompt task is spawned — so an immediate stop
        // reaches the live session's cancel().
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
            .start_session(&project.project_id, "first", "test")
            .await
            .unwrap();
        // Let the background prompt task finish its turn.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Snapshot the live handle so we can prove the SAME one is reused.
        let handle_before = cp
            .running
            .lock()
            .unwrap()
            .get(&session.session_pk)
            .cloned()
            .expect("start_session must register a live handle");

        // Second turn: MUST reuse the live handle — no new ACP session.
        cp.continue_session(&session.session_pk, "second")
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
            .start_session(&project.project_id, "first", "test")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Simulate an app restart that wiped the in-memory running map.
        cp.running.lock().unwrap().remove(&session.session_pk);

        // Continue must fall back to the cold-resume path: start a FRESH session
        // (via session/load) and register a new live handle.
        cp.continue_session(&session.session_pk, "second")
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
            .start_session(&project.project_id, "go", "test")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(cp.running.lock().unwrap().contains_key(&session.session_pk));

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
            .start_session(&project.project_id, "do it", "test")
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

        // History is durable: the user prompt + streamed assistant text persist in order.
        let msgs = cp.list_messages(&session.session_pk).await.unwrap();
        let kinds: Vec<(&str, &str)> = msgs
            .iter()
            .map(|m| (m.role.as_str(), m.block_type.as_str()))
            .collect();
        assert_eq!(kinds.first(), Some(&("user", "text")));
        assert_eq!(msgs[0].payload["text"], "do it");
        assert!(msgs.iter().any(|m| m.role == "assistant"
            && m.block_type == "text"
            && m.payload["text"] == "working"));
        // seq is monotonic and matches insertion order.
        assert!(msgs.windows(2).all(|w| w[0].seq < w[1].seq));
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

        let s = store.get_session("s1").await.unwrap().unwrap();
        // the resumed turn completed via the fake → demote reset attempts to 0
        assert_eq!(s.resume_attempts, 0);
        assert_eq!(s.status, SessionStatus::Idle);
        // the 🔄 status row was persisted
        let msgs = store.list_messages("s1").await.unwrap();
        assert!(msgs.iter().any(
            |m| m.block_type == "status" && m.payload["summary"] == "🔄 Resumed after restart."
        ));
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
        assert!(msgs.iter().any(|m| m.payload["summary"]
            == "⚠️ Interrupted by a restart and could not be auto-resumed — send a message to continue."));
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

        assert!(cp.continue_session("s1", "hi").await.is_err());

        let s = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(
            s.status,
            SessionStatus::Idle,
            "must not be left stuck in Running"
        );
    }
}
