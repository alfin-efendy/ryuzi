//! The daemon-hosted background-rail drainer (spec §6.1). Delivers completed
//! out-of-band work (async delegation, learning forks, scheduled jobs, orch
//! events — anything `Store::enqueue_background_event` recorded) into its
//! target session as a NEW user turn, but only while that session is idle.
//!
//! This idle-only delivery is the rail's one load-bearing invariant: it must
//! NEVER splice into a running turn. Hermes' role-alternation and
//! prompt-cache safety depend on every user turn starting from an idle
//! session. `Store::claim_deliverable_background_event` enforces the
//! idle-only filter atomically (it joins against `sessions.status`); this
//! module only has to honor whatever it hands back and never bypass it.
//!
//! Delivery always goes through `ControlPlane::continue_session_with_prompt`
//! — the SAME "clean new user turn" path every other caller uses — never a
//! direct ledger write and never a mid-turn steer injection. A successful
//! delivery marks the row delivered; a failed one (target session vanished,
//! the harness couldn't start, the daemon is draining, …) releases the claim
//! so a later tick retries it. Rows are durable: they are never dropped on
//! failure, only left pending for the next attempt.

use crate::control::ControlPlane;
use crate::harness::TurnPrompt;
use std::sync::Arc;
use std::time::Duration;

/// Poll cadence for the drainer loop (mirrors `scheduler`/`orch`'s 5s cadence
/// — see their `run_loop`s).
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Deliver at most this many rows per tick, so one busy tick can't starve the
/// loop or block the daemon's runtime for too long — each delivery dispatches
/// a full continue-turn.
const MAX_PER_TICK: usize = 8;

/// One drain pass: claim up to `MAX_PER_TICK` idle-target rows and deliver
/// each as a new user turn. Factored out of [`run_loop`] so tests can drive
/// it without sleeping.
pub async fn tick(cp: &Arc<ControlPlane>) {
    let store = cp.store();
    // Rows that failed delivery THIS tick are kept CLAIMED for the rest of the
    // tick so the next claim (which filters `claimed_by IS NULL`) skips them and
    // reaches a different target's pending work — then released below so a later
    // tick retries them. This bounds a permanently-broken target to one delivery
    // attempt per tick (never MAX_PER_TICK re-attempts of the same oldest row)
    // and stops it starving every other target within the tick.
    let mut failed_this_tick: Vec<String> = Vec::new();
    for _ in 0..MAX_PER_TICK {
        let event = match store.claim_deliverable_background_event("drainer").await {
            Ok(Some(event)) => event,
            Ok(None) => break, // nothing deliverable right now
            Err(e) => {
                tracing::warn!("background_rail: claim failed: {e}");
                break;
            }
        };
        // An `unblock` answer must resume the blocked worker's orch task
        // causally with delivery — flip it `blocked → running` here, not by a
        // tick-time session-status poll (which false-resumes the block-turn
        // tail and strands a fast resume). Safe for non-orch kinds: this is
        // gated on `kind == "unblock"`, which only ever targets a worker
        // session. Runs BEFORE the turn re-enters so the flip is settled by the
        // time the resumed turn's terminal event reaches `watch_session`.
        if event.kind == "unblock" {
            crate::orch::on_unblock_delivered(cp, &event.target_session_pk).await;
        }
        // `continue_session_with_prompt` is the ONLY delivery path: a clean
        // new user turn onto a session `claim_deliverable_background_event`
        // already proved is idle. Never a mid-turn splice.
        match cp
            .continue_session_with_prompt(
                &event.target_session_pk,
                TurnPrompt::text(event.payload.clone(), event.payload.clone()),
                &[],
            )
            .await
        {
            Ok(()) => {
                let _ = store.mark_background_delivered(&event.id).await;
            }
            Err(e) => {
                // Target vanished / harness couldn't start / daemon draining.
                // Keep the row claimed for now (released after the loop) so the
                // next claim reaches other targets instead of re-picking this
                // same oldest row.
                tracing::warn!("background_rail: delivery of {} failed: {e}", event.id);
                failed_this_tick.push(event.id);
            }
        }
    }
    // Release this tick's failures so a future tick re-claims and retries them
    // (once per tick — never lost, never left claimed forever under normal run).
    for id in failed_this_tick {
        let _ = store.release_background_claim(&id).await;
    }
}

/// The drainer's background loop: sleep, then drain a batch, forever.
///
/// Returned as a future (not self-spawned) so hosts can run it on their own
/// runtime, mirroring `scheduler::run_loop` / `orch::run_loop`.
pub async fn run_loop(cp: Arc<ControlPlane>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        tick(&cp).await;
    }
}

/// Spawn the drainer on the host's runtime (mirrors `scheduler::spawn_runner`
/// / `orch::spawn_runner` — the daemon is the single always-on engine host
/// for all of these background loops).
pub fn spawn_runner(cp: Arc<ControlPlane>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(cp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Message, NewMessage, PermMode, Session, SessionKind, SessionStatus};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use crate::plugins::Registries;
    use crate::store::Store;
    use async_trait::async_trait;
    use serial_test::serial;

    /// Redirects `dirs::data_dir()`/HOME into a tempdir for the duration of a
    /// test, so a cold-resumed chat session's scratch dir never touches the
    /// real state dir. Process-global env — every test using it must be
    /// `#[serial]`.
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

    /// A fake `HarnessSession` that persists the driven prompt as a durable
    /// user turn, mirroring what the real ACP/native session does — so tests
    /// can assert the rail actually delivered the row's payload as a new
    /// user message rather than merely dequeuing it.
    struct FakeSession {
        store: Arc<Store>,
        session_pk: String,
    }

    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn send_prompt(&self, prompt: TurnPrompt) -> anyhow::Result<()> {
            self.store
                .insert_message(NewMessage::block(
                    &self.session_pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": prompt.display }),
                ))
                .await?;
            Ok(())
        }
        async fn cancel(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn end(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn agent_session_id(&self) -> Option<String> {
            Some("agent-1".to_string())
        }
    }

    struct FakeHarness;
    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession {
                store: ctx.store.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }

    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    /// A `HarnessFactory` whose `create()` always fails — models "the
    /// target's turn couldn't start" for the release-on-failure test.
    struct FailingHarnessFactory;
    impl HarnessFactory for FailingHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            anyhow::bail!("boom: harness intentionally fails to start")
        }
    }

    /// A harness whose `start_session` fails only for a configured set of
    /// session pks and otherwise behaves like `FakeHarness` — models "this
    /// one target is permanently broken" without blocking every other
    /// target, for the anti-starvation regression test below.
    /// `registries.harness.create()` runs fresh per session start (see
    /// `ControlPlane::start_harness_session`), so keying the failure inside
    /// `start_session` on `ctx.session_pk` is enough; no cross-session state
    /// is needed in the factory itself.
    struct SelectiveFailHarness {
        fail_for: std::collections::HashSet<String>,
    }
    #[async_trait]
    impl Harness for SelectiveFailHarness {
        async fn start_session(&self, ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            if self.fail_for.contains(&ctx.session_pk) {
                anyhow::bail!(
                    "boom: harness intentionally fails to start for {}",
                    ctx.session_pk
                );
            }
            Ok(Box::new(FakeSession {
                store: ctx.store.clone(),
                session_pk: ctx.session_pk.clone(),
            }))
        }
    }

    struct SelectiveFailHarnessFactory {
        fail_for: std::collections::HashSet<String>,
    }
    impl HarnessFactory for SelectiveFailHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(SelectiveFailHarness {
                fail_for: self.fail_for.clone(),
            }))
        }
    }

    /// A fresh `ControlPlane` backed by a temp sqlite file and the given
    /// harness factory. Returns the temp-file guard too — dropping it deletes
    /// the db file, so the caller must keep it alive for the test's duration.
    async fn control_plane_with(
        harness: Arc<dyn HarnessFactory>,
    ) -> (Arc<ControlPlane>, tempfile::NamedTempFile) {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "test-anthropic".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Test Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        crate::agents::bootstrap::ensure_default_routes(&store)
            .await
            .unwrap();
        let mut regs = Registries::new();
        regs.harness = harness;
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(Arc::clone(&store))
            .await
            .unwrap();
        let cp = ControlPlane::new(store, regs, persistence).await;
        (cp, db)
    }

    /// Insert an idle, project-less chat session directly (bypassing
    /// `start_chat_session`), so it has no live handle in the control plane's
    /// `running` map — the drainer's delivery must cold-resume it via
    /// `continue_session_with_prompt`, exactly like an idle session
    /// rehydrated after a daemon restart.
    async fn idle_chat(cp: &Arc<ControlPlane>, pk: &str) {
        let now = crate::paths::now_ms();
        let primary_agent = cp.registry().default_agent_id().await;
        let primary_agent_snapshot = cp
            .registry()
            .resolved_snapshot(&primary_agent)
            .await
            .unwrap();
        cp.store()
            .insert_session(Session {
                session_pk: pk.into(),
                primary_agent_id: Some(primary_agent),
                primary_agent_snapshot: Some(crate::domain::AgentIdentitySnapshot {
                    id: primary_agent_snapshot.profile.id.clone(),
                    name: primary_agent_snapshot.profile.name.clone(),
                    avatar_color: primary_agent_snapshot.profile.avatar.color.clone(),
                }),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
    }

    /// Poll `store.list_messages` until a row matches `pred`, or panic after
    /// a timeout. Delivery drives the fake session's `send_prompt` on a
    /// `spawn_prompt` background task (fire-and-forget), so it isn't
    /// necessarily visible the instant `tick` returns.
    async fn wait_for_message(store: &Store, session_pk: &str, pred: impl Fn(&Message) -> bool) {
        for _ in 0..400 {
            if store
                .list_messages(session_pk)
                .await
                .unwrap()
                .iter()
                .any(&pred)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for a matching message in {session_pk}");
    }

    #[tokio::test]
    #[serial]
    async fn tick_delivers_a_pending_row_to_an_idle_session() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FakeHarnessFactory)).await;
        idle_chat(&cp, "chat-1").await;
        cp.store()
            .enqueue_background_event("chat-1", "delegation", "RESULT BLOCK")
            .await
            .unwrap();

        tick(&cp).await;

        // The row is consumed (delivered) synchronously inside `tick` —
        // asserting this immediately after does not race the background
        // delivery task.
        assert_eq!(cp.store().pending_background_count().await.unwrap(), 0);
        // The delivered payload landed as a NEW user turn — proving delivery
        // went through `continue_session_with_prompt`, not a raw splice.
        wait_for_message(cp.store(), "chat-1", |m| {
            m.role == "user" && m.payload["text"] == "RESULT BLOCK"
        })
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn tick_skips_a_running_target() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FakeHarnessFactory)).await;
        let now = crate::paths::now_ms();
        cp.store()
            .insert_session(Session {
                session_pk: "busy".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Running,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        cp.store()
            .enqueue_background_event("busy", "delegation", "X")
            .await
            .unwrap();

        tick(&cp).await;

        assert_eq!(
            cp.store().pending_background_count().await.unwrap(),
            1,
            "must not deliver mid-turn"
        );
    }

    #[tokio::test]
    #[serial]
    async fn tick_releases_the_claim_when_delivery_fails() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FailingHarnessFactory)).await;
        idle_chat(&cp, "chat-1").await;
        let id = cp
            .store()
            .enqueue_background_event("chat-1", "delegation", "X")
            .await
            .unwrap();

        tick(&cp).await;

        assert_eq!(
            cp.store().pending_background_count().await.unwrap(),
            1,
            "a failed delivery must not mark the row delivered"
        );
        // The claim must be released, not stuck forever — proven by
        // successfully re-claiming the SAME row for a retry.
        let reclaimed = cp
            .store()
            .claim_deliverable_background_event("retry")
            .await
            .unwrap();
        assert_eq!(
            reclaimed.map(|e| e.id),
            Some(id),
            "a released row must be claimable again"
        );
    }

    /// Pins the anti-starvation fix: a permanently-failing OLDEST row must
    /// not monopolize every claim slot in the tick and starve a newer
    /// target's row.
    ///
    /// `claim_deliverable_background_event` always returns the globally
    /// oldest eligible row. Under the pre-fix code (release-on-failure
    /// INSIDE the loop), the failing older row (`chat-a`) would be released
    /// and immediately re-claimed on every remaining loop iteration — it is
    /// still the oldest — so the newer row (`chat-b`) would never be reached
    /// within this tick and `wait_for_message` below would time out and
    /// panic. With the fix, `chat-a`'s claim is deferred to the end of the
    /// tick, so the very next claim in the loop skips it and reaches
    /// `chat-b`.
    #[tokio::test]
    #[serial]
    async fn tick_does_not_let_a_broken_oldest_target_starve_a_newer_one() {
        let _guard = StateDirGuard::new();
        let mut fail_for = std::collections::HashSet::new();
        fail_for.insert("chat-a".to_string());
        let (cp, _db) =
            control_plane_with(Arc::new(SelectiveFailHarnessFactory { fail_for })).await;
        idle_chat(&cp, "chat-a").await;
        idle_chat(&cp, "chat-b").await;

        // `chat-a` is enqueued FIRST, so it holds the older `created_at` and
        // is the row `claim_deliverable_background_event` picks first.
        let id_a = cp
            .store()
            .enqueue_background_event("chat-a", "delegation", "A")
            .await
            .unwrap();
        // `created_at` has millisecond resolution — sleep past a tick so
        // `chat-b`'s row is unambiguously newer than `chat-a`'s.
        tokio::time::sleep(Duration::from_millis(5)).await;
        cp.store()
            .enqueue_background_event("chat-b", "delegation", "B")
            .await
            .unwrap();

        tick(&cp).await;

        // The newer target (`chat-b`) was delivered within the SAME tick —
        // proving the still-claimed `chat-a` row wasn't re-picked ahead of
        // it.
        wait_for_message(cp.store(), "chat-b", |m| {
            m.role == "user" && m.payload["text"] == "B"
        })
        .await;

        // `chat-a`'s row is released at tick end (deferred, not lost) and
        // still pending — a later tick gets exactly one more attempt at it.
        assert_eq!(
            cp.store().pending_background_count().await.unwrap(),
            1,
            "chat-a's row must still be pending (undelivered), not lost"
        );
        let reclaimed = cp
            .store()
            .claim_deliverable_background_event("retry")
            .await
            .unwrap();
        assert_eq!(
            reclaimed.map(|e| e.id),
            Some(id_a),
            "chat-a's released row must be re-claimable by a later tick"
        );
    }
}
