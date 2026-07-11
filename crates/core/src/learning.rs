//! Daemon-hosted learning worker (spec §3.1/§7.2). Claims background-rail
//! rows of `kind='learning'` — enqueued by the delegation/orch producers in
//! Task 7 — that the generic rail drainer (`background_rail.rs`) deliberately
//! skips (`Store::claim_deliverable_background_event` excludes them) so a
//! learning payload is never injected into a chat as a user turn.
//!
//! Each claimed row drives a background review fork
//! (`ControlPlane::run_review_fork`, filled in by Task 9): `kind='review'`,
//! no parent persistence, dispatch-time tool whitelist, budget 16, approvals
//! auto-denied. Unlike the chat rail, `claim_learning_event` has NO
//! idle-target filter — the fork is an isolated session, not a splice into
//! the parent chat's turn, so it must run regardless of the parent's status.
//!
//! A successful run marks the row delivered; a failure releases the claim so
//! a later tick retries it — durable, never dropped, mirroring
//! `background_rail`'s retry contract.

use crate::control::ControlPlane;
use std::sync::Arc;
use std::time::Duration;

/// Poll cadence for the worker loop (mirrors `background_rail`'s 5s cadence).
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Process at most this many rows per tick. Kept small relative to
/// `background_rail::MAX_PER_TICK` (8) — a review fork is a full LLM session,
/// heavier than a chat delivery, so one busy tick shouldn't dispatch too many
/// at once.
const MAX_PER_TICK: usize = 2;

/// One drain pass: claim up to `MAX_PER_TICK` learning rows and run a review
/// fork for each. Factored out of [`run_loop`] so tests can drive it without
/// sleeping.
pub async fn tick(cp: &Arc<ControlPlane>) {
    let store = cp.store();
    // Rows that fail THIS tick are kept claimed until the loop below ends,
    // mirroring `background_rail::tick`'s anti-starvation deferral — a
    // permanently-broken row must not be re-claimed ahead of a different
    // row within the same tick.
    let mut failed_this_tick: Vec<String> = Vec::new();
    for _ in 0..MAX_PER_TICK {
        let event = match store.claim_learning_event("learner").await {
            Ok(Some(event)) => event,
            Ok(None) => break, // nothing to learn from right now
            Err(e) => {
                tracing::warn!("learning: claim failed: {e}");
                break;
            }
        };
        match cp.run_review_fork(&event.payload).await {
            Ok(()) => {
                let _ = store.mark_background_delivered(&event.id).await;
            }
            Err(e) => {
                tracing::warn!("learning: review fork for {} failed: {e}", event.id);
                failed_this_tick.push(event.id);
            }
        }
    }
    for id in failed_this_tick {
        let _ = store.release_background_claim(&id).await;
    }
}

/// The worker's background loop: sleep, then drain a batch, forever.
///
/// Returned as a future (not self-spawned) so hosts can run it on their own
/// runtime, mirroring `background_rail::run_loop` / `scheduler::run_loop` /
/// `orch::run_loop`.
pub async fn run_loop(cp: Arc<ControlPlane>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        tick(&cp).await;
    }
}

/// Spawn the worker on the host's runtime (mirrors
/// `background_rail::spawn_runner` — the daemon is the single always-on
/// engine host for every one of these background loops).
pub fn spawn_runner(cp: Arc<ControlPlane>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(cp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PermMode, Session, SessionKind, SessionStatus};
    use crate::harness::native::llm::{LlmStream, LlmStreamFactory};
    use crate::harness::native::runner::testutil::{
        message_delta, message_stop, text_delta, ScriptedLlm,
    };
    use crate::harness::native::runner::LearningPayload;
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx, TurnPrompt};
    use crate::plugins::Registries;
    use crate::store::Store;
    use async_trait::async_trait;
    use serial_test::serial;

    /// Always hands out the SAME scripted stream regardless of `store` — the
    /// worker's `run_review_fork` builds its own `RunnerDeps.llm` via
    /// `ControlPlane::review_llm_factory()`, bypassing `Registries.harness`
    /// entirely (see that field's doc in `control.rs`), so these tests inject
    /// through `ControlPlane::set_review_llm_factory_for_test` instead of the
    /// `FakeHarnessFactory` below.
    struct FixedLlmFactory(Arc<dyn LlmStream>);
    impl LlmStreamFactory for FixedLlmFactory {
        fn create(&self, _store: Arc<Store>) -> Arc<dyn LlmStream> {
            self.0.clone()
        }
    }

    /// A review-fork LLM factory that always replies with one plain end_turn
    /// (no tool calls) — enough to drive `run_review_fork` to completion
    /// without touching the network.
    fn scripted_review_factory(text: &str) -> Arc<dyn LlmStreamFactory> {
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![vec![
            text_delta(text),
            message_delta("end_turn"),
            message_stop(),
        ]]));
        Arc::new(FixedLlmFactory(llm))
    }

    /// A minimal, valid `LearningPayload` JSON string targeting `parent_pk` —
    /// enough for `run_review_fork` to decode and drive without erroring
    /// (these tests care about the claim → dispatch → mark-delivered
    /// skeleton, not the payload's content).
    fn learning_payload_json(parent_pk: &str) -> String {
        let payload = LearningPayload {
            review_kind: "memory".into(),
            parent_session_pk: parent_pk.into(),
            model: "test/model".into(),
            supports_prompt_cache: false,
            system: "You are ryuzi.".into(),
            tool_defs: vec![],
            messages: vec![
                serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            ],
        };
        serde_json::to_string(&payload).unwrap()
    }

    /// Redirects `dirs::data_dir()`/HOME into a tempdir for the duration of a
    /// test — see `background_rail::tests::StateDirGuard`'s doc for why.
    /// Process-global env — every test using it must be `#[serial]`.
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

    /// A minimal harness: this module's tests never dispatch a real turn
    /// through it — the learning worker never calls into the harness
    /// registry directly; `run_review_fork` drives the native runner
    /// straight, via `ControlPlane::review_llm_factory()` (see
    /// `scripted_review_factory`, above) — but `control_plane_with` needs
    /// some `HarnessFactory` to construct a `ControlPlane`.
    struct FakeSession;
    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn send_prompt(&self, _prompt: TurnPrompt) -> anyhow::Result<()> {
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
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession))
        }
    }

    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    /// A fresh `ControlPlane` backed by a temp sqlite file and the given
    /// harness factory. Returns the temp-file guard too — dropping it deletes
    /// the db file, so the caller must keep it alive for the test's duration.
    async fn control_plane_with(
        harness: Arc<dyn HarnessFactory>,
    ) -> (Arc<ControlPlane>, tempfile::NamedTempFile) {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        let mut regs = Registries::new();
        regs.harness = harness;
        let cp = ControlPlane::new(store, regs).await;
        (cp, db)
    }

    /// Insert an idle, project-less chat session directly, mirroring
    /// `background_rail::tests::idle_chat`.
    async fn idle_chat(cp: &Arc<ControlPlane>, pk: &str) {
        let now = crate::paths::now_ms();
        cp.store()
            .insert_session(Session {
                session_pk: pk.into(),
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

    /// The Task 8 rail split, end to end through the real `ControlPlane` /
    /// `Store` pair: a `learning` row is invisible to the generic chat
    /// drainer's claim and is claimed by the dedicated learning claim
    /// instead.
    #[tokio::test]
    #[serial]
    async fn generic_drainer_skips_learning_rows_and_worker_claims_them() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FakeHarnessFactory)).await;
        idle_chat(&cp, "chat-1").await;
        cp.store()
            .enqueue_background_event("chat-1", "learning", "{}")
            .await
            .unwrap();

        // The generic rail must NOT pick a learning row up.
        assert!(cp
            .store()
            .claim_deliverable_background_event("drainer")
            .await
            .unwrap()
            .is_none());
        // The learning worker's dedicated claim DOES.
        let ev = cp
            .store()
            .claim_learning_event("learner")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ev.kind, "learning");
    }

    /// `tick` claims a learning row, drives the review fork (Task 9) to
    /// completion against a scripted end_turn, and marks the row delivered —
    /// proving the worker's claim → dispatch → mark-delivered skeleton is
    /// wired end to end.
    #[tokio::test]
    #[serial]
    async fn tick_claims_and_delivers_a_learning_row() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FakeHarnessFactory)).await;
        cp.set_review_llm_factory_for_test(scripted_review_factory("nothing to add"));
        idle_chat(&cp, "chat-1").await;
        cp.store()
            .enqueue_background_event("chat-1", "learning", &learning_payload_json("chat-1"))
            .await
            .unwrap();

        tick(&cp).await;

        assert_eq!(cp.store().pending_background_count().await.unwrap(), 0);
    }

    /// A `learning` fork runs even while its parent chat is RUNNING (not
    /// idle) — proving `claim_learning_event` has no idle-target filter,
    /// unlike the chat rail's `claim_deliverable_background_event`.
    #[tokio::test]
    #[serial]
    async fn tick_delivers_a_learning_row_even_when_the_parent_session_is_running() {
        let _guard = StateDirGuard::new();
        let (cp, _db) = control_plane_with(Arc::new(FakeHarnessFactory)).await;
        cp.set_review_llm_factory_for_test(scripted_review_factory("nothing to add"));
        let now = crate::paths::now_ms();
        cp.store()
            .insert_session(Session {
                session_pk: "busy-1".into(),
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
            .enqueue_background_event("busy-1", "learning", &learning_payload_json("busy-1"))
            .await
            .unwrap();

        tick(&cp).await;

        assert_eq!(
            cp.store().pending_background_count().await.unwrap(),
            0,
            "a learning fork must run regardless of the parent session's status"
        );
    }
}
