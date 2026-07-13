use crate::agents::registry::AgentRegistry;
use crate::agents::types::{AgentModel, AgentSnapshot};
use crate::domain::{AgentRun, AgentRunKind, AgentRunStatus, CoreEvent, NewAgentRun};
use crate::store::Store;
use anyhow::{anyhow, bail};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

pub const MAX_MAIN_DELEGATION_DEPTH: usize = 4;
pub const MAX_ACTIVE_CHILD_RUNS: usize = 8;
pub const RESTART_INTERRUPTION_REASON: &str = "Ryuzi restarted before this run completed.";

pub struct MainDelegationRequest {
    pub parent_run_id: String,
    pub target_agent_id: String,
    pub task: String,
    pub context: Option<String>,
    pub background: bool,
}

pub struct SubagentRunRequest {
    pub parent_run_id: String,
    pub subagent_type: String,
    pub task: String,
    pub context: Option<String>,
    pub background: bool,
}

pub struct RunHandle {
    pub run: AgentRun,
    pub agent_snapshot: Option<Arc<AgentSnapshot>>,
    pub cancel: CancellationToken,
}

struct InFlightRun {
    agent_snapshot: Option<Arc<AgentSnapshot>>,
    cancel: CancellationToken,
}

pub struct DelegationRuntime {
    store: Arc<Store>,
    registry: Arc<AgentRegistry>,
    events: broadcast::Sender<CoreEvent>,
    admission: Mutex<()>,
    live: Mutex<HashMap<String, InFlightRun>>,
    terminal: Mutex<HashMap<String, AgentRun>>,
    terminal_events: broadcast::Sender<AgentRun>,
}

impl DelegationRuntime {
    pub fn new(
        store: Arc<Store>,
        registry: Arc<AgentRegistry>,
        events: broadcast::Sender<CoreEvent>,
    ) -> Arc<Self> {
        let (terminal_events, _) = broadcast::channel(1024);
        Arc::new(Self {
            store,
            registry,
            events,
            admission: Mutex::new(()),
            live: Mutex::new(HashMap::new()),
            terminal: Mutex::new(HashMap::new()),
            terminal_events,
        })
    }

    pub async fn recover_after_restart(&self) -> anyhow::Result<u64> {
        let interrupted = self
            .store
            .interrupt_incomplete_agent_runs(RESTART_INTERRUPTION_REASON)
            .await?;
        for run in &interrupted {
            self.emit(
                &run.session_pk,
                &run.run_id,
                run.parent_run_id.clone(),
                run.status,
            );
            self.record_terminal(run.clone()).await;
        }
        Ok(interrupted.len() as u64)
    }

    pub async fn begin_primary(
        &self,
        session_pk: &str,
        snapshot: Arc<AgentSnapshot>,
        task: &str,
    ) -> anyhow::Result<RunHandle> {
        let _admission = self.admission.lock().await;
        let run = self
            .store
            .insert_primary_agent_run(new_run(
                session_pk,
                None,
                None,
                &snapshot.profile.id,
                Some(&snapshot),
                AgentRunKind::Primary,
                task,
            ))
            .await?;
        Ok(self.register(run, Some(snapshot)).await)
    }

    pub async fn activate_persisted_primary(
        &self,
        run: AgentRun,
        snapshot: Arc<AgentSnapshot>,
    ) -> anyhow::Result<RunHandle> {
        if run.agent_kind != AgentRunKind::Primary || run.parent_run_id.is_some() {
            bail!("only root primary runs can be activated");
        }
        let _admission = self.admission.lock().await;
        Ok(self.register(run, Some(snapshot)).await)
    }

    pub async fn queue_main(&self, request: MainDelegationRequest) -> anyhow::Result<RunHandle> {
        // Resolve before admission/insertion, preserving the exact profile used by this run.
        let target = self
            .registry
            .resolved_snapshot(&request.target_agent_id)
            .await?;
        let _admission = self.admission.lock().await;
        let (parent, ancestry, root) = self.tree(&request.parent_run_id).await?;
        if ancestry
            .iter()
            .filter_map(|run| run.executing_agent_id.as_deref())
            .any(|id| id == target.profile.id)
        {
            bail!("main delegation target would create a self-delegation or ancestry cycle");
        }
        let main_depth = ancestry
            .iter()
            .filter(|run| run.agent_kind == AgentRunKind::MainDelegate)
            .count();
        if main_depth >= MAX_MAIN_DELEGATION_DEPTH {
            bail!("main delegation depth limit exceeded");
        }
        self.ensure_capacity(&root).await?;
        let run = self
            .store
            .insert_agent_run(new_run(
                &parent.session_pk,
                Some(&parent.run_id),
                None,
                &root.primary_agent_id,
                Some(&target),
                AgentRunKind::MainDelegate,
                &request.task,
            ))
            .await?;
        Ok(self.register(run, Some(target)).await)
    }

    pub async fn queue_subagent(&self, request: SubagentRunRequest) -> anyhow::Result<RunHandle> {
        let subagents = self.registry.snapshot().await.subagent_model;
        let _admission = self.admission.lock().await;
        let (parent, _, root) = self.tree(&request.parent_run_id).await?;
        self.ensure_capacity(&root).await?;
        let (resolved_model, resolved_effort) = model_parts(&subagents);
        let run = self
            .store
            .insert_agent_run(NewAgentRun {
                run_id: uuid::Uuid::new_v4().to_string(),
                session_pk: parent.session_pk,
                parent_run_id: Some(parent.run_id),
                retry_of: None,
                primary_agent_id: root.primary_agent_id,
                executing_agent_id: None,
                executing_agent_name_snapshot: request.subagent_type,
                agent_kind: AgentRunKind::Subagent,
                task: request.task,
                status: AgentRunStatus::Queued,
                resolved_model,
                resolved_effort,
            })
            .await?;
        Ok(self.register(run, None).await)
    }

    pub async fn execution_snapshot(&self, run_id: &str) -> Option<Arc<AgentSnapshot>> {
        self.live
            .lock()
            .await
            .get(run_id)
            .and_then(|run| run.agent_snapshot.clone())
    }

    pub async fn mark_running(&self, run_id: &str) -> anyhow::Result<()> {
        self.transition(
            run_id,
            &[AgentRunStatus::Queued],
            AgentRunStatus::Running,
            None,
            None,
        )
        .await
    }

    pub async fn complete(&self, run_id: &str, result: &str) -> anyhow::Result<()> {
        self.transition(
            run_id,
            &[AgentRunStatus::Queued, AgentRunStatus::Running],
            AgentRunStatus::Completed,
            Some(result),
            None,
        )
        .await
    }

    pub async fn fail(&self, run_id: &str, error: &str) -> anyhow::Result<()> {
        self.transition(
            run_id,
            &[AgentRunStatus::Queued, AgentRunStatus::Running],
            AgentRunStatus::Failed,
            None,
            Some(error),
        )
        .await
    }

    pub async fn cancel_child(&self, session_pk: &str, run_id: &str) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        let run = self
            .store
            .get_agent_run(run_id)
            .await?
            .ok_or_else(|| anyhow!("unknown agent run"))?;
        if run.session_pk != session_pk || run.parent_run_id.is_none() {
            bail!("only a child in this session can be cancelled");
        }
        let mut runs = vec![run];
        runs.extend(self.store.list_descendant_agent_runs(run_id).await?);
        for run in runs {
            if self
                .store
                .transition_agent_run(
                    &run.run_id,
                    &[AgentRunStatus::Queued, AgentRunStatus::Running],
                    AgentRunStatus::Cancelled,
                    None,
                    None,
                )
                .await?
            {
                self.emit(
                    &run.session_pk,
                    &run.run_id,
                    run.parent_run_id,
                    AgentRunStatus::Cancelled,
                );
                if let Some(live) = self.live.lock().await.remove(&run.run_id) {
                    live.cancel.cancel();
                }
            }
        }
        Ok(())
    }

    pub async fn retry_child(&self, session_pk: &str, run_id: &str) -> anyhow::Result<AgentRun> {
        let _admission = self.admission.lock().await;
        let previous = self
            .store
            .get_agent_run(run_id)
            .await?
            .ok_or_else(|| anyhow!("unknown agent run"))?;
        if previous.session_pk != session_pk
            || previous.parent_run_id.is_none()
            || !previous.status.is_terminal()
        {
            bail!("only terminal child runs in this session can be retried");
        }
        let (_, _, root) = self.tree(&previous.run_id).await?;
        self.ensure_capacity(&root).await?;
        let snapshot = match previous.executing_agent_id.as_deref() {
            Some(id) => Some(self.registry.resolved_snapshot(id).await?),
            None => None,
        };
        let (resolved_model, resolved_effort) = match &snapshot {
            Some(snapshot) => model_parts(&snapshot.profile.model),
            None => model_parts(&self.registry.snapshot().await.subagent_model),
        };
        let run = self
            .store
            .insert_agent_run(NewAgentRun {
                run_id: uuid::Uuid::new_v4().to_string(),
                session_pk: previous.session_pk.clone(),
                parent_run_id: previous.parent_run_id.clone(),
                retry_of: Some(previous.run_id),
                primary_agent_id: previous.primary_agent_id,
                executing_agent_id: previous.executing_agent_id,
                executing_agent_name_snapshot: snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.profile.name.clone())
                    .unwrap_or(previous.executing_agent_name_snapshot),
                agent_kind: previous.agent_kind,
                task: previous.task,
                status: AgentRunStatus::Queued,
                resolved_model,
                resolved_effort,
            })
            .await?;
        self.emit(
            &run.session_pk,
            &run.run_id,
            run.parent_run_id.clone(),
            run.status,
        );
        Ok(run)
    }

    async fn tree(&self, run_id: &str) -> anyhow::Result<(AgentRun, Vec<AgentRun>, AgentRun)> {
        let parent = self
            .store
            .get_agent_run(run_id)
            .await?
            .ok_or_else(|| anyhow!("unknown agent run"))?;
        let all = self
            .store
            .list_session_agent_runs(&parent.session_pk)
            .await?;
        let mut ancestry = vec![parent.clone()];
        let mut current = parent.clone();
        while let Some(parent_id) = current.parent_run_id.as_deref() {
            current = all
                .iter()
                .find(|run| run.run_id == parent_id)
                .cloned()
                .ok_or_else(|| anyhow!("agent run ancestry is incomplete"))?;
            ancestry.push(current.clone());
        }
        Ok((parent, ancestry, current))
    }

    async fn ensure_capacity(&self, root: &AgentRun) -> anyhow::Result<()> {
        let active = self
            .store
            .list_descendant_agent_runs(&root.run_id)
            .await?
            .into_iter()
            .filter(|run| run.status.is_active())
            .count();
        if active >= MAX_ACTIVE_CHILD_RUNS {
            bail!("active child run limit exceeded");
        }
        Ok(())
    }

    async fn register(&self, run: AgentRun, snapshot: Option<Arc<AgentSnapshot>>) -> RunHandle {
        let cancel = CancellationToken::new();
        self.live.lock().await.insert(
            run.run_id.clone(),
            InFlightRun {
                agent_snapshot: snapshot.clone(),
                cancel: cancel.clone(),
            },
        );
        self.emit(
            &run.session_pk,
            &run.run_id,
            run.parent_run_id.clone(),
            run.status,
        );
        RunHandle {
            run,
            agent_snapshot: snapshot,
            cancel,
        }
    }

    async fn transition(
        &self,
        run_id: &str,
        allowed: &[AgentRunStatus],
        to: AgentRunStatus,
        result: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        if self
            .store
            .transition_agent_run(run_id, allowed, to, result, error)
            .await?
        {
            let run = self
                .store
                .get_agent_run(run_id)
                .await?
                .ok_or_else(|| anyhow!("unknown agent run"))?;
            self.emit(&run.session_pk, &run.run_id, run.parent_run_id.clone(), to);
            if to.is_terminal() {
                self.live.lock().await.remove(run_id);
                self.record_terminal(run).await;
            }
        }
        Ok(())
    }

    async fn record_terminal(&self, run: AgentRun) {
        self.terminal
            .lock()
            .await
            .insert(run.run_id.clone(), run.clone());
        let _ = self.terminal_events.send(run);
    }

    /// Wait for a child run's persisted terminal record. This is the delivery
    /// endpoint for the immediate delegating tool caller; it never writes a
    /// session message or creates a user turn.
    pub async fn await_terminal(&self, run_id: &str) -> anyhow::Result<AgentRun> {
        let mut events = self.terminal_events.subscribe();
        if let Some(run) = self.terminal.lock().await.get(run_id).cloned() {
            return Ok(run);
        }
        if let Some(run) = self.store.get_agent_run(run_id).await? {
            if run.status.is_terminal() {
                return Ok(run);
            }
        } else {
            bail!("unknown agent run");
        }
        loop {
            let run = events.recv().await?;
            if run.run_id == run_id {
                return Ok(run);
            }
        }
    }

    /// Read terminal child outcomes for a root run. The root primary can use
    /// this to consume results from any nested delegation without a user turn.
    pub async fn terminal_outcomes_for_root(
        &self,
        root_run_id: &str,
    ) -> anyhow::Result<Vec<AgentRun>> {
        let root = self
            .store
            .get_agent_run(root_run_id)
            .await?
            .ok_or_else(|| anyhow!("unknown agent run"))?;
        if root.parent_run_id.is_some() {
            bail!("terminal outcomes are only available to root runs");
        }
        let mut outcomes = self
            .store
            .list_descendant_agent_runs(root_run_id)
            .await?
            .into_iter()
            .filter(|run| run.status.is_terminal())
            .collect::<Vec<_>>();
        outcomes.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(outcomes)
    }

    fn emit(
        &self,
        session_pk: &str,
        run_id: &str,
        parent_run_id: Option<String>,
        status: AgentRunStatus,
    ) {
        let _ = self.events.send(CoreEvent::AgentRunChanged {
            session_pk: session_pk.to_string(),
            run_id: run_id.to_string(),
            parent_run_id,
            status: status.as_db().to_string(),
        });
    }
}

fn new_run(
    session_pk: &str,
    parent_run_id: Option<&str>,
    retry_of: Option<&str>,
    primary_agent_id: &str,
    snapshot: Option<&AgentSnapshot>,
    agent_kind: AgentRunKind,
    task: &str,
) -> NewAgentRun {
    let (resolved_model, resolved_effort) = snapshot
        .map(|snapshot| model_parts(&snapshot.profile.model))
        .unwrap_or((None, None));
    NewAgentRun {
        run_id: uuid::Uuid::new_v4().to_string(),
        session_pk: session_pk.to_string(),
        parent_run_id: parent_run_id.map(str::to_string),
        retry_of: retry_of.map(str::to_string),
        primary_agent_id: primary_agent_id.to_string(),
        executing_agent_id: snapshot.map(|snapshot| snapshot.profile.id.clone()),
        executing_agent_name_snapshot: snapshot
            .map(|snapshot| snapshot.profile.name.clone())
            .unwrap_or_else(|| "subagent".to_string()),
        agent_kind,
        task: task.to_string(),
        status: AgentRunStatus::Queued,
        resolved_model,
        resolved_effort,
    }
}

fn model_parts(model: &AgentModel) -> (Option<String>, Option<String>) {
    match model {
        AgentModel::Concrete { name, effort } => (Some(name.clone()), effort.clone()),
        AgentModel::Route { route } => (Some(route.clone()), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::bootstrap::{ensure_default_routes, initialize_agent_persistence};
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use tempfile::TempDir;

    async fn runtime() -> (
        Arc<DelegationRuntime>,
        Arc<AgentRegistry>,
        broadcast::Receiver<CoreEvent>,
        TempDir,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open(&directory.path().join("core.sqlite"))
                .await
                .unwrap(),
        );
        for session_pk in ["s", "mixed"] {
            let session_pk = session_pk.to_string();
            store
                .with_conn(move |connection| {
                    connection.execute(
                        "INSERT INTO sessions(session_pk,status,perm_mode,kind,branch_owned,resume_attempts) VALUES (?1,'idle','default','chat',0,0)",
                        [&session_pk],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
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
        ensure_default_routes(&store).await.unwrap();
        let agents_root = directory.path().join("agents");
        for id in ["first", "second", "third", "fourth", "fifth", "sixth"] {
            std::fs::create_dir_all(agents_root.join(id)).unwrap();
        }
        std::fs::write(
            agents_root.join("index.yaml"),
            "schema_version: 1\norder: [first, second, third, fourth, fifth, sixth]\ndefault_agent_id: first\n",
        )
        .unwrap();
        for (id, name) in [
            ("first", "First"),
            ("second", "Second"),
            ("third", "Third"),
            ("fourth", "Fourth"),
            ("fifth", "Fifth"),
            ("sixth", "Sixth"),
        ] {
            std::fs::write(
                agents_root.join(id).join("agent.yaml"),
                format!("schema_version: 1\nid: {id}\nname: {name}\ndescription: test\navatar: {{ color: blue }}\nmodel: {{ name: anthropic/claude-opus-4-8, effort: high }}\npermissions: {{ mode: ask, rules: [] }}\nskills: {{ enabled: [] }}\ntools: {{ native: [], plugins: [], apps: [] }}\nloop: {{ max_turns: 1, max_tool_rounds: 1 }}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            agents_root.join("subagents.yaml"),
            "schema_version: 1\nmodel: { name: anthropic/claude-opus-4-8, effort: high }\n",
        )
        .unwrap();
        let persistence =
            initialize_agent_persistence(directory.path().to_path_buf(), store.clone())
                .await
                .unwrap();
        let (events, receiver) = broadcast::channel(32);
        (
            DelegationRuntime::new(store, persistence.registry.clone(), events),
            persistence.registry,
            receiver,
            directory,
        )
    }

    async fn two_agents(registry: &Arc<AgentRegistry>) -> (Arc<AgentSnapshot>, Arc<AgentSnapshot>) {
        let profile = registry.resolved_snapshot("first").await.unwrap();
        let second = registry.resolved_snapshot("second").await.unwrap();
        (profile, second)
    }

    #[tokio::test]
    async fn rejects_self_delegation() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime
            .begin_primary("s", first.clone(), "root")
            .await
            .unwrap();

        let error = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: root.run.run_id,
                target_agent_id: first.profile.id.clone(),
                task: "self".into(),
                context: None,
                background: false,
            })
            .await
            .err()
            .expect("a primary agent must not delegate to itself");
        assert!(error.to_string().contains("self-delegation"));
    }

    #[tokio::test]
    async fn rejects_ancestry_cycle() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, second) = two_agents(&registry).await;
        let root = runtime
            .begin_primary("s", first.clone(), "root")
            .await
            .unwrap();
        let child = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: root.run.run_id,
                target_agent_id: second.profile.id.clone(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();

        let error = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: child.run.run_id,
                target_agent_id: first.profile.id.clone(),
                task: "cycle".into(),
                context: None,
                background: false,
            })
            .await
            .err()
            .expect("a descendant must not delegate to an ancestor agent");
        assert!(error.to_string().contains("ancestry cycle"));
    }

    #[tokio::test]
    async fn allows_four_main_edges_then_rejects_the_fifth() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let mut parent = root.run.run_id;
        for target_agent_id in ["second", "third", "fourth", "fifth"] {
            parent = runtime
                .queue_main(MainDelegationRequest {
                    parent_run_id: parent,
                    target_agent_id: target_agent_id.into(),
                    task: "main".into(),
                    context: None,
                    background: false,
                })
                .await
                .expect("the first four main delegation edges are allowed")
                .run
                .run_id;
        }

        let error = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: parent,
                target_agent_id: "sixth".into(),
                task: "fifth".into(),
                context: None,
                background: false,
            })
            .await
            .err()
            .expect("the fifth main delegation edge must exceed the depth limit");
        assert!(error.to_string().contains("depth limit"));
    }

    #[tokio::test]
    async fn rejects_ninth_active_child_across_mixed_main_and_subagent_runs() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("mixed", first, "root").await.unwrap();
        let main = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: root.run.run_id.clone(),
                target_agent_id: "second".into(),
                task: "main".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        for number in 0..3 {
            runtime
                .queue_subagent(SubagentRunRequest {
                    parent_run_id: root.run.run_id.clone(),
                    subagent_type: format!("root-{number}"),
                    task: "sub".into(),
                    context: None,
                    background: false,
                })
                .await
                .unwrap();
        }
        runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: main.run.run_id.clone(),
                target_agent_id: "third".into(),
                task: "nested-main".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        for number in 0..3 {
            runtime
                .queue_subagent(SubagentRunRequest {
                    parent_run_id: main.run.run_id.clone(),
                    subagent_type: format!("main-{number}"),
                    task: "sub".into(),
                    context: None,
                    background: false,
                })
                .await
                .unwrap();
        }

        let error = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id,
                subagent_type: "ninth".into(),
                task: "sub".into(),
                context: None,
                background: false,
            })
            .await
            .err()
            .expect("the ninth mixed child must be refused");
        assert!(error.to_string().contains("active child run limit"));
    }

    #[tokio::test]
    async fn cancellation_changes_only_the_requested_subtree() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let sibling = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id.clone(),
                subagent_type: "sibling".into(),
                task: "sibling".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        let child = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id.clone(),
                subagent_type: "child".into(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        let grandchild = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: child.run.run_id.clone(),
                subagent_type: "grandchild".into(),
                task: "grandchild".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();

        runtime.cancel_child("s", &child.run.run_id).await.unwrap();
        for run_id in [&root.run.run_id, &sibling.run.run_id] {
            assert_eq!(
                runtime
                    .store
                    .get_agent_run(run_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                AgentRunStatus::Queued
            );
        }
        for (run_id, cancel) in [
            (&child.run.run_id, &child.cancel),
            (&grandchild.run.run_id, &grandchild.cancel),
        ] {
            assert_eq!(
                runtime
                    .store
                    .get_agent_run(run_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                AgentRunStatus::Cancelled
            );
            assert!(cancel.is_cancelled());
        }
    }

    #[tokio::test]
    async fn terminal_runs_are_immutable() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let child = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id,
                subagent_type: "child".into(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();

        runtime.complete(&child.run.run_id, "done").await.unwrap();
        runtime.fail(&child.run.run_id, "late").await.unwrap();
        runtime.cancel_child("s", &child.run.run_id).await.unwrap();
        let stored = runtime
            .store
            .get_agent_run(&child.run.run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, AgentRunStatus::Completed);
        assert_eq!(stored.result.as_deref(), Some("done"));
        assert_eq!(stored.error, None);
    }

    #[tokio::test]
    async fn retry_resolves_the_current_registry_snapshot() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, second) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let child = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: root.run.run_id,
                target_agent_id: second.profile.id.clone(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        runtime.complete(&child.run.run_id, "done").await.unwrap();

        let mut profile = registry.get("second").await.unwrap().profile;
        profile.name = "Second, updated".into();
        profile.model = AgentModel::Concrete {
            name: "anthropic/claude-opus-4-8".into(),
            effort: Some("high".into()),
        };
        registry
            .update(
                "second",
                crate::agents::types::AgentMutationInput {
                    name: profile.name,
                    description: profile.description,
                    avatar: profile.avatar,
                    model: profile.model,
                    permissions: profile.permissions,
                    skills: profile.skills,
                    tools: profile.tools,
                    loop_settings: profile.loop_settings,
                },
            )
            .await
            .unwrap();

        let retry = runtime.retry_child("s", &child.run.run_id).await.unwrap();
        assert_eq!(retry.retry_of.as_deref(), Some(child.run.run_id.as_str()));
        assert_eq!(retry.executing_agent_name_snapshot, "Second, updated");
        assert_eq!(
            retry.resolved_model.as_deref(),
            Some("anthropic/claude-opus-4-8")
        );
        assert_eq!(retry.resolved_effort.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn live_execution_snapshot_remains_immutable_after_registry_change() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let live_before = runtime.execution_snapshot(&root.run.run_id).await.unwrap();
        let mut profile = registry.get("first").await.unwrap().profile;
        profile.name = "First, updated".into();
        registry
            .update(
                "first",
                crate::agents::types::AgentMutationInput {
                    name: profile.name,
                    description: profile.description,
                    avatar: profile.avatar,
                    model: profile.model,
                    permissions: profile.permissions,
                    skills: profile.skills,
                    tools: profile.tools,
                    loop_settings: profile.loop_settings,
                },
            )
            .await
            .unwrap();

        let live_after = runtime.execution_snapshot(&root.run.run_id).await.unwrap();
        assert!(Arc::ptr_eq(&live_before, &live_after));
        assert_eq!(live_after.profile.name, "First");
        assert_eq!(
            registry
                .resolved_snapshot("first")
                .await
                .unwrap()
                .profile
                .name,
            "First, updated"
        );
    }

    #[tokio::test]
    async fn emits_agent_run_changes_only_after_the_run_is_committed() {
        let (runtime, registry, mut events, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let event = events.recv().await.unwrap();
        let CoreEvent::AgentRunChanged { run_id, status, .. } = event else {
            panic!("beginning a run must emit an AgentRunChanged event");
        };
        assert_eq!(run_id, root.run.run_id);
        assert_eq!(status, AgentRunStatus::Queued.as_db());
        assert_eq!(
            runtime
                .store
                .get_agent_run(&run_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            AgentRunStatus::Queued,
            "the event must follow the committed database row"
        );
    }

    #[tokio::test]
    async fn retry_rejects_a_terminal_child_when_eight_descendants_are_active() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let terminal = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id.clone(),
                subagent_type: "terminal".into(),
                task: "terminal".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        runtime
            .complete(&terminal.run.run_id, "done")
            .await
            .unwrap();
        for number in 0..MAX_ACTIVE_CHILD_RUNS {
            runtime
                .queue_subagent(SubagentRunRequest {
                    parent_run_id: root.run.run_id.clone(),
                    subagent_type: format!("active-{number}"),
                    task: "active".into(),
                    context: None,
                    background: false,
                })
                .await
                .unwrap();
        }

        let error = runtime
            .retry_child("s", &terminal.run.run_id)
            .await
            .expect_err("retry must count as the ninth active descendant");
        assert!(error.to_string().contains("active child run limit"));
    }

    #[tokio::test]
    async fn terminal_child_outcome_reaches_its_immediate_caller_and_root_without_user_turns() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, second) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let parent = runtime
            .queue_main(MainDelegationRequest {
                parent_run_id: root.run.run_id.clone(),
                target_agent_id: second.profile.id.clone(),
                task: "parent".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        let child = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: parent.run.run_id.clone(),
                subagent_type: "nested".into(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        let waiter_runtime = runtime.clone();
        let child_id = child.run.run_id.clone();
        let waiter = tokio::spawn(async move { waiter_runtime.await_terminal(&child_id).await });
        tokio::task::yield_now().await;

        runtime
            .fail(&child.run.run_id, "nested failure")
            .await
            .unwrap();
        let delivered = waiter.await.unwrap().unwrap();
        assert_eq!(delivered.run_id, child.run.run_id);
        assert_eq!(delivered.status, AgentRunStatus::Failed);
        assert_eq!(delivered.error.as_deref(), Some("nested failure"));
        assert_eq!(
            runtime
                .terminal_outcomes_for_root(&root.run.run_id)
                .await
                .unwrap(),
            vec![delivered]
        );
        assert!(runtime.store.list_messages("s").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn recovery_emits_every_committed_interruption_with_its_persisted_reason() {
        let (runtime, registry, mut events, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();
        let child = runtime
            .queue_subagent(SubagentRunRequest {
                parent_run_id: root.run.run_id.clone(),
                subagent_type: "child".into(),
                task: "child".into(),
                context: None,
                background: false,
            })
            .await
            .unwrap();
        runtime.mark_running(&child.run.run_id).await.unwrap();
        while events.try_recv().is_ok() {}

        assert_eq!(runtime.recover_after_restart().await.unwrap(), 2);
        let mut changed = Vec::new();
        for _ in 0..2 {
            let CoreEvent::AgentRunChanged {
                session_pk,
                run_id,
                parent_run_id,
                status,
            } = events.recv().await.unwrap()
            else {
                panic!("recovery must emit AgentRunChanged");
            };
            let persisted = runtime.store.get_agent_run(&run_id).await.unwrap().unwrap();
            assert_eq!(persisted.status, AgentRunStatus::Interrupted);
            assert_eq!(
                persisted.error.as_deref(),
                Some(RESTART_INTERRUPTION_REASON)
            );
            changed.push((session_pk, run_id, parent_run_id, status));
        }
        assert_eq!(
            changed,
            vec![
                (
                    "s".into(),
                    root.run.run_id.clone(),
                    None,
                    AgentRunStatus::Interrupted.as_db().into(),
                ),
                (
                    "s".into(),
                    child.run.run_id.clone(),
                    Some(root.run.run_id),
                    AgentRunStatus::Interrupted.as_db().into(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn restart_recovery_marks_incomplete_runs_with_the_exact_reason_once() {
        let (runtime, registry, _, _directory) = runtime().await;
        let (first, _) = two_agents(&registry).await;
        let root = runtime.begin_primary("s", first, "root").await.unwrap();

        assert_eq!(runtime.recover_after_restart().await.unwrap(), 1);
        assert_eq!(runtime.recover_after_restart().await.unwrap(), 0);
        let recovered = runtime
            .store
            .get_agent_run(&root.run.run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status, AgentRunStatus::Interrupted);
        assert_eq!(
            recovered.error.as_deref(),
            Some(RESTART_INTERRUPTION_REASON)
        );
    }
}
