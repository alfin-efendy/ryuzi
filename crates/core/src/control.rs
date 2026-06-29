use crate::approval::{ApprovalDecider, ApprovalHub, ApprovalServer};
use crate::domain::{AgentEvent, CoreEvent, PermMode, Project, Session, SessionStatus};
use crate::paths::{new_id, now_ms, worktree_path_for};
use crate::policy::resolve_tool_policy;
use crate::runtime::{build_claude_args, parse_line, ApprovalWiring, ClaudeRunner, RunInput};
use crate::store::Store;
use crate::worktree;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub struct ControlPlane {
    store: Store,
    runner: Arc<dyn ClaudeRunner>,
    events: broadcast::Sender<CoreEvent>,
    approvals: Arc<ApprovalHub>,
    approval_server: Mutex<Option<ApprovalServer>>,
    hook_bin_path: Mutex<Option<String>>,
    approval_timeout: Duration,
    running: Mutex<HashMap<String, CancellationToken>>,
}

impl ControlPlane {
    pub async fn new(store: Store, runner: Arc<dyn ClaudeRunner>) -> Arc<ControlPlane> {
        let (events, _) = broadcast::channel(1024);
        Arc::new(ControlPlane {
            store,
            runner,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            approval_server: Mutex::new(None),
            hook_bin_path: Mutex::new(None),
            approval_timeout: Duration::from_secs(300),
            running: Mutex::new(HashMap::new()),
        })
    }

    pub fn enable_approvals(self: &Arc<Self>, hook_bin_path: String) -> std::io::Result<()> {
        let handle = tokio::runtime::Handle::current();
        let decider: Arc<dyn ApprovalDecider> = self.clone();
        let server = ApprovalServer::start(handle, decider)?;
        *self.approval_server.lock().unwrap() = Some(server);
        *self.hook_bin_path.lock().unwrap() = Some(hook_bin_path);
        Ok(())
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
            created_at: Some(now),
            last_active: Some(now),
        };
        self.store.insert_session(session.clone()).await?;
        let _ = self.events.send(CoreEvent::SessionCreated {
            session_pk: session_pk.clone(),
            project_id: project.project_id.clone(),
        });

        // Non-blocking: stream the run in the background and return the in-memory
        // Running session immediately so the cockpit can drive multiple sessions
        // concurrently without waiting for the harness to finish.
        let me = std::sync::Arc::clone(self);
        let project_for_run = project.clone();
        let pk = session_pk.clone();
        let prompt_owned = prompt.to_string();
        tokio::spawn(async move {
            me.run_harness(&project_for_run, &pk, &prompt_owned, None).await;
        });
        Ok(session)
    }

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
        let project = self
            .store
            .get_project(&session.project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown project: {}", session.project_id))?;
        self.store
            .update_status(session_pk, SessionStatus::Running, None)
            .await?;

        // Non-blocking: resume in the background and return immediately.
        let me = std::sync::Arc::clone(self);
        let project_for_run = project.clone();
        let pk = session_pk.to_string();
        let prompt_owned = prompt.to_string();
        let resume = session.agent_session_id.clone();
        tokio::spawn(async move {
            me.run_harness(&project_for_run, &pk, &prompt_owned, resume).await;
        });
        Ok(())
    }

    pub async fn stop_session(&self, session_pk: &str) -> anyhow::Result<()> {
        if let Some(tok) = self.running.lock().unwrap().get(session_pk) {
            tok.cancel();
        }
        self.store
            .update_status(session_pk, SessionStatus::Idle, Some(now_ms()))
            .await?;
        Ok(())
    }

    pub async fn end_session(&self, session_pk: &str) -> anyhow::Result<()> {
        if let Some(tok) = self.running.lock().unwrap().get(session_pk) {
            tok.cancel();
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

    fn approval_wiring(&self, session_pk: &str) -> Option<ApprovalWiring> {
        let server = self.approval_server.lock().unwrap();
        let hook = self.hook_bin_path.lock().unwrap();
        match (server.as_ref(), hook.as_ref()) {
            (Some(s), Some(h)) => Some(ApprovalWiring {
                url: s.url().to_string(),
                session_pk: session_pk.to_string(),
                hook_bin_path: h.clone(),
            }),
            _ => None,
        }
    }

    async fn run_harness(
        &self,
        project: &Project,
        session_pk: &str,
        prompt: &str,
        resume: Option<String>,
    ) {
        let cancel = CancellationToken::new();
        self.running
            .lock()
            .unwrap()
            .insert(session_pk.to_string(), cancel.clone());

        let new_session_id = resume.clone().unwrap_or_else(new_id);
        let approval = if project.perm_mode == PermMode::Default {
            self.approval_wiring(session_pk)
        } else {
            None
        };

        let workdir = self
            .store
            .get_session(session_pk)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.worktree_path)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(&project.workdir));

        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(a) = &approval {
            env.push(("HARNESS_APPROVAL_URL".into(), a.url.clone()));
            env.push(("HARNESS_SESSION_PK".into(), a.session_pk.clone()));
        }

        let input = RunInput {
            workdir: workdir.clone(),
            resume,
            prompt: prompt.to_string(),
            model: project.model.clone(),
            effort: project.effort.clone(),
            permission_mode: project.perm_mode,
            approval,
        };
        let args = build_claude_args(&input, &new_session_id);

        let mut rx = self.runner.spawn(args, workdir, env, cancel.clone());
        while let Some(item) = rx.recv().await {
            match item {
                Ok(line) => {
                    for ev in parse_line(&line) {
                        self.handle_agent_event(session_pk, ev).await;
                    }
                }
                Err(e) => {
                    let _ = self.events.send(CoreEvent::Error {
                        session_pk: session_pk.to_string(),
                        message: e,
                    });
                }
            }
        }

        self.running.lock().unwrap().remove(session_pk);
        if let Ok(Some(s)) = self.store.get_session(session_pk).await {
            if s.status == SessionStatus::Running {
                let _ = self
                    .store
                    .update_status(session_pk, SessionStatus::Idle, Some(now_ms()))
                    .await;
            }
        }
    }

    async fn handle_agent_event(&self, session_pk: &str, ev: AgentEvent) {
        match ev {
            AgentEvent::Init { session_id } => {
                let _ = self
                    .store
                    .update_agent_session_id(session_pk, &session_id)
                    .await;
            }
            AgentEvent::Status { text } => {
                let _ = self.events.send(CoreEvent::Status {
                    session_pk: session_pk.to_string(),
                    text,
                });
            }
            AgentEvent::Text { text } => {
                let _ = self.events.send(CoreEvent::Text {
                    session_pk: session_pk.to_string(),
                    text,
                });
            }
            AgentEvent::Result { session_id } => {
                if let Some(sid) = session_id {
                    let _ = self.store.update_agent_session_id(session_pk, &sid).await;
                }
                let _ = self.events.send(CoreEvent::Result {
                    session_pk: session_pk.to_string(),
                });
            }
            AgentEvent::Error { message } => {
                let _ = self.events.send(CoreEvent::Error {
                    session_pk: session_pk.to_string(),
                    message,
                });
            }
        }
    }
}

impl ApprovalDecider for ControlPlane {
    fn decide(
        &self,
        session_pk: String,
        tool: String,
        input: serde_json::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            let session = match self.store.get_session(&session_pk).await.ok().flatten() {
                Some(s) => s,
                None => return false,
            };
            let project = match self.store.get_project(&session.project_id).await.ok().flatten() {
                Some(p) => p,
                None => return false,
            };
            if resolve_tool_policy(project.perm_mode, &tool) {
                return true;
            }
            let request_id = new_id();
            let rx = self.approvals.register(request_id.clone());
            let summary = crate::policy::tool_summary(&tool, &input);
            let _ = self.events.send(CoreEvent::ApprovalRequested {
                session_pk: session_pk.clone(),
                request_id: request_id.clone(),
                tool,
                summary,
            });
            match tokio::time::timeout(self.approval_timeout, rx).await {
                Ok(Ok(allow)) => allow,
                _ => {
                    self.approvals.resolve(&request_id, false);
                    false
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CoreEvent;
    use crate::runtime::ClaudeRunner;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct ScriptedRunner;
    impl ClaudeRunner for ScriptedRunner {
        fn spawn(
            &self,
            _args: Vec<String>,
            _cwd: std::path::PathBuf,
            _env: Vec<(String, String)>,
            _cancel: CancellationToken,
        ) -> tokio::sync::mpsc::Receiver<Result<String, String>> {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                let lines = [
                    r#"{"type":"system","subtype":"init","session_id":"agent-1"}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
                    r#"{"type":"result","session_id":"agent-1"}"#,
                ];
                for l in lines {
                    let _ = tx.send(Ok(l.to_string())).await;
                }
            });
            rx
        }
    }

    /// Init a git repo with one commit (worktrees need a HEAD commit).
    fn init_repo(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }

    #[tokio::test]
    async fn start_session_streams_events_and_records_agent_id() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(db.path()).await.unwrap();
        let cp = ControlPlane::new(store, Arc::new(ScriptedRunner)).await;

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let project = cp.connect_project(repo.path(), "demo").await.unwrap();

        let mut rx = cp.subscribe();
        let session = cp.start_session(&project.project_id, "do it").await.unwrap();

        // Drain events until Result.
        let mut texts = Vec::new();
        loop {
            match rx.recv().await.unwrap() {
                CoreEvent::Text { text, .. } => texts.push(text),
                CoreEvent::Result { .. } => break,
                _ => {}
            }
        }
        assert!(texts.contains(&"working".to_string()));

        let stored = cp.list_sessions(Some(&project.project_id)).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].agent_session_id.as_deref(), Some("agent-1"));
        assert_eq!(session.status, crate::domain::SessionStatus::Running);
    }
}
