//! Session lifecycle: start/continue/resume/reconcile/stop/end, plus the
//! harness-session wiring and the background prompt driver.

use super::{ControlPlane, RESUME_NUDGE};
use crate::domain::{AttachmentRef, CoreEvent, PermMode, Project, Session, SessionStatus};
use crate::harness::{HarnessSession, SessionCtx, TurnPrompt};
use crate::paths::{new_id, now_ms, worktree_path_for};
use crate::worktree;
use std::path::Path;
use std::sync::Arc;

impl ControlPlane {
    pub async fn start_session(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        started_by: &str,
        attachments: &[AttachmentRef],
    ) -> anyhow::Result<Session> {
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("daemon is draining for an update; try again shortly");
        }
        let mut project = self
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?;

        // Projects without pinned settings inherit the default agent's
        // configured model / permission mode (Runtime screen → real effect).
        if project.model.is_none() || project.perm_mode == PermMode::Default {
            if let Ok(defaults) = crate::runtimes::session_defaults(&self.store).await {
                if project.model.is_none() {
                    project.model = defaults.model;
                }
                if project.perm_mode == PermMode::Default {
                    if let Some(pm) = defaults.perm_mode {
                        project.perm_mode = pm;
                    }
                }
            }
        }

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
        self.telemetry.count("session.run", vec![]);
        // Sessions run on the local gateway today; its log is the real record.
        let _ = crate::gateways::add_event(
            &self.store,
            "local",
            "info",
            &format!(
                "session {short} started ({} · {})",
                project.harness, project.name
            ),
        )
        .await;

        // Resolve + start the harness session synchronously so an immediate
        // `stop_session` finds a live handle. The prompt is then driven in the
        // background so the cockpit can juggle many sessions concurrently.
        let handle = self
            .start_harness_session(&project, &session_pk, &worktree_path, None)
            .await?;
        let final_prompt = self
            .with_attachments(&session_pk, prompt, attachments)
            .await;
        self.spawn_prompt(
            handle,
            session_pk.clone(),
            TurnPrompt {
                agent: final_prompt,
                display: prompt.to_string(),
            },
        );

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
        attachments: &[AttachmentRef],
    ) -> anyhow::Result<()> {
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("daemon is draining for an update; try again shortly");
        }
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
                let resume = async {
                    let project = self
                        .store
                        .get_project(&session.project_id)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!("unknown project: {}", session.project_id)
                        })?;
                    let work_dir = session
                        .worktree_path
                        .clone()
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.exists())
                        .unwrap_or_else(|| std::path::PathBuf::from(&project.workdir));
                    self.start_harness_session(
                        &project,
                        session_pk,
                        &work_dir,
                        session.agent_session_id.clone(),
                    )
                    .await
                    // `start_harness_session` already persists any resolved
                    // agent_session_id, and `spawn_prompt` persists it post-turn,
                    // so no redundant `update_agent_session_id` is needed here.
                }
                .await;
                match resume {
                    Ok(handle) => handle,
                    Err(e) => {
                        // Roll back the optimistic Running write above —
                        // otherwise a failed resume wedges the session in a
                        // false "running" state with no live handle.
                        let _ = self.store.demote_if_running(session_pk, now_ms()).await;
                        return Err(e);
                    }
                }
            }
        };
        let final_prompt = self.with_attachments(session_pk, prompt, attachments).await;
        self.spawn_prompt(
            handle,
            session_pk.to_string(),
            TurnPrompt {
                agent: final_prompt,
                display: prompt.to_string(),
            },
        );
        Ok(())
    }

    /// Persist a user-visible status row (role=system, block_type=status) and
    /// broadcast it so live subscribers render it immediately.
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
                self.spawn_prompt(
                    handle,
                    session_pk.to_string(),
                    TurnPrompt {
                        agent: RESUME_NUDGE.to_string(),
                        display: RESUME_NUDGE.to_string(),
                    },
                );
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

        // Attach the Apps screen's enabled MCP servers to the session. The MCP
        // per-agent allowlist is keyed by runtime id, which differs from the
        // harness id: the claude-code harness maps to the "claude" runtime;
        // other harnesses (e.g. "native") use their own id.
        let mcp_agent_id = if project.harness == "claude-code" {
            "claude"
        } else {
            project.harness.as_str()
        };
        let mcp_servers = crate::mcp::servers_for_session(&self.store, mcp_agent_id)
            .await
            .unwrap_or_default();
        let ctx = SessionCtx {
            session_pk: session_pk.to_string(),
            work_dir: work_dir.to_path_buf(),
            perm_mode: project.perm_mode,
            model: project.model.clone(),
            effort: project.effort.clone(),
            resume,
            mcp_servers,
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
        prompt: TurnPrompt,
    ) {
        // Panic-safe in-flight turn counter: incremented synchronously here
        // (before the task is spawned, so `drain`/`running_count` never race
        // a not-yet-counted turn) and decremented by `TurnGuard`'s `Drop` as
        // the task's first statement, covering both the Ok/Err result arms
        // below AND a panic mid-turn.
        struct TurnGuard(Arc<ControlPlane>);
        impl Drop for TurnGuard {
            fn drop(&mut self) {
                self.0
                    .active_turns
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.active_turns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let _turn = TurnGuard(Arc::clone(&me));
            let mut span = me
                .telemetry
                .start_span("harness.run", vec![("session_pk", session_pk.clone())]);
            let result = handle.send_prompt(prompt).await;
            if let Err(e) = &result {
                span.set_error(&e.to_string());
                me.telemetry.count("harness.error", vec![]);
            }
            span.end();
            match result {
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
            // Interrupt any in-flight turn first so teardown doesn't race a
            // still-working agent inside the worktree we're about to delete.
            let _ = handle.cancel().await;
            let _ = handle.end().await;
        }
        if let Some(session) = self.store.get_session(session_pk).await? {
            if let Some(project) = self.store.get_project(&session.project_id).await? {
                if let Some(wt) = &session.worktree_path {
                    let short: String = session_pk.chars().take(8).collect();
                    let _ = worktree::remove(
                        Path::new(&project.workdir),
                        &short,
                        session.branch.as_deref(),
                        Path::new(wt),
                    );
                    // Forget the deleted path so a later continue cold-resumes
                    // into the project workdir instead of a dead directory.
                    let _ = self.store.clear_session_worktree(session_pk).await;
                }
            }
        }
        // Best-effort cleanup of any downloaded attachments for this session.
        let _ = tokio::fs::remove_dir_all(self.attachment_dest_dir(session_pk).await).await;
        self.store
            .update_status(session_pk, SessionStatus::Ended, Some(now_ms()))
            .await?;
        let _ = self.events.send(CoreEvent::SessionEnded {
            session_pk: session_pk.to_string(),
        });
        let short: String = session_pk.chars().take(8).collect();
        let _ = crate::gateways::add_event(
            &self.store,
            "local",
            "info",
            &format!("session {short} ended"),
        )
        .await;
        Ok(())
    }
}
