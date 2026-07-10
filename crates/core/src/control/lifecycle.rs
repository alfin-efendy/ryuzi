//! Session lifecycle: start/continue/resume/reconcile/stop/end, plus the
//! harness-session wiring and the background prompt driver.

use super::{ControlPlane, RESUME_NUDGE};
use crate::connector::ConnectorCtx;
use crate::domain::{
    AttachmentRef, CoreEvent, PermMode, Project, Session, SessionGitOptions, SessionKind,
    SessionStatus,
};
use crate::harness::{HarnessSession, SessionCtx, TurnPrompt};
use crate::paths::{new_id, now_ms, worktree_path_for};
use crate::settings::SettingsStore;
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
        self.start_session_with_prompt(
            project_id,
            TurnPrompt::text(prompt, prompt),
            started_by,
            attachments,
            None,
        )
        .await
    }

    pub async fn start_session_with_prompt(
        self: &Arc<Self>,
        project_id: &str,
        prompt: TurnPrompt,
        started_by: &str,
        attachments: &[AttachmentRef],
        git: Option<SessionGitOptions>,
    ) -> anyhow::Result<Session> {
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("daemon is draining for an update; try again shortly");
        }
        let mut project = self
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?;

        // A project without a pinned MODEL inherits THIS session's runtime
        // config (Runtime screen → real effect), keyed off the project's own
        // harness — otherwise a native session would inherit the Claude card's
        // model and every turn would hit the Claude subscription.
        //
        // Permission mode is NOT inherited from the runtime card: the project's
        // own `perm_mode` (set from the composer / project settings) is the
        // single source of truth. `Default` means "Ask" (prompt before
        // edits/commands) — inheriting the card's default (e.g. "Full") here is
        // exactly what made a project set to Ask silently run without asking.
        if project.model.is_none() {
            let runtime_id = crate::runtimes::runtime_id_for_harness(&project.harness);
            if let Ok(defaults) =
                crate::runtimes::session_defaults_for(&self.store, runtime_id).await
            {
                project.model = defaults.model;
            }
        }

        // Cheap validation only — the session row must be returnable
        // immediately. Anything disk- or process-heavy runs in the background
        // startup task below and surfaces failures in the transcript.
        if self.registries.harness.get(&project.harness).is_none() {
            anyhow::bail!(
                "unknown harness '{}' (registered: {:?})",
                project.harness,
                self.registries.harness.names()
            );
        }
        let git = git.unwrap_or_default();
        // Git options (branch name / worktree) only apply to git projects; a
        // plain folder runs in-place with no branch, so skip the branch-name
        // check for it.
        if project.is_git {
            if let Some(name) = git.branch_name.as_deref() {
                crate::workspace::validate_branch_name(name)?;
            }
        }

        let session_pk = new_id();
        let short: String = session_pk.chars().take(8).collect();
        let now = now_ms();
        let title: String = prompt.display.chars().take(80).collect();
        // Workspace columns are provisional: for a git project the background
        // git prep backfills the real values (engine-generated names,
        // current-branch resolution) via `update_session_workspace`. A non-git
        // project skips prep entirely and carries no branch/worktree regardless
        // of any git options passed.
        let session = Session {
            session_pk: session_pk.clone(),
            project_id: Some(project.project_id.clone()),
            agent_session_id: None,
            worktree_path: None,
            branch: if project.is_git {
                git.branch_name.clone().or_else(|| git.base_branch.clone())
            } else {
                None
            },
            title: Some(title),
            status: SessionStatus::Running,
            started_by: Some(started_by.to_string()),
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: project.is_git && git.create_branch && git.branch_name.is_none(),
            kind: SessionKind::Project,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        };
        self.store.insert_session(session.clone()).await?;
        let _ = self.events.send(CoreEvent::SessionCreated {
            session_pk: session_pk.clone(),
            project_id: Some(project.project_id.clone()),
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

        // Everything slow — git prep, harness + MCP startup, the first prompt
        // — runs in the background, streaming progress into the transcript.
        let me = Arc::clone(self);
        let attachments = attachments.to_vec();
        tokio::spawn(async move {
            me.run_session_startup(project, session_pk, git, prompt, attachments)
                .await;
        });

        Ok(session)
    }

    /// Start a project-less (`kind = Chat`) session: no project, no git prep,
    /// no worktree. Its "workspace" is a managed scratch dir
    /// (`paths::chat_scratch_dir`) created on first use. Modeled on
    /// `start_session_with_prompt`, minus everything project/git-specific.
    pub async fn start_chat_session(
        self: &Arc<Self>,
        prompt: TurnPrompt,
        started_by: &str,
        attachments: &[AttachmentRef],
    ) -> anyhow::Result<Session> {
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("daemon is draining for an update; try again shortly");
        }
        let session_pk = new_id();
        let short: String = session_pk.chars().take(8).collect();
        let now = now_ms();
        let title: String = prompt.display.chars().take(80).collect();
        let session = Session {
            session_pk: session_pk.clone(),
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: Some(title),
            status: SessionStatus::Running,
            started_by: Some(started_by.to_string()),
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: false,
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        };
        self.store.insert_session(session.clone()).await?;
        let _ = self.events.send(CoreEvent::SessionCreated {
            session_pk: session_pk.clone(),
            project_id: None,
        });
        self.telemetry.count("session.run", vec![]);
        let _ = crate::gateways::add_event(
            &self.store,
            "local",
            "info",
            &format!("chat session {short} started"),
        )
        .await;

        // Everything slow — harness + MCP startup, the first prompt — runs in
        // the background, streaming progress into the transcript, exactly
        // like a project session's startup.
        let me = Arc::clone(self);
        let attachments = attachments.to_vec();
        tokio::spawn(async move {
            me.run_chat_startup(session_pk, prompt, attachments).await;
        });

        Ok(session)
    }

    /// Background half of `start_chat_session`. Mirrors
    /// `run_session_startup`'s `starting`-token bookkeeping so a stop/end
    /// that lands mid-startup cancels cleanly, the same as a project session.
    async fn run_chat_startup(
        self: Arc<Self>,
        session_pk: String,
        prompt: TurnPrompt,
        attachments: Vec<AttachmentRef>,
    ) {
        let cancel = tokio_util::sync::CancellationToken::new();
        self.starting
            .lock()
            .unwrap()
            .insert(session_pk.clone(), cancel.clone());
        self.chat_startup_phases(&session_pk, prompt, attachments, &cancel)
            .await;
        self.starting.lock().unwrap().remove(&session_pk);
    }

    /// The chat-session startup phases: create the scratch dir → harness +
    /// MCP → first prompt. No git/workspace prep — there is no project or
    /// worktree — so this is a trimmed-down `startup_phases`.
    async fn chat_startup_phases(
        self: &Arc<Self>,
        session_pk: &str,
        prompt: TurnPrompt,
        attachments: Vec<AttachmentRef>,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        let work_dir = crate::paths::chat_scratch_dir(session_pk);
        if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
            self.fail_startup(
                session_pk,
                &format!("Couldn't prepare the chat workspace: {e}"),
            )
            .await;
            return;
        }
        if cancel.is_cancelled() {
            return;
        }

        self.emit_status(session_pk, "Connecting tools…").await;
        let handle = match self
            .start_harness_session(None, session_pk, &work_dir, None)
            .await
        {
            Ok(handle) => handle,
            Err(e) => {
                self.fail_startup(session_pk, &format!("Couldn't start the agent: {e}"))
                    .await;
                return;
            }
        };

        if cancel.is_cancelled() {
            let _ = handle.cancel().await;
            return;
        }
        let prepared = self
            .prepare_attachments(session_pk, &prompt.agent, &attachments)
            .await;
        self.spawn_prompt(
            handle,
            session_pk.to_string(),
            TurnPrompt {
                agent: prepared.agent,
                display: prompt.display,
                blocks: prepared.image_blocks,
                attachments: prepared.attachments_meta,
            },
        );
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
        self.continue_session_with_prompt(session_pk, TurnPrompt::text(prompt, prompt), attachments)
            .await
    }

    pub async fn continue_session_with_prompt(
        self: &Arc<Self>,
        session_pk: &str,
        prompt: TurnPrompt,
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

        // A session still in background startup has no live handle yet, and its
        // FIRST prompt hasn't been driven. Cold-resuming now would spawn a
        // SECOND harness (in `project.workdir` while `worktree_path` is still
        // provisional), run this follow-up ahead of the first prompt, and
        // orphan the handle the startup task later registers. Wait for startup
        // to settle, then fall through to the fast path onto its live handle
        // (or, if startup failed, the normal cold-resume/error path below).
        if self.starting.lock().unwrap().contains_key(session_pk) {
            self.wait_for_startup(session_pk).await;
        }

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
                // A chat (project-less) session has no project to resolve — it
                // cold-resumes straight into its managed scratch dir.
                let resume = async {
                    let project = match session.project_id.as_deref() {
                        Some(project_id) => Some(
                            self.store
                                .get_project(project_id)
                                .await?
                                .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?,
                        ),
                        None => None,
                    };
                    let work_dir = session
                        .worktree_path
                        .clone()
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.exists())
                        .unwrap_or_else(|| match &project {
                            Some(p) => std::path::PathBuf::from(&p.workdir),
                            None => crate::paths::chat_scratch_dir(session_pk),
                        });
                    if project.is_none() {
                        let _ = tokio::fs::create_dir_all(&work_dir).await;
                    }
                    self.start_harness_session(
                        project.as_ref(),
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
        // Refresh the live session's permission mode from the project row so a
        // change made in the composer/project settings between turns takes
        // effect NOW — without this the warm handle keeps whatever mode it
        // started with (ACP delegates permission externally, so its default
        // no-op set_perm_mode simply does nothing).
        if let Some(project_id) = session.project_id.as_deref() {
            if let Ok(Some(project)) = self.store.get_project(project_id).await {
                handle.set_perm_mode(project.perm_mode);
            }
        }
        let prepared = self
            .prepare_attachments(session_pk, &prompt.agent, attachments)
            .await;
        self.spawn_prompt(
            handle,
            session_pk.to_string(),
            TurnPrompt {
                agent: prepared.agent,
                display: prompt.display,
                blocks: prepared.image_blocks,
                attachments: prepared.attachments_meta,
            },
        );
        Ok(())
    }

    /// Mid-turn steering (Task B3): inject `text` into a LIVE turn's next
    /// tool-result batch instead of racing a whole new turn onto the session.
    /// Looks up the live handle in `running` and calls
    /// `HarnessSession::steer` — this never bypasses the turn lock or starts
    /// a new turn, it only queues for whatever turn that handle is (or will
    /// be) running to pick up on its own next iteration.
    ///
    /// Returns `true` when a live handle received it. When the session has no
    /// live handle (ended, never started, or the in-memory handle was lost to
    /// a restart), there is no in-flight turn to steer into at all, so this
    /// falls back to ordinary `continue_session` semantics — the text starts
    /// a fresh turn — and returns `false`.
    pub async fn steer_session(
        self: &Arc<Self>,
        session_pk: &str,
        text: &str,
    ) -> anyhow::Result<bool> {
        let handle = self.running.lock().unwrap().get(session_pk).cloned();
        match handle {
            Some(handle) => {
                handle.steer(text.to_string());
                Ok(true)
            }
            None => {
                self.continue_session(session_pk, text, &[]).await?;
                Ok(false)
            }
        }
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

    /// Persist a user-visible error row (role=system, block_type=error) and
    /// broadcast it so live subscribers render it immediately.
    async fn emit_error(&self, session_pk: &str, text: &str) {
        let payload = serde_json::json!({ "message": text });
        let msg = crate::domain::NewMessage::block(session_pk, "system", "error", payload.clone());
        if let Ok(seq) = self.store.insert_message(msg).await {
            let _ = self.events.send(CoreEvent::Message {
                session_pk: session_pk.to_string(),
                seq,
                role: "system".to_string(),
                block_type: "error".to_string(),
                payload,
                tool_call_id: None,
                status: None,
                tool_kind: None,
            });
        }
    }

    /// Background half of `start_session_with_prompt`. Registers a
    /// cancellation token in `starting` for the duration of the phases so a
    /// stop/end that lands mid-startup can abort them cleanly.
    async fn run_session_startup(
        self: Arc<Self>,
        project: Project,
        session_pk: String,
        git: SessionGitOptions,
        prompt: TurnPrompt,
        attachments: Vec<AttachmentRef>,
    ) {
        let cancel = tokio_util::sync::CancellationToken::new();
        self.starting
            .lock()
            .unwrap()
            .insert(session_pk.clone(), cancel.clone());
        self.startup_phases(&project, &session_pk, git, prompt, attachments, &cancel)
            .await;
        self.starting.lock().unwrap().remove(&session_pk);
    }

    /// The startup phases proper: git prep → harness + MCP → first prompt,
    /// streaming progress into the transcript as status rows. The copy
    /// varies by git cell — an in-place or existing-branch session must not
    /// claim a worktree/branch was created. Failures emit an error row,
    /// demote the session to Idle, and broadcast the bus-terminal error —
    /// the row persists so the user can retry from the same chat.
    // `pub(super)` (not private) so `control::tests` can drive this phase
    // function directly with a pre-cancelled token — the non-git path's
    // pre-harness checkpoint sits before any `.await`, so a `stop_session()`
    // call from a test can never deterministically land inside that window
    // on this crate's current-thread `#[tokio::test]` runtime (there is no
    // scheduling opportunity between registering the cancellation token and
    // evaluating the checkpoint). Calling the phase directly with the token
    // already cancelled is the only reliable way to pin it.
    pub(super) async fn startup_phases(
        self: &Arc<Self>,
        project: &Project,
        session_pk: &str,
        git: SessionGitOptions,
        prompt: TurnPrompt,
        attachments: Vec<AttachmentRef>,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        // Non-git projects skip all git prep — no worktree, no branch — and run
        // the harness directly in the project workdir. Git projects run the
        // full branch-controls prep and backfill the workspace columns.
        let work_dir = if project.is_git {
            // Captured before `git` moves into the spawn_blocking closure below.
            let (use_worktree, create_branch) = (git.use_worktree, git.create_branch);
            self.emit_status(
                session_pk,
                match (use_worktree, create_branch) {
                    (true, _) => "Creating worktree…",
                    (false, true) => "Creating branch…",
                    (false, false) => "Preparing workspace…",
                },
            )
            .await;
            let worktree_candidate = worktree_path_for(&project.project_id, session_pk);
            let repo_dir = std::path::PathBuf::from(&project.workdir);
            let prep_pk = session_pk.to_string();
            let prep_git = git;
            // git2 is synchronous, disk-heavy work — keep it off the async runtime.
            let prep = tokio::task::spawn_blocking(move || {
                crate::workspace::prepare_session_workspace(
                    &repo_dir,
                    &prep_git,
                    &prep_pk,
                    &worktree_candidate,
                )
            })
            .await;
            let ws = match prep {
                Ok(Ok(ws)) => ws,
                Ok(Err(e)) => {
                    self.fail_startup(
                        session_pk,
                        &format!("Couldn't prepare the git workspace: {e}"),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    self.fail_startup(session_pk, &format!("Workspace preparation failed: {e}"))
                        .await;
                    return;
                }
            };
            let _ = self
                .store
                .update_session_workspace(
                    session_pk,
                    ws.worktree_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    &ws.branch,
                    ws.branch_owned,
                )
                .await;
            // Cancelled during git prep: the workspace columns are persisted, so
            // the end_session that cancelled us (it waits for this task to unwind
            // before teardown) reads the real worktree path and cleans it up; a
            // plain stop leaves the workspace in place for a later retry or end.
            if cancel.is_cancelled() {
                return;
            }
            self.emit_status(
                session_pk,
                &match (use_worktree, create_branch) {
                    (_, true) => format!("Created and checked out branch {}", ws.branch),
                    (true, false) => format!("Checked out branch {}", ws.branch),
                    (false, false) => format!("Using branch {}", ws.branch),
                },
            )
            .await;
            ws.work_dir
        } else {
            std::path::PathBuf::from(&project.workdir)
        };

        // Unconditional pre-harness cancel checkpoint, common to BOTH paths.
        // For a git session this is redundant with the checkpoint above (git
        // prep already re-checked `cancel` before returning `work_dir`), but
        // for a non-git session this is the ONLY checkpoint before the
        // harness is started — the `else` branch above does no prep at all,
        // so without this a stop landing right after startup begins would
        // still spawn the harness (unlike a git session with identical
        // timing, caught above).
        if cancel.is_cancelled() {
            return;
        }

        self.emit_status(session_pk, "Connecting tools…").await;
        let handle = match self
            .start_harness_session(Some(project), session_pk, &work_dir, None)
            .await
        {
            Ok(handle) => handle,
            Err(e) => {
                self.fail_startup(session_pk, &format!("Couldn't start the agent: {e}"))
                    .await;
                return;
            }
        };

        // Stopped while the harness was starting: the handle stays parked in
        // `running` (the normal post-stop state) — just don't drive the turn.
        if cancel.is_cancelled() {
            let _ = handle.cancel().await;
            return;
        }
        let prepared = self
            .prepare_attachments(session_pk, &prompt.agent, &attachments)
            .await;
        self.spawn_prompt(
            handle,
            session_pk.to_string(),
            TurnPrompt {
                agent: prepared.agent,
                display: prompt.display,
                blocks: prepared.image_blocks,
                attachments: prepared.attachments_meta,
            },
        );
    }

    /// Startup failed: surface it in the transcript, release the session
    /// back to Idle so the user can retry, and broadcast the bus-terminal
    /// `CoreEvent::Error` (mirroring `spawn_prompt`'s error arm). The
    /// broadcast is load-bearing: the orchestrator's `watch_session` and the
    /// scheduler's run watcher finish only on `Result`/`Error` for the
    /// session, so without it they would hang to their 2h deadline instead
    /// of reporting the real git/harness error. `demote_if_running` (not a
    /// blind status write) so a stop that already marked it Interrupted
    /// wins; it runs before the broadcast so a lagged watcher that falls
    /// back to consulting the session row never reads a stale Running.
    async fn fail_startup(&self, session_pk: &str, message: &str) {
        self.emit_error(session_pk, message).await;
        let _ = self.store.demote_if_running(session_pk, now_ms()).await;
        let _ = self.events.send(CoreEvent::Error {
            session_pk: session_pk.to_string(),
            message: message.to_string(),
        });
    }

    /// Re-drive an interrupted turn after a restart, guarded by the attempts
    /// cap so a session that reliably crashes the daemon cannot loop forever.
    ///
    /// A chat (project-less) session resumes the same as a project session —
    /// only its workspace resolution differs (the managed scratch dir instead
    /// of a project workdir/worktree). Earlier this bailed out for any
    /// project-less session, silently leaving a crash-interrupted chat turn
    /// stuck Running forever; that gap is closed here.
    pub async fn resume_session(
        self: &Arc<Self>,
        session_pk: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let Some(session) = self.store.get_session(session_pk).await? else {
            return Ok(());
        };
        let project = match session.project_id.as_deref() {
            Some(project_id) => match self.store.get_project(project_id).await? {
                Some(project) => Some(project),
                // The bound project is gone — nothing sane to resume into.
                None => return Ok(()),
            },
            None => None,
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
            .unwrap_or_else(|| match &project {
                Some(p) => std::path::PathBuf::from(&p.workdir),
                None => crate::paths::chat_scratch_dir(session_pk),
            });
        if project.is_none() {
            // Chat sessions have no worktree — make sure the managed scratch
            // dir still exists before the harness starts in it.
            let _ = tokio::fs::create_dir_all(&work_dir).await;
        }
        match self
            .start_harness_session(
                project.as_ref(),
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
                    TurnPrompt::text(RESUME_NUDGE, RESUME_NUDGE),
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

    /// Resolve the harness in the registry, build a `SessionCtx`, and start
    /// the session. Records the returned handle in the `running` map and
    /// returns a clone for driving the first prompt.
    ///
    /// `project` is `None` for a chat (project-less) session — there is no
    /// `Project` row to inherit `harness`/`perm_mode`/`model`/`effort` from,
    /// so those fall back to engine-wide settings: `perm_mode` from the
    /// `default_perm_mode` setting (the same fallback a brand-new project
    /// gets at `connect_project` time), `model` from the resolved runtime's
    /// own persisted config (mirrors how a project without a pinned model
    /// inherits its runtime card, above in `start_session_with_prompt`), and
    /// `effort` from `default_effort`. The harness/mcp-agent id itself comes
    /// from the `default_agent` setting (same fallback `native` that
    /// `session_defaults` uses), mapped onto the harness registry's id space.
    async fn start_harness_session(
        self: &Arc<Self>,
        project: Option<&Project>,
        session_pk: &str,
        work_dir: &Path,
        resume: Option<String>,
    ) -> anyhow::Result<Arc<dyn HarnessSession>> {
        let settings = SettingsStore::new(self.store.clone());
        let (harness_id, perm_mode, model, effort): (
            String,
            PermMode,
            Option<String>,
            Option<String>,
        ) = match project {
            Some(p) => (
                p.harness.clone(),
                p.perm_mode,
                p.model.clone(),
                p.effort.clone(),
            ),
            None => {
                let runtime_id = settings
                    .get("default_agent")
                    .await
                    .ok()
                    .flatten()
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| "native".to_string());
                // Runtime ids and harness ids diverge only for Claude:
                // runtime "claude" ⇒ harness "claude-code" (the inverse of
                // `runtimes::runtime_id_for_harness`).
                let harness_id = if runtime_id == "claude" {
                    "claude-code".to_string()
                } else {
                    runtime_id.clone()
                };
                let default_perm_raw = settings
                    .get("default_perm_mode")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "default".to_string());
                let perm_mode = PermMode::from_db(&default_perm_raw);
                let model = crate::runtimes::session_defaults_for(&self.store, &runtime_id)
                    .await
                    .ok()
                    .and_then(|d| d.model);
                let effort = settings
                    .get("default_effort")
                    .await
                    .ok()
                    .flatten()
                    .filter(|v| !v.trim().is_empty());
                (harness_id, perm_mode, model, effort)
            }
        };

        let factory = self.registries.harness.get(&harness_id).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown harness '{harness_id}' (registered: {:?})",
                self.registries.harness.names()
            )
        })?;
        let harness = factory.create()?;

        // Attach the Apps screen's enabled MCP servers to the session. The MCP
        // per-agent allowlist is keyed by runtime id, which differs from the
        // harness id: the claude-code harness maps to the "claude" runtime;
        // other harnesses (e.g. "native") use their own id.
        let mcp_agent_id = if harness_id == "claude-code" {
            "claude"
        } else {
            harness_id.as_str()
        };
        let mut mcp_servers = crate::mcp::servers_for_session(&self.store, mcp_agent_id)
            .await
            .unwrap_or_default();
        // A chat session has no project to scope plugin connectors by —
        // `ConnectorCtx.project_id` isn't read by any connector today, so the
        // session id is a harmless, uniquely-scoped stand-in.
        let scope_id = project.map(|p| p.project_id.as_str()).unwrap_or(session_pk);
        self.attach_plugin_mcp_servers(scope_id, work_dir, &settings, &mut mcp_servers)
            .await;
        let extra_skill_dirs = self.registries.plugins.enabled_skill_dirs(&settings).await;
        // `kind`/`agent` come from the session row rather than a caller
        // parameter — every caller of `start_harness_session` (fresh start,
        // cold-resume, crash-resume) has already inserted the row before
        // reaching here, so this is a reliable single source of truth. A
        // missing row (shouldn't happen in practice) falls back to the kind
        // implied by whether a project was resolved.
        let session_row = self.store.get_session(session_pk).await.ok().flatten();
        let kind = session_row
            .as_ref()
            .map(|s| s.kind)
            .unwrap_or(if project.is_some() {
                SessionKind::Project
            } else {
                SessionKind::Chat
            });
        let agent = session_row.and_then(|s| s.agent);
        let ctx = SessionCtx {
            session_pk: session_pk.to_string(),
            project_id: project.map(|p| p.project_id.clone()),
            kind,
            agent,
            work_dir: work_dir.to_path_buf(),
            perm_mode,
            model,
            effort,
            resume,
            mcp_servers,
            extra_skill_dirs,
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

    /// Extend `mcp_servers` with the MCP servers of every enabled,
    /// connector-capable plugin (`registries.plugins`). A DB-configured
    /// server (already in `mcp_servers`) wins over a plugin server with the
    /// same name — the plugin's entry is dropped rather than overriding it.
    /// A connector that fails to enable, fails `ensure_auth` (e.g. missing
    /// credential — logged with its friendly, secret-free message), or
    /// fails to resolve its servers is logged via `tracing::warn!` and
    /// skipped: a broken plugin integration must never prevent a session
    /// from starting.
    async fn attach_plugin_mcp_servers(
        &self,
        project_id: &str,
        work_dir: &Path,
        settings: &SettingsStore,
        mcp_servers: &mut Vec<crate::domain::McpServerSpec>,
    ) {
        let mut names: std::collections::HashSet<String> =
            mcp_servers.iter().map(|s| s.name.clone()).collect();
        for plugin in self.registries.plugins.list() {
            let Some(connector) = &plugin.connector else {
                continue;
            };
            let id = &plugin.manifest.id;
            match self.registries.plugins.is_enabled(settings, id).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!(plugin = %id, "plugin connector failed: {e}");
                    continue;
                }
            }
            let ctx = ConnectorCtx {
                project_id: project_id.to_string(),
                work_dir: work_dir.to_path_buf(),
                settings: settings.clone(),
            };
            if let Err(e) = connector.ensure_auth(&ctx).await {
                tracing::warn!(plugin = %id, "plugin connector not ready: {e}");
                continue;
            }
            match connector.mcp_servers(&ctx).await {
                Ok(specs) => {
                    for spec in specs {
                        if !names.insert(spec.name.clone()) {
                            continue; // a DB-configured (or earlier plugin's) server wins
                        }
                        mcp_servers.push(spec);
                    }
                }
                Err(e) => {
                    tracing::warn!(plugin = %id, "plugin connector failed: {e}");
                }
            }
        }
    }

    /// Drive a prompt on `handle` in the background. `send_prompt` blocks until
    /// the turn completes (ACP `EndTurn`); on completion we atomically demote
    /// `Running → Idle` (unless the session was already Interrupted/Ended) and
    /// broadcast a `Result`. Errors are persisted as a durable error row
    /// (via `emit_error`), the row is demoted Running→Idle, and only then
    /// does the bus-terminal `CoreEvent::Error` fire — mirroring
    /// `fail_startup`'s ordering.
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
                    let message = e.to_string();
                    // Persist the turn error as a DURABLE transcript row
                    // (role=system, block_type=error) so it survives an app
                    // reload — the bus-terminal broadcast below is transient.
                    // emit_error also re-broadcasts the row as a normal
                    // Message event, which is what live UIs render (the UI's
                    // "error" handler no longer appends its own transient
                    // copy). Mirrors `fail_startup`.
                    me.emit_error(&session_pk, &message).await;
                    // Demote BEFORE the broadcast so a subscriber that
                    // refreshes on Error (the UI does) never reads a stale
                    // Running row.
                    let _ = me.store.demote_if_running(&session_pk, now_ms()).await;
                    let _ = me.events.send(CoreEvent::Error {
                        session_pk: session_pk.clone(),
                        message,
                    });
                }
            }
        });
    }

    /// Bounded wait (2 min — covers even a slow worktree checkout) for a
    /// session's in-flight background startup task to deregister from
    /// `starting`. The startup task always deregisters on every path (its
    /// phases return rather than panic), so this normally returns promptly; if
    /// it is somehow wedged we return best-effort rather than blocking the
    /// caller forever. Callers that need the startup ABORTED must cancel its
    /// token first (see `end_session`); `continue_session_with_prompt` instead
    /// lets it finish so the follow-up lands on the live handle it registers.
    async fn wait_for_startup(&self, session_pk: &str) {
        for _ in 0..2400 {
            if !self.starting.lock().unwrap().contains_key(session_pk) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    pub async fn stop_session(&self, session_pk: &str) -> anyhow::Result<()> {
        // A session still in background startup has no live handle yet —
        // cancel its startup task; it checks the token between phases.
        if let Some(token) = self.starting.lock().unwrap().get(session_pk) {
            token.cancel();
        }
        let handle = self.running.lock().unwrap().get(session_pk).cloned();
        if let Some(handle) = handle {
            let _ = handle.cancel().await;
        }
        // Deny any approval prompts still parked for this session so a
        // stopped turn settles (pairing its tool_use with an error
        // tool_result) instead of waiting forever on an answer that will
        // never come. The native gate also observes the turn token; this
        // covers the hub side and clears stale prompts.
        self.approvals.resolve_session(session_pk, false);
        self.store
            .update_status(session_pk, SessionStatus::Interrupted, Some(now_ms()))
            .await?;
        Ok(())
    }

    /// Tear down a session. This is the ONLY place the persistent live-session
    /// handle is removed from `running` and `end()`ed (graceful ACP teardown),
    /// after which the worktree is cleaned up and the session marked `Ended`.
    pub async fn end_session(&self, session_pk: &str) -> anyhow::Result<()> {
        // Abort any in-flight background startup and WAIT for it to unwind
        // before tearing down: the teardown below must read the FINAL
        // workspace columns (git prep backfills them at its checkpoint), or
        // an end that races git prep would see worktree_path=NULL, skip the
        // worktree cleanup, and leak the just-created directory + branch.
        let starting_token = self.starting.lock().unwrap().get(session_pk).cloned();
        if let Some(token) = starting_token {
            token.cancel();
            self.wait_for_startup(session_pk).await;
        }
        let handle = self.running.lock().unwrap().remove(session_pk);
        if let Some(handle) = handle {
            // Interrupt any in-flight turn first so teardown doesn't race a
            // still-working agent inside the worktree we're about to delete.
            let _ = handle.cancel().await;
            let _ = handle.end().await;
        }
        if let Some(session) = self.store.get_session(session_pk).await? {
            match session.project_id.as_deref() {
                Some(project_id) => {
                    if let Some(project) = self.store.get_project(project_id).await? {
                        if let Some(wt) = &session.worktree_path {
                            let short: String = session_pk.chars().take(8).collect();
                            // Delete the branch only when the engine generated its
                            // name; user-named and pre-existing branches survive.
                            // No-worktree sessions never reach this block at all —
                            // the user's checkout is never switched back.
                            let owned_branch = if session.branch_owned {
                                session.branch.as_deref()
                            } else {
                                None
                            };
                            let _ = worktree::remove(
                                Path::new(&project.workdir),
                                &short,
                                owned_branch,
                                Path::new(wt),
                            );
                            // Forget the deleted path so a later continue cold-resumes
                            // into the project workdir instead of a dead directory.
                            let _ = self.store.clear_session_worktree(session_pk).await;
                        }
                    }
                }
                // Chat sessions have no worktree — their "workspace" is the
                // managed scratch dir (`paths::chat_scratch_dir`), which is
                // ephemeral: the durable record is the transcript in the
                // store, so the on-disk scratch files are removed here.
                None => {
                    let _ =
                        tokio::fs::remove_dir_all(crate::paths::chat_scratch_dir(session_pk)).await;
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
