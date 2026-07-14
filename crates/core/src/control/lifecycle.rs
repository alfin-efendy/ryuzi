//! Session lifecycle: start/continue/resume/reconcile/stop/end, plus the
//! harness-session wiring and the background prompt driver.

use super::{ControlPlane, RESUME_NUDGE};
use crate::connector::ConnectorCtx;
use crate::domain::{
    AttachmentRef, CoreEvent, NewMessage, PermMode, Project, Session, SessionGitOptions,
    SessionKind, SessionStatus, WriteOrigin,
};
use crate::harness::{HarnessSession, SessionCtx, TurnPrompt};
use crate::paths::{new_id, now_ms, worktree_path_for};
use crate::settings::SettingsStore;
use crate::worktree;
use std::path::Path;
use std::sync::Arc;

/// Binds a spawned session to an orchestration as a labeled worker (spec §8):
/// it runs `agent`, its bubbles are attributed to that name, and its
/// `parent_session_pk` points at the home chat it reports into.
#[derive(Debug, Clone)]
pub struct WorkerBinding {
    pub agent: String,
    pub home_session_pk: Option<String>,
}

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
            None,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_session_with_prompt(
        self: &Arc<Self>,
        project_id: &str,
        prompt: TurnPrompt,
        started_by: &str,
        attachments: &[AttachmentRef],
        git: Option<SessionGitOptions>,
        perm_mode: Option<PermMode>,
        model_override: Option<String>,
        worker: Option<WorkerBinding>,
    ) -> anyhow::Result<Session> {
        self.start_session_with_prompt_and_origin(
            project_id,
            prompt,
            started_by,
            attachments,
            git,
            perm_mode,
            model_override,
            worker,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_session_with_prompt_and_origin(
        self: &Arc<Self>,
        project_id: &str,
        prompt: TurnPrompt,
        started_by: &str,
        attachments: &[AttachmentRef],
        git: Option<SessionGitOptions>,
        perm_mode: Option<PermMode>,
        model_override: Option<String>,
        worker: Option<WorkerBinding>,
        automation_origin: Option<crate::automation::HookOrigin>,
    ) -> anyhow::Result<Session> {
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("daemon is draining for an update; try again shortly");
        }
        let mut project = self
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {project_id}"))?;

        // A project without a pinned MODEL inherits the legacy agent default
        // stored under the `agent_model` settings key.
        //
        // Permission mode is per-session: this new session's mode comes from
        // `perm_mode` above (the picker) or falls back to the project's own
        // `perm_mode` — it is NOT inherited from a global/agent default.
        // `Default` means "Ask" (prompt before edits/commands) — inheriting a
        // global default here is exactly what made a project set to Ask
        // silently run without asking. Once created, the SESSION's own row is
        // the source of truth (per-session mode) — the project's `perm_mode`
        // only seeds new sessions.
        if project.model.is_none() {
            if let Ok(agent) = crate::agent_settings::get(&self.store).await {
                project.model = agent.model.filter(|m| !m.trim().is_empty());
            }
        }

        // A caller-supplied override (currently: a job's `model_override`)
        // wins over both the project's pinned model and the agent default
        // resolved just above — scoped to this one session's start.
        if let Some(m) = model_override.filter(|m| !m.trim().is_empty()) {
            project.model = Some(m);
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
            perm_mode: perm_mode.unwrap_or(project.perm_mode),
            started_by: Some(started_by.to_string()),
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: project.is_git && git.create_branch && git.branch_name.is_none(),
            kind: if worker.is_some() {
                SessionKind::Worker
            } else {
                SessionKind::Project
            },
            speaker: worker.as_ref().map(|w| w.agent.clone()),
            agent: worker.as_ref().map(|w| w.agent.clone()),
            parent_session_pk: worker.as_ref().and_then(|w| w.home_session_pk.clone()),
        };
        self.store.insert_session(session.clone()).await?;
        if let Some(origin) = automation_origin {
            self.store.insert_hook_origin(&session_pk, &origin).await?;
        }
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
            &format!("session {short} started ({})", project.name),
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
        self.start_chat_session_with_runtime(prompt, started_by, attachments, None, None, None)
            .await
    }

    pub async fn start_chat_session_with_runtime(
        self: &Arc<Self>,
        prompt: TurnPrompt,
        started_by: &str,
        attachments: &[AttachmentRef],
        model: Option<String>,
        effort: Option<String>,
        perm_mode: Option<PermMode>,
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
            perm_mode: perm_mode.unwrap_or(PermMode::Default),
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        };
        self.store
            .insert_chat_session_with_runtime(session.clone(), model, effort)
            .await?;
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
        let cancel = tokio_util::sync::CancellationToken::new();
        self.starting
            .lock()
            .unwrap()
            .insert(session_pk.clone(), cancel.clone());
        tokio::spawn(async move {
            me.run_chat_startup(session_pk, prompt, attachments, cancel)
                .await;
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
        cancel: tokio_util::sync::CancellationToken,
    ) {
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
                force_subtask: prompt.force_subtask,
            },
        );
    }

    /// Send a follow-up prompt on an existing session.
    ///
    /// The harness session built by `start_harness_session` is long-lived;
    /// the fast path reuses the live handle from the `running` map; only
    /// when the handle is absent (app restart) does a fresh session resume
    /// from the persisted agent session id.
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

        // Fast path: reuse the live native session if its handle is still in
        // the `running` map. The live harness already holds context, so no
        // new session is spawned and no transcript replay happens.
        let existing = self.running.lock().unwrap().get(session_pk).cloned();
        let handle = match existing {
            Some(handle) => handle,
            None => {
                // Cold-resume path: the in-memory handle is gone (e.g. after an
                // app restart). Start a FRESH native session; it reconstructs
                // conversation context from the persisted transcript keyed by
                // `session_pk` (the `resume` id passed through is currently
                // unused by `NativeHarness`, which resumes from the Store). A
                // chat (project-less) session has no project to resolve — it
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
        // Refresh the live session's permission mode from ITS OWN row so a
        // change made in the composer between turns takes effect NOW — and so
        // one session's change never leaks into siblings (per-session mode).
        // Works for chat sessions too: they carry their own perm_mode with no
        // project to consult.
        handle.set_perm_mode(session.perm_mode);
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
                force_subtask: prompt.force_subtask,
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
                speaker: None,
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
                speaker: None,
            });
        }
    }

    /// Post a labeled display bubble into a session's transcript (spec §8:
    /// worker/orchestrator start/status/report bubbles). A DISPLAY row only —
    /// written to the `messages` ledger via `insert_message`, NOT
    /// `provider_turns`, so it never enters the model's history or perturbs
    /// role alternation. Emits the live `CoreEvent::Message` so attached
    /// surfaces render it immediately.
    pub async fn post_speaker_bubble(
        &self,
        session_pk: &str,
        speaker: &str,
        block_type: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::json!({ "text": text });
        let seq = self
            .store
            .insert_message(NewMessage::speaker_block(
                session_pk,
                speaker,
                block_type,
                payload.clone(),
            ))
            .await?;
        let _ = self.events.send(CoreEvent::Message {
            session_pk: session_pk.to_string(),
            seq,
            role: "assistant".to_string(),
            block_type: block_type.to_string(),
            payload,
            tool_call_id: None,
            status: None,
            tool_kind: None,
            speaker: Some(speaker.to_string()),
        });
        Ok(())
    }

    /// Deliver a human's answer to a blocked worker (spec §8, Task E6). The
    /// answer flows back over the rail (`kind='unblock'`) as a clean new user
    /// turn once the worker session is idle — never a mid-turn splice; the
    /// idle-only drainer resumes the worker. Returns whether the task was
    /// actually blocked (a stale/unknown/already-resumed task id is a no-op,
    /// not an error).
    pub async fn answer_orch_block(&self, task_id: &str, answer: &str) -> anyhow::Result<bool> {
        let Some(task) = crate::orch::get_task(&self.store, task_id).await? else {
            return Ok(false);
        };
        if task.status != "blocked" {
            return Ok(false);
        }
        let Some(worker) = task.session_pk.as_deref() else {
            return Ok(false);
        };
        let block = format!(
            "[HUMAN ANSWER — orchestration block for task {task_id}] The user answered your \
             blocking question. Continue the subtask using this answer.\n\n{answer}"
        );
        self.store
            .enqueue_background_event(worker, "unblock", &block)
            .await?;
        Ok(true)
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
            let settings = SettingsStore::new(self.store.clone());
            let worktree_base = settings
                .get("worktree_dir")
                .await
                .ok()
                .flatten()
                .filter(|s| !s.trim().is_empty())
                .map(|s| crate::settings::expand_home(s.trim()));
            let worktree_candidate =
                worktree_path_for(worktree_base.as_deref(), &project.project_id, session_pk);
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
                force_subtask: prompt.force_subtask,
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

    /// Create the native harness, build a `SessionCtx`, and start the
    /// session. Records the returned handle in the `running` map and
    /// returns a clone for driving the first prompt.
    ///
    /// `project` is `None` for a chat (project-less) session — there is no
    /// `Project` row to inherit `perm_mode`/`model`/`effort` from, so those
    /// fall back to engine-wide settings: `perm_mode` from `default_perm_mode`,
    /// `model` from the native agent's configured model (`agent_settings`), and
    /// `effort` from `default_effort`. The harness is always native.
    async fn start_harness_session(
        self: &Arc<Self>,
        project: Option<&Project>,
        session_pk: &str,
        work_dir: &Path,
        resume: Option<String>,
    ) -> anyhow::Result<Arc<dyn HarnessSession>> {
        let settings = SettingsStore::new(self.store.clone());
        // Native-only (#105): a single harness, so no harness/runtime id
        // resolution. model/effort/perm_mode come from the project when one is
        // bound; a chat (project-less) session falls back to engine-wide
        // settings — model from the native agent's configured model
        // (`agent_settings`, replacing the deleted `runtimes::session_defaults`),
        // perm_mode from `default_perm_mode`, effort from `default_effort`.
        // (perm_mode here is only a fallback; the session row's own perm_mode
        // overrides it below.)
        let (perm_mode, model, effort): (PermMode, Option<String>, Option<String>) = match project {
            Some(p) => (p.perm_mode, p.model.clone(), p.effort.clone()),
            None => {
                let default_perm_raw = settings
                    .get("default_perm_mode")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "default".to_string());
                let perm_mode = PermMode::from_db(&default_perm_raw);
                let runtime = self.store.get_session_runtime_settings(session_pk).await?;
                let model = runtime
                    .as_ref()
                    .and_then(|runtime| runtime.model.clone())
                    .or(crate::agent_settings::get(&self.store)
                        .await
                        .ok()
                        .and_then(|a| a.model)
                        .filter(|m| !m.trim().is_empty()));
                let effort = runtime.and_then(|runtime| runtime.effort).or(settings
                    .get("default_effort")
                    .await
                    .ok()
                    .flatten()
                    .filter(|v| !v.trim().is_empty()));
                (perm_mode, model, effort)
            }
        };

        let harness = self.registries.harness.create()?;

        // Attach the Apps screen's enabled MCP servers to the session. The MCP
        // per-agent allowlist has a single agent id: "native".
        let mut mcp_servers =
            crate::mcp::servers_for_session(&self.store, crate::harness::native::NATIVE_ID)
                .await
                .unwrap_or_default();
        // A chat session has no project to scope plugin connectors by —
        // `ConnectorCtx.project_id` isn't read by any connector today, so the
        // session id is a harmless, uniquely-scoped stand-in.
        let scope_id = project.map(|p| p.project_id.as_str()).unwrap_or(session_pk);
        let mcp_principals = self
            .attach_plugin_mcp_servers(scope_id, work_dir, &settings, &mut mcp_servers)
            .await;
        let extra_skill_dirs = self.registries.plugins.enabled_skill_dirs(&settings).await;
        // Track D: thread the daemon's extension host in only when it has
        // something spawned — `None` keeps every hook fire site a true
        // no-op (zero extra dispatch/await) for the common case, and for
        // every test `ControlPlane` (which never calls `spawn_extensions`).
        let extension_host_empty = self.extension_host.is_empty().await;
        let extension_events: Option<Arc<dyn crate::plugins::extension::ExtensionEvents>> =
            if extension_host_empty {
                None
            } else {
                Some(self.extension_host.clone()
                    as Arc<dyn crate::plugins::extension::ExtensionEvents>)
            };
        // DT6: the tool-provision sibling to `extension_events` above — same
        // guard, same source (`self.extension_host`), so a daemon with no
        // extensions spawned pays nothing extra building either the hook
        // dispatch path or the session's tool registry.
        let extension_tools: Option<Arc<dyn crate::plugins::extension::ExtensionTools>> =
            if extension_host_empty {
                None
            } else {
                Some(self.extension_host.clone()
                    as Arc<dyn crate::plugins::extension::ExtensionTools>)
            };
        // `kind`/`agent` come from the session row rather than a caller
        // parameter — every caller of `start_harness_session` (fresh start,
        // cold-resume, crash-resume) has already inserted the row before
        // reaching here, so this is a reliable single source of truth. A
        // missing row (shouldn't happen in practice) falls back to the kind
        // implied by whether a project was resolved. The same row also carries
        // the per-session permission mode (#100), which overrides the
        // runtime/settings default computed above; when the row can't be read
        // (a not-yet-persisted resume path) that default stands in — and it is
        // already project-less-safe, so no `project` deref is needed here.
        let session_row = self.store.get_session(session_pk).await.ok().flatten();
        let kind = session_row
            .as_ref()
            .map(|s| s.kind)
            .unwrap_or(if project.is_some() {
                SessionKind::Project
            } else {
                SessionKind::Chat
            });
        let perm_mode = session_row
            .as_ref()
            .map(|s| s.perm_mode)
            .unwrap_or(perm_mode);
        let agent = session_row.and_then(|s| s.agent);
        // The curated app-control facade (spec §9.1) is only for a top-level
        // interactive session: a worker or review-fork session never gets
        // the `app_*` tools (mirrors the existing sub-agent blocklist, Task
        // 6's `child_deps.app_control` reset, and this crate's `Harness`
        // trait boundary — `SessionCtx` is the one channel from here into
        // `NativeHarness::start_session`, which cannot reach `ControlPlane`
        // itself to build one).
        let app_control = match kind {
            SessionKind::Project | SessionKind::Chat => Some(self.build_app_control()),
            SessionKind::Worker | SessionKind::Review => None,
        };
        let persistence = self.agent_persistence().ok_or_else(|| {
            anyhow::anyhow!("agent persistence was not attached to the control plane")
        })?;
        let main_agent_id = persistence.registry.default_agent_id().await;
        let ctx = SessionCtx {
            session_pk: session_pk.to_string(),
            main_agent_id,
            project_id: project.map(|p| p.project_id.clone()),
            kind,
            agent,
            work_dir: work_dir.to_path_buf(),
            attachments_dir: Some(self.attachment_dest_dir(session_pk).await),
            perm_mode,
            model,
            effort,
            resume,
            mcp_servers,
            mcp_principals,
            extra_skill_dirs,
            extension_events,
            extension_tools,
            events: self.events.clone(),
            approvals: self.approvals.clone(),
            automation_events: Some(Arc::new(super::ControlPlaneAutomationSink(Arc::downgrade(
                self,
            )))),
            background: self.background.clone(),
            agent_knowledge: persistence.knowledge.clone(),
            learning_queue: persistence.learning.clone(),
            store: self.store.clone(),
            app_control,
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
    ///
    /// Each plugin's outcome (`"ok"`/`"failed"`, plus a secret-free reason on
    /// failure) is also recorded via [`Self::record_attach`] for
    /// `plugin_doctor` to surface later — recording is best-effort and never
    /// changes this loop's control flow or its warn-and-continue discipline.
    ///
    /// Returns the `McpServerSpec.name` → owning-plugin [`Principal`] binding
    /// for every server this call actually attached — resolved HERE, at the
    /// only place a server name is definitively known to belong to a given
    /// `CorePlugin`, rather than reconstructed later from a substring match
    /// on the server/tool name. A server that lost the `names.insert` race
    /// (a DB-configured or earlier plugin's same-named server won) gets no
    /// entry, mirroring its exclusion from `mcp_servers` itself.
    ///
    /// [`Principal`]: crate::domain::Principal
    async fn attach_plugin_mcp_servers(
        &self,
        project_id: &str,
        work_dir: &Path,
        settings: &SettingsStore,
        mcp_servers: &mut Vec<crate::domain::McpServerSpec>,
    ) -> std::collections::HashMap<String, crate::domain::Principal> {
        let mut names: std::collections::HashSet<String> =
            mcp_servers.iter().map(|s| s.name.clone()).collect();
        let mut principals: std::collections::HashMap<String, crate::domain::Principal> =
            std::collections::HashMap::new();
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
                    let reason = safe_attach_reason(id, AttachStage::Enable, &e);
                    self.record_attach(id, "failed", Some(&reason)).await;
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
                let reason = safe_attach_reason(id, AttachStage::Auth, &e);
                self.record_attach(id, "failed", Some(&reason)).await;
                continue;
            }
            match connector.mcp_servers(&ctx).await {
                Ok(specs) => {
                    for spec in specs {
                        if !names.insert(spec.name.clone()) {
                            continue; // a DB-configured (or earlier plugin's) server wins
                        }
                        principals.insert(
                            spec.name.clone(),
                            crate::domain::Principal {
                                plugin_id: id.clone(),
                                plugin_name: plugin.manifest.name.clone(),
                            },
                        );
                        mcp_servers.push(spec);
                    }
                    self.record_attach(id, "ok", None).await;
                }
                Err(e) => {
                    tracing::warn!(plugin = %id, "plugin connector failed: {e}");
                    let reason = safe_attach_reason(id, AttachStage::McpServers, &e);
                    self.record_attach(id, "failed", Some(&reason)).await;
                }
            }
        }
        principals
    }

    /// Best-effort record of a plugin's session-attach outcome into
    /// `plugin_attach_status`, for `plugin_doctor` to read back later. Never
    /// surfaces its own failure — a store write failing here must not turn
    /// into a session-start error, mirroring the warn-and-continue discipline
    /// of the loop that calls it.
    ///
    /// `reason` must already be secret-free (see [`safe_attach_reason`]): the
    /// persisted value is doctor/UI-visible, so raw connector error text must
    /// never reach it.
    async fn record_attach(&self, id: &str, outcome: &str, reason: Option<&str>) {
        let _ = self
            .store
            .record_plugin_attach(&crate::store::PluginAttachStatus {
                plugin_id: id.to_string(),
                last_attach_at: crate::paths::now_ms(),
                outcome: outcome.to_string(),
                reason: reason.map(str::to_string),
            })
            .await;
    }

    /// Drive a prompt on `handle` in the background. `send_prompt` blocks until
    /// the turn completes (turn end); on completion we atomically demote
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
                    // Broadcast `Result` BEFORE the idle demote (the Err arm
                    // below deliberately demotes first, for UI-refresh
                    // reasons; this arm must not). The ordering is load-bearing
                    // for orch's block-for-human resume (Task E6): the
                    // background-rail drainer flips a `blocked` worker task to
                    // `running` (`orch::on_unblock_delivered`) only AFTER it
                    // claims the unblock event, and a claim requires the worker
                    // session to be idle (`claim_deliverable_background_event`
                    // joins on `sessions.status`). Emitting `Result` before the
                    // demote guarantees a parked `watch_session` has the block
                    // turn's `Result` queued before the session is ever
                    // claimable — so it reads the task as still `blocked` and
                    // keeps waiting, instead of racing the flip and finishing
                    // the unanswered block turn as `done`.
                    let _ = me.events.send(CoreEvent::Result {
                        session_pk: session_pk.clone(),
                    });
                    me.finish_automation_session(&session_pk, "success", None)
                        .await;
                    let _ = me.store.demote_if_running(&session_pk, now_ms()).await;
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
                    me.finish_automation_session(&session_pk, "failed", Some(&message))
                        .await;
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
        self.finish_automation_session(session_pk, "failed", Some("session cancelled"))
            .await;
        Ok(())
    }

    /// Tear down a session. This is the ONLY place the persistent live-session
    /// handle is removed from `running` and `end()`ed (graceful native-harness
    /// teardown), after which the worktree is cleaned up and the session
    /// marked `Ended`.
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
        self.store
            .update_status(session_pk, SessionStatus::Ended, Some(now_ms()))
            .await?;
        // A cancelled hook-origin turn can still return Ok while its handle is
        // unwinding. Terminally fail its run before cancellation so that late
        // completion loses the guarded transition.
        self.finish_automation_session(session_pk, "failed", Some("session cancelled"))
            .await;
        let handle = self.running.lock().unwrap().remove(session_pk);
        if let Some(handle) = handle {
            // Interrupt any in-flight turn first so teardown doesn't race a
            // still-working agent inside the worktree we're about to delete.
            let _ = handle.cancel().await;
            let _ = handle.end().await;
        }
        // Cancel any in-flight background delegations this session dispatched
        // and purge its pending rail rows — orphaned background work must not
        // survive to leak into a new chat or be delivered to a dead session
        // (spec §6.1). This runs AFTER the turn interrupt above: a still-running
        // turn could spawn a new background worker, so the turn is stopped
        // first, then its background children are cancelled, then their rail
        // rows are dropped. A narrow race remains — a worker could reserve a
        // fresh slot in the instant between `interrupt_for_session` and this
        // delete — accepted as a known limitation rather than closed with a
        // new locking mechanism.
        self.background.interrupt_for_session(session_pk);
        let _ = self
            .store
            .delete_background_events_for_session(session_pk)
            .await;
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
        // The terminal status was persisted before `handle.end()` so its
        // lifecycle observer reads a final row. Do not finalize hook-run
        // success here: a prior stop/cancel or turn failure owns that result.
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

    /// Drive one review-fork replay from a durable learning-queue payload
    /// captured `LearningPayload`, spin up a `kind='review'`,
    /// `parent_session_pk`-linked session — a real, isolatable session, but
    /// hidden from every picker (which filters on `kind`) — and replay the
    /// parent's exact captured `system`/`tool_defs`/`messages` prefix
    /// byte-for-byte when the resolved review model matches the payload's
    /// captured model (prompt-cache parity), or a tail digest otherwise. The
    /// fork advertises the parent's full `tool_defs` but is restricted to
    /// `runner::REVIEW_TOOL_WHITELIST` at DISPATCH time
    /// (`agent.tools.allows`, enforced inside `runner::drive`), carries
    /// `WriteOrigin::BackgroundReview` into every `ToolCtx` it builds (so
    /// Task 6's skill-write guard applies), and runs with a fresh
    /// `NudgeState` under `DisplayMode::Silent` so it can never recursively
    /// enqueue its own review. On completion, persists a `💾
    /// Self-improvement review: …` notice into the PARENT transcript and
    /// broadcasts it so a live Cockpit view picks it up immediately.
    ///
    /// Called by `learning::tick` once per claimed row; a successful return
    /// marks the row delivered, an error releases the claim so a later tick
    /// retries it.
    pub async fn run_review_fork(&self, payload: &str) -> anyhow::Result<()> {
        use crate::harness::native::agents::AgentRegistry;
        use crate::harness::native::commands::CommandRegistry;
        use crate::harness::native::context_manager::{ContextConfig, ContextManager};
        use crate::harness::native::runner::{self, LearningPayload, NudgeState, RunnerDeps};
        use crate::harness::native::steer::SteerBuffer;
        use crate::harness::native::tools::ToolRegistry;

        let payload: LearningPayload = serde_json::from_str(payload)?;
        let store = self.store.clone();

        // Resolve the review model: `auxiliary.review.model` if configured,
        // else the parent's captured model. Only an EXACT match with the
        // captured model can safely replay the captured prefix — a
        // different model may tokenize/route/cache differently, so cache
        // parity is impossible and a tail digest stands in instead.
        let model = crate::harness::native::llm::aux_model(&store, "review", &payload.model).await;
        let cache_parity = model == payload.model;

        let mut meta = crate::llm_router::model_meta::resolve(&store, &model).await;
        if cache_parity {
            // Pin the EXACT flag the payload was captured with — `drive()`'s
            // system-wrapping formula branches on this, and it must
            // reproduce whatever the parent's turn actually did, regardless
            // of any model-metadata drift since capture time.
            meta.supports_prompt_cache = payload.supports_prompt_cache;
        }

        let parent = store.get_session(&payload.parent_session_pk).await?;
        let project_id = parent.as_ref().and_then(|s| s.project_id.clone());
        // Reuse the parent's own workspace so `skill`/`skill_manage` see the
        // same project-local skill dirs the parent conversation saw. Project
        // sessions have a real worktree; chat-first sessions have none — the
        // same deterministic scratch dir the parent's own harness session
        // already runs in stands in.
        let work_dir = parent
            .as_ref()
            .and_then(|s| s.worktree_path.clone())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| crate::paths::chat_scratch_dir(&payload.parent_session_pk));

        let review_pk = new_id();
        let now = now_ms();
        self.store
            .insert_session(Session {
                session_pk: review_pk.clone(),
                project_id: project_id.clone(),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Running,
                perm_mode: PermMode::BypassPermissions,
                started_by: None,
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Review,
                speaker: None,
                agent: None,
                parent_session_pk: Some(payload.parent_session_pk.clone()),
            })
            .await?;

        let settings = SettingsStore::new(store.clone());
        let extra_skill_dirs = self.registries.plugins.enabled_skill_dirs(&settings).await;
        let effort_policy =
            crate::llm_router::model_effort::build_utility_effort_policy(&store, &model).await?;
        let llm = self.review_llm_factory().create(store.clone());

        let persistence = self.agent_persistence().ok_or_else(|| {
            anyhow::anyhow!("agent persistence was not attached to the control plane")
        })?;
        let deps = RunnerDeps {
            session_pk: review_pk.clone(),
            main_agent_id: persistence.registry.default_agent_id().await,
            learning_queue: persistence.learning.clone(),
            kind: SessionKind::Review,
            work_dir,
            attachments_dir: None,
            extra_skill_dirs,
            model: Some(model.clone()),
            turn_effort_policy: Arc::new(effort_policy),
            meta,
            // BypassPermissions: the review fork is unattended — no human
            // can ever answer an approval prompt — and the ONLY tools it can
            // reach at dispatch are `memory`/`skill`/`skill_manage`, whose
            // real safety boundary is Task 6's origin × provenance guard
            // (via `write_origin` below), not the interactive gate.
            perm_mode: Arc::new(std::sync::Mutex::new(PermMode::BypassPermissions)),
            project_id: project_id.clone(),
            perm_overrides: Arc::new(std::sync::Mutex::new(Default::default())),
            store: store.clone(),
            events: self.events.clone(),
            approvals: self.approvals.clone(),
            automation_events: None,
            llm,
            tools: Arc::new(ToolRegistry::builtin()),
            agent: runner::review_agent(payload.system.clone()),
            agents: Arc::new(AgentRegistry::builtin()),
            commands: Arc::new(CommandRegistry::builtin()),
            memory: None,
            snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            steer: SteerBuffer::new(),
            background: self.background.clone(),
            // The review fork is never a top-level interactive session — no
            // app-control facade, mirroring the primary builder's default.
            app_control: None,
            // Isolated background session: no extension host/event sink, like
            // sub-agent and test builders.
            extension_events: None,
            // A fresh, unshared `NudgeState`: the review fork's own tool
            // iterations must never feed the PARENT's nudge counters (that
            // would be a feedback loop), and it drives under
            // `DisplayMode::Silent` so its own end_turn never fires a nudge
            // regardless.
            nudge: Arc::new(NudgeState::default()),
            // Cache parity: advertise the parent's FULL captured tool set —
            // dispatch (`agent.tools`, above) still enforces the whitelist.
            review_tool_defs: Some(payload.tool_defs.clone()),
            write_origin: WriteOrigin::BackgroundReview,
        };

        let cfg = ContextConfig::with_meta(deps.meta.clone());
        let mut cm = if cache_parity {
            ContextManager::seed_projected(&review_pk, cfg, payload.messages.clone())
        } else {
            ContextManager::seed_digest(&review_pk, cfg, payload.messages.clone(), 24)
        };
        cm.append_user_text(&runner::review_prompt_text(&payload.review_kind))
            .await?;

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = runner::drive_review(&deps, &deps.agent, &mut cm, &cancel).await;

        // The review session is throwaway — never resumed, hidden from every
        // picker by `kind` — so it never lingers as `running`, whether the
        // drive succeeded or errored.
        let _ = self
            .store
            .update_status(&review_pk, SessionStatus::Ended, Some(now_ms()))
            .await;

        let final_text = result?;
        let outcome_line = {
            let t = final_text.trim();
            if t.is_empty() {
                "reviewed, no changes needed".to_string()
            } else {
                t.lines()
                    .next()
                    .unwrap_or(t)
                    .chars()
                    .take(240)
                    .collect::<String>()
            }
        };
        let summary = format!("{}: {outcome_line}", runner::SELF_IMPROVEMENT_NOTICE_PREFIX);
        let notice_payload = serde_json::json!({ "text": summary });
        if let Ok(seq) = self
            .store
            .insert_message(NewMessage::block(
                &payload.parent_session_pk,
                "system",
                "notice",
                notice_payload.clone(),
            ))
            .await
        {
            self.emit(CoreEvent::Message {
                session_pk: payload.parent_session_pk.clone(),
                seq,
                role: "system".into(),
                block_type: "notice".into(),
                payload: notice_payload,
                tool_call_id: None,
                status: None,
                tool_kind: None,
                speaker: None,
            });
        }
        Ok(())
    }
}

/// The stage of `attach_plugin_mcp_servers` at which a connector failed —
/// used only to pick a generic fallback message for [`safe_attach_reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachStage {
    /// `PluginHost::is_enabled` itself errored (internal, secret-free).
    Enable,
    /// `Connector::ensure_auth` errored — the one stage whose error text can
    /// carry a raw token-endpoint response body (HTTP-OAuth refresh path).
    Auth,
    /// `Connector::mcp_servers` errored while resolving specs.
    McpServers,
}

/// Map a connector-attach failure to a secret-free reason safe to PERSIST
/// into `plugin_attach_status` (which `plugin_doctor` reads back and later
/// surfaces in the UI). The full error still reaches `tracing::warn!` at the
/// call site — only the persisted reason is sanitized here.
///
/// Only the friendly `"configure {id}: ..."` messages that `ensure_auth`
/// (and the HTTP-OAuth `auth_required` path) raise for a missing/expired
/// credential are known to be secret-free: they name a setting key or a help
/// URL, never a value. Those pass through verbatim. Every other error — in
/// particular `refresh_http_oauth_token`'s `"{id} OAuth token refresh failed
/// with HTTP {status}: {detail}"`, where `detail` is the raw token-endpoint
/// response body and the refresh POST carried the real
/// `refresh_token`/`client_secret` — is collapsed to a generic per-stage
/// message so no connector error body is ever written to the DB.
fn safe_attach_reason(id: &str, stage: AttachStage, err: &anyhow::Error) -> String {
    let msg = err.to_string();
    if msg.starts_with(&format!("configure {id}:")) {
        return msg;
    }
    match stage {
        AttachStage::Enable => format!("{id}: could not determine whether the plugin is enabled"),
        AttachStage::Auth => format!("{id}: authentication failed"),
        AttachStage::McpServers => format!("{id}: could not resolve MCP servers"),
    }
}

#[cfg(test)]
mod safe_attach_reason_tests {
    use super::{safe_attach_reason, AttachStage};

    #[test]
    fn friendly_configure_message_passes_through_verbatim() {
        // The secret-free `ensure_auth` missing-credential message (names a
        // setting key / help URL, never a value) is preserved as-is.
        let err = anyhow::anyhow!("configure acme: see https://acme.test/help");
        assert_eq!(
            safe_attach_reason("acme", AttachStage::Auth, &err),
            "configure acme: see https://acme.test/help"
        );
    }

    #[test]
    fn oauth_refresh_body_never_reaches_the_persisted_reason() {
        // Simulate the raw HTTP-OAuth token-refresh error whose `detail` is
        // an untruncated response body echoing the refresh POST's form
        // fields — the exact leak the sanitizer must stop.
        let err = anyhow::anyhow!(
            "acme OAuth token refresh failed with HTTP 400: \
             {{\"echo\":\"refresh_token=leaked-secret-token&client_secret=leaked-client-secret\"}}"
        );
        let reason = safe_attach_reason("acme", AttachStage::Auth, &err);
        assert_eq!(reason, "acme: authentication failed");
        assert!(!reason.contains("leaked-secret-token"));
        assert!(!reason.contains("leaked-client-secret"));
        assert!(!reason.contains("refresh_token"));
    }

    #[test]
    fn enable_and_mcp_stage_errors_are_generic_and_drop_raw_text() {
        let err = anyhow::anyhow!("some internal detail with a token=abc123 in it");
        let enable = safe_attach_reason("acme", AttachStage::Enable, &err);
        let mcp = safe_attach_reason("acme", AttachStage::McpServers, &err);
        assert!(!enable.contains("abc123"));
        assert!(!mcp.contains("abc123"));
        assert!(enable.starts_with("acme:"));
        assert!(mcp.starts_with("acme:"));
    }
}
