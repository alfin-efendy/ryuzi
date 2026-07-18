use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use anyhow::{anyhow, Context};
use indexmap::IndexMap;
use serde_yaml::Value;
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::llm_router::model_effort::ModelPreferenceKey;
use crate::llm_router::{model_capabilities, routes};
use crate::paths;
use crate::store::Store;

use super::learning_queue::LearningQueue;
#[cfg(test)]
use super::transaction::TransactionFailpoint;
use super::transaction::{recover_transactions, registry_generation, AgentTransaction};
use super::types::*;
use super::yaml::{
    parse_agent_index, parse_agent_index_document, parse_agent_profile,
    parse_agent_profile_document, parse_subagent_config, parse_subagent_config_document,
    render_agent_index, render_agent_index_document, render_agent_profile,
    render_agent_profile_document, render_subagent_config, render_subagent_config_document,
};

pub struct AgentRegistry {
    // Task 3's transaction layer persists validated candidates beneath this root.
    #[allow(dead_code)]
    config_root: PathBuf,
    store: Arc<Store>,
    state: RwLock<Arc<RegistryState>>,
    mutations: Mutex<()>,
    learning_queue: OnceLock<Weak<LearningQueue>>,
    #[cfg(test)]
    failpoint: std::sync::atomic::AtomicU8,
}

#[derive(Debug)]
struct RegistryState {
    index: AgentIndex,
    agents: IndexMap<AgentId, Arc<AgentSnapshot>>,
    subagents: SubagentConfig,
    generation: String,
    recovery: Vec<AgentRecoveryNotice>,
    #[allow(dead_code)]
    repairs: Vec<ProfileRepairRecord>,
}

#[derive(Debug, Clone)]
struct ProfileRepairRecord {
    #[allow(dead_code)]
    path: PathBuf,
}

/// Owns the pre-commit queue rollback independently from the delete future.
/// Dropping the command sender (including task abortion) tells the worker to
/// acquire the apply fence before reopening enqueue, preserving fence order.
enum DeleteBlockCommand {
    Commit,
    Rollback { acquire_fence: bool },
}

struct DeleteBlockRollback {
    command: Option<oneshot::Sender<DeleteBlockCommand>>,
    completion: Option<oneshot::Receiver<anyhow::Result<()>>>,
    committed: Arc<std::sync::atomic::AtomicBool>,
}

impl DeleteBlockRollback {
    fn arm(queue: Arc<LearningQueue>, agent_id: AgentId) -> Self {
        let (command_tx, command_rx) = oneshot::channel();
        let (completion_tx, completion_rx) = oneshot::channel();
        let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_committed = Arc::clone(&committed);
        tokio::spawn(async move {
            let command = command_rx.await.unwrap_or_else(|_| {
                if worker_committed.load(std::sync::atomic::Ordering::Acquire) {
                    DeleteBlockCommand::Commit
                } else {
                    DeleteBlockCommand::Rollback {
                        acquire_fence: true,
                    }
                }
            });
            let result = match command {
                DeleteBlockCommand::Commit => Ok(()),
                DeleteBlockCommand::Rollback {
                    acquire_fence: false,
                } => queue.unblock(&agent_id).await,
                DeleteBlockCommand::Rollback {
                    acquire_fence: true,
                } => match queue.acquire_apply_fence(&agent_id).await {
                    Ok(fence) => {
                        let result = queue.unblock(&agent_id).await;
                        drop(fence);
                        result
                    }
                    Err(error) => queue.unblock(&agent_id).await.map_err(|unblock| {
                        anyhow!("{error}; queue rollback also failed: {unblock}")
                    }),
                },
            };
            let _ = completion_tx.send(result);
        });
        Self {
            command: Some(command_tx),
            completion: Some(completion_rx),
            committed,
        }
    }

    async fn rollback(mut self, acquire_fence: bool) -> anyhow::Result<()> {
        if let Some(command) = self.command.take() {
            let _ = command.send(DeleteBlockCommand::Rollback { acquire_fence });
        }
        self.wait().await
    }

    fn commit_marker(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.committed)
    }

    fn disarm(&self) {
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    async fn commit(mut self) {
        self.disarm();
        if let Some(command) = self.command.take() {
            let _ = command.send(DeleteBlockCommand::Commit);
        }
        let _ = self.wait().await;
    }

    async fn wait(&mut self) -> anyhow::Result<()> {
        self.completion
            .take()
            .expect("rollback completion is consumed once")
            .await
            .map_err(|_| anyhow!("delete queue rollback worker stopped unexpectedly"))?
    }
}

#[derive(Debug)]
pub enum AgentRegistryError {
    NotFound(String),
    DuplicateId(String),
    DuplicateName(String),
    LastAgent,
    Invalid(Vec<AgentValidationIssue>),
    Io(anyhow::Error),
}

impl std::fmt::Display for AgentRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(formatter, "agent `{id}` was not found"),
            Self::DuplicateId(id) => write!(formatter, "agent id `{id}` already exists"),
            Self::DuplicateName(name) => write!(formatter, "agent name `{name}` already exists"),
            Self::LastAgent => formatter.write_str("the last agent cannot be deleted"),
            Self::Invalid(_) => formatter.write_str("agent candidate is invalid"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AgentRegistryError {}

impl From<anyhow::Error> for AgentRegistryError {
    fn from(value: anyhow::Error) -> Self {
        Self::Io(value)
    }
}

impl AgentRegistry {
    pub async fn load(config_root: PathBuf, store: Arc<Store>) -> anyhow::Result<Self> {
        let transaction_recovery = recover_transactions(&config_root)?;
        let agents_root = paths::agents_dir_in(&config_root);
        let index_path = agents_root.join("index.yaml");
        let parsed_index = read_to_string(&index_path)
            .and_then(|raw| parse_agent_index(&raw).map_err(std::io::Error::other));
        let recovering = parsed_index.is_err();
        let mut repairs = Vec::new();
        let mut profiles = IndexMap::new();
        let mut parse_issues = IndexMap::<AgentId, Vec<AgentValidationIssue>>::new();

        let ordered_ids = if let Ok(index) = &parsed_index {
            index.order.clone()
        } else {
            discover_agent_directories(&agents_root)?
        };

        for directory_id in ordered_ids {
            let path = paths::agent_dir_in(&config_root, &directory_id).join("agent.yaml");
            match read_to_string(&path)
                .and_then(|raw| parse_agent_profile(&raw).map_err(std::io::Error::other))
            {
                Ok(profile) => {
                    profiles.insert(directory_id, profile);
                }
                Err(error) => {
                    if let Ok(raw) = std::fs::read_to_string(&path) {
                        if let Some(profile) = recover_minimal_profile(&raw, &directory_id) {
                            parse_issues.insert(
                                directory_id.clone(),
                                vec![issue(
                                    "profile",
                                    format!("profile could not be parsed: {error}"),
                                )],
                            );
                            repairs.push(ProfileRepairRecord { path });
                            profiles.insert(directory_id, profile);
                        }
                    }
                }
            }
        }

        let subagents_path = agents_root.join("subagents.yaml");
        let subagents = parse_subagent_config(
            &std::fs::read_to_string(&subagents_path)
                .with_context(|| format!("failed to read {}", subagents_path.display()))?,
        )
        .with_context(|| format!("failed to parse {}", subagents_path.display()))?;

        let mut recovery = transaction_recovery;
        recovery.extend(recovering.then(|| AgentRecoveryNotice {
            code: "index-rebuilt".into(),
            message: "Recovered agent registry order from agent directories.".into(),
        }));

        let mut index = parsed_index.unwrap_or_else(|_| AgentIndex {
            schema_version: AGENT_SCHEMA_VERSION,
            order: profiles.keys().cloned().collect(),
            default_agent_id: profiles.keys().next().cloned().unwrap_or_default(),
            extensions: IndexMap::new(),
        });
        let mut validation =
            validate_registry_candidate(&store, &index, &profiles, &subagents).await;
        merge_parse_issues(&mut validation, parse_issues);

        if recovering {
            index.default_agent_id = profiles
                .keys()
                .find(|id| validation.get(*id).is_none_or(Vec::is_empty))
                .cloned()
                .or_else(|| profiles.keys().next().cloned())
                .unwrap_or_default();
            validation = validate_registry_candidate(&store, &index, &profiles, &subagents).await;
            // Parser failures must survive the second validation pass.
            for (id, profile) in &profiles {
                if repairs.iter().any(|record| {
                    record
                        .path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|value| value == id.as_str())
                }) {
                    validation.entry(id.clone()).or_default().push(issue(
                        "profile",
                        format!("profile `{id}` could not be parsed"),
                    ));
                }
                let _ = profile;
            }
        }

        let agents = profiles
            .into_iter()
            .map(|(id, profile)| {
                let validation = validation.shift_remove(&id).unwrap_or_default();
                (
                    id,
                    Arc::new(AgentSnapshot {
                        executable: validation.is_empty(),
                        profile,
                        validation,
                    }),
                )
            })
            .collect();
        let generation = registry_generation(&config_root)?;
        Ok(Self {
            config_root,
            store,
            state: RwLock::new(Arc::new(RegistryState {
                index,
                agents,
                subagents,
                generation,
                recovery,
                repairs,
            })),
            mutations: Mutex::new(()),
            learning_queue: OnceLock::new(),
            #[cfg(test)]
            failpoint: std::sync::atomic::AtomicU8::new(0),
        })
    }

    pub fn attach_learning_queue(&self, queue: Weak<LearningQueue>) -> anyhow::Result<()> {
        self.learning_queue
            .set(queue)
            .map_err(|_| anyhow!("learning queue is already attached"))
    }

    pub async fn default_agent_id(&self) -> String {
        self.state.read().await.index.default_agent_id.clone()
    }

    /// Temporary Plan 2 identity seam: the registry's current default agent
    /// id, falling back to the built-in `ryuzi` id when no registry has been
    /// bootstrapped yet (fresh installs, bare test states). Plan 4 replaces
    /// this default selection with the session's persisted owner.
    pub async fn default_agent_id_or_builtin(
        config_root: std::path::PathBuf,
        store: Arc<crate::store::Store>,
    ) -> String {
        match AgentRegistry::load(config_root, store).await {
            Ok(registry) => registry.default_agent_id().await,
            Err(_) => "ryuzi".to_owned(),
        }
    }

    pub async fn snapshot(&self) -> AgentRegistrySnapshot {
        let state = self.state.read().await.clone();
        snapshot_from_state(&state)
    }

    /// Appends bootstrap-time notices (transaction recovery, default agent
    /// creation) to the published snapshot without touching disk state.
    pub(crate) async fn append_recovery_notices(&self, notices: Vec<AgentRecoveryNotice>) {
        if notices.is_empty() {
            return;
        }
        let mut guard = self.state.write().await;
        let current = guard.as_ref();
        let mut recovery = current.recovery.clone();
        recovery.extend(notices);
        *guard = Arc::new(RegistryState {
            index: current.index.clone(),
            agents: current.agents.clone(),
            subagents: current.subagents.clone(),
            generation: current.generation.clone(),
            recovery,
            repairs: current.repairs.clone(),
        });
    }

    pub async fn get(&self, agent_id: &str) -> anyhow::Result<AgentSnapshot> {
        Ok(self.resolved_snapshot(agent_id).await?.as_ref().clone())
    }

    pub async fn resolved_snapshot(&self, agent_id: &str) -> anyhow::Result<Arc<AgentSnapshot>> {
        self.state
            .read()
            .await
            .agents
            .get(agent_id)
            .cloned()
            .ok_or_else(|| anyhow!("agent `{agent_id}` was not found"))
    }

    pub async fn create(
        &self,
        input: AgentMutationInput,
    ) -> Result<AgentSnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        let id = new_agent_id();
        let profile = profile_from_input(id.clone(), input);
        let state = self.state.read().await.clone();
        let mut profiles = typed_profiles(&state);
        profiles.insert(id.clone(), profile.clone());
        let mut index = state.index.clone();
        index.order.push(id.clone());
        self.validate_mutation(&index, &profiles, &state.subagents, &id)
            .await?;
        let snapshot = self
            .commit_candidate(index, profiles, state.subagents.clone(), Vec::new())
            .await?;
        Ok(snapshot
            .agents
            .into_iter()
            .find(|agent| agent.profile.id == id)
            .expect("committed agent"))
    }

    pub async fn update(
        &self,
        agent_id: &str,
        input: AgentMutationInput,
    ) -> Result<AgentSnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        let state = self.state.read().await.clone();
        if !state.agents.contains_key(agent_id) {
            return Err(AgentRegistryError::NotFound(agent_id.into()));
        }
        let mut profiles = typed_profiles(&state);
        profiles.insert(agent_id.into(), profile_from_input(agent_id.into(), input));
        self.validate_mutation(&state.index, &profiles, &state.subagents, agent_id)
            .await?;
        let snapshot = self
            .commit_candidate(
                state.index.clone(),
                profiles,
                state.subagents.clone(),
                Vec::new(),
            )
            .await?;
        Ok(snapshot
            .agents
            .into_iter()
            .find(|agent| agent.profile.id == agent_id)
            .expect("committed updated agent"))
    }

    pub async fn duplicate(&self, agent_id: &str) -> Result<AgentSnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        let state = self.state.read().await.clone();
        let source = state
            .agents
            .get(agent_id)
            .ok_or_else(|| AgentRegistryError::NotFound(agent_id.into()))?;
        let id = new_agent_id();
        let mut profile = source.profile.clone();
        profile.id = id.clone();
        profile.name = unique_copy_name(&profile.name, &state.agents);
        let mut profiles = typed_profiles(&state);
        profiles.insert(id.clone(), profile);
        let mut index = state.index.clone();
        index.order.push(id.clone());
        self.validate_mutation(&index, &profiles, &state.subagents, &id)
            .await?;
        let snapshot = self
            .commit_candidate_with_profile_sources(
                index,
                profiles,
                state.subagents.clone(),
                Vec::new(),
                HashMap::from([(id.clone(), agent_id.to_owned())]),
                None,
            )
            .await?;
        Ok(snapshot
            .agents
            .into_iter()
            .find(|agent| agent.profile.id == id)
            .expect("committed duplicate"))
    }

    pub async fn delete(
        &self,
        agent_id: &str,
    ) -> Result<AgentRegistrySnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        let state = self.state.read().await.clone();
        if !state.agents.contains_key(agent_id) {
            return Err(AgentRegistryError::NotFound(agent_id.into()));
        }
        if state.agents.len() == 1 {
            return Err(AgentRegistryError::LastAgent);
        }
        let mut profiles = typed_profiles(&state);
        profiles.shift_remove(agent_id);
        let mut index = state.index.clone();
        let deleted_position = index
            .order
            .iter()
            .position(|id| id == agent_id)
            .expect("loaded agent is present in index order");
        index.order.retain(|id| id != agent_id);
        if index.default_agent_id == agent_id {
            let next_position = deleted_position.min(index.order.len() - 1);
            index.default_agent_id = index.order[next_position].clone();
        }
        self.validate_candidate(&index, &profiles, &state.subagents)
            .await?;
        let queue = self.learning_queue.get().and_then(Weak::upgrade);
        let mut block_rollback = if let Some(queue) = &queue {
            queue.block(agent_id).await?;
            Some(DeleteBlockRollback::arm(
                Arc::clone(queue),
                agent_id.to_owned(),
            ))
        } else {
            None
        };
        let apply_fence = if let Some(queue) = &queue {
            match queue.acquire_apply_fence(agent_id).await {
                Ok(fence) => Some(fence),
                Err(error) => {
                    if let Some(rollback) = block_rollback.take() {
                        rollback.rollback(false).await?;
                    }
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        let commit_marker = block_rollback
            .as_ref()
            .map(DeleteBlockRollback::commit_marker);
        let result = self
            .commit_candidate_with_profile_sources(
                index,
                profiles,
                state.subagents.clone(),
                vec![agent_id.into()],
                HashMap::new(),
                commit_marker,
            )
            .await;
        match result {
            Ok(mut snapshot) => {
                if let Some(rollback) = block_rollback.as_ref() {
                    // `AgentTransaction::commit` is the filesystem commit point.
                    // Cancellation after this point must retain the durable block.
                    rollback.disarm();
                }
                if let Some(queue) = &queue {
                    if let Err(error) = queue.discard_unconsumed(agent_id).await {
                        snapshot.recovery.push(AgentRecoveryNotice {
                            code: "learning_cleanup_pending".into(),
                            message: format!(
                                "agent `{agent_id}` was deleted, but learning queue cleanup is pending: {error}"
                            ),
                        });
                    }
                }
                drop(apply_fence);
                if let Some(rollback) = block_rollback.take() {
                    rollback.commit().await;
                }
                Ok(snapshot)
            }
            Err(error) => {
                drop(apply_fence);
                if let Some(rollback) = block_rollback.take() {
                    rollback.rollback(false).await?;
                }
                Err(error)
            }
        }
    }

    pub async fn set_default(
        &self,
        agent_id: &str,
    ) -> Result<AgentRegistrySnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        let state = self.state.read().await.clone();
        if !state.agents.contains_key(agent_id) {
            return Err(AgentRegistryError::NotFound(agent_id.into()));
        }
        let mut index = state.index.clone();
        index.default_agent_id = agent_id.into();
        self.validate_candidate(&index, &typed_profiles(&state), &state.subagents)
            .await?;
        self.commit_candidate(
            index,
            typed_profiles(&state),
            state.subagents.clone(),
            Vec::new(),
        )
        .await
    }

    pub async fn set_subagent_model(
        &self,
        model: AgentModel,
    ) -> Result<AgentRegistrySnapshot, AgentRegistryError> {
        let _guard = self.mutations.lock().await;
        crate::agents::bootstrap::ensure_default_routes(&self.store).await?;
        let state = self.state.read().await.clone();
        let subagents = SubagentConfig {
            schema_version: AGENT_SCHEMA_VERSION,
            model,
        };
        self.validate_candidate(&state.index, &typed_profiles(&state), &subagents)
            .await?;
        let issues = validate_model(&self.store, &subagents.model).await;
        if !issues.is_empty() {
            return Err(AgentRegistryError::Invalid(issues));
        }
        self.commit_candidate(
            state.index.clone(),
            typed_profiles(&state),
            subagents,
            Vec::new(),
        )
        .await
    }

    async fn commit_candidate(
        &self,
        index: AgentIndex,
        profiles: IndexMap<AgentId, AgentProfile>,
        subagents: SubagentConfig,
        deleted_agent_ids: Vec<AgentId>,
    ) -> Result<AgentRegistrySnapshot, AgentRegistryError> {
        self.commit_candidate_with_profile_sources(
            index,
            profiles,
            subagents,
            deleted_agent_ids,
            HashMap::new(),
            None,
        )
        .await
    }

    async fn commit_candidate_with_profile_sources(
        &self,
        index: AgentIndex,
        profiles: IndexMap<AgentId, AgentProfile>,
        subagents: SubagentConfig,
        deleted_agent_ids: Vec<AgentId>,
        profile_sources: HashMap<AgentId, AgentId>,
        commit_marker: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<AgentRegistrySnapshot, AgentRegistryError> {
        self.validate_candidate(&index, &profiles, &subagents)
            .await?;
        let disk_image = self.render_disk_image(
            &index,
            &profiles,
            &subagents,
            deleted_agent_ids,
            &profile_sources,
        )?;
        let expected_generation = self.state.read().await.generation.clone();
        let transaction =
            AgentTransaction::prepare(&self.config_root, &disk_image, Some(&expected_generation))?;
        #[cfg(test)]
        let transaction = transaction.with_failpoint(
            match self.failpoint.load(std::sync::atomic::Ordering::SeqCst) {
                1 => TransactionFailpoint::BeforeIndexReplace,
                2 => TransactionFailpoint::AfterIndexReplaceBeforeCommitMarker,
                _ => TransactionFailpoint::None,
            },
        );
        // The committed generation is captured inside `commit()` while the
        // transaction still holds the exclusive registry file lock. Reading
        // the disk again here would race a foreign process committing after
        // the lock is released, caching its generation over our stale image.
        let generation = transaction.commit()?;
        if let Some(marker) = commit_marker {
            marker.store(true, std::sync::atomic::Ordering::Release);
        }
        #[cfg(test)]
        if self.failpoint.load(std::sync::atomic::Ordering::SeqCst) == 3 {
            // Simulates a foreign process that acquires the registry lock and
            // commits in the window after our commit released the lock but
            // before this instance caches the on-disk generation.
            let index_path = paths::agents_dir_in(&self.config_root).join("index.yaml");
            let mut raw = std::fs::read_to_string(&index_path).map_err(anyhow::Error::from)?;
            raw.push_str("# foreign commit\n");
            std::fs::write(&index_path, raw).map_err(anyhow::Error::from)?;
        }
        let agents = profiles
            .into_iter()
            .map(|(id, profile)| {
                (
                    id,
                    Arc::new(AgentSnapshot {
                        profile,
                        executable: true,
                        validation: Vec::new(),
                    }),
                )
            })
            .collect();
        let next = Arc::new(RegistryState {
            index,
            agents,
            subagents,
            generation,
            recovery: self.state.read().await.recovery.clone(),
            repairs: Vec::new(),
        });
        let snapshot = snapshot_from_state(&next);
        *self.state.write().await = next;
        Ok(snapshot)
    }

    fn render_disk_image(
        &self,
        index: &AgentIndex,
        profiles: &IndexMap<AgentId, AgentProfile>,
        subagents: &SubagentConfig,
        deleted_agent_ids: Vec<AgentId>,
        profile_sources: &HashMap<AgentId, AgentId>,
    ) -> anyhow::Result<RegistryDiskImage> {
        let index_path = paths::agents_dir_in(&self.config_root).join("index.yaml");
        let index_yaml = match std::fs::read_to_string(&index_path)
            .ok()
            .and_then(|raw| parse_agent_index_document(&raw).ok())
        {
            Some(mut document) => {
                document.merge_typed(index.clone());
                render_agent_index_document(&document)?
            }
            None => render_agent_index(index)?,
        };
        let subagents_path = paths::agents_dir_in(&self.config_root).join("subagents.yaml");
        let subagents_yaml = match std::fs::read_to_string(&subagents_path)
            .ok()
            .and_then(|raw| parse_subagent_config_document(&raw).ok())
        {
            Some(mut document) => {
                document.merge_typed(subagents.clone());
                render_subagent_config_document(&document)?
            }
            None => render_subagent_config(subagents)?,
        };
        let mut agents = IndexMap::new();
        for (id, profile) in profiles {
            let source_id = profile_sources.get(id).unwrap_or(id);
            let profile_path = paths::agent_dir_in(&self.config_root, source_id).join("agent.yaml");
            let yaml = match std::fs::read_to_string(&profile_path)
                .ok()
                .and_then(|raw| parse_agent_profile_document(&raw).ok())
            {
                Some(mut document) => {
                    document.merge_typed(profile.clone());
                    render_agent_profile_document(&document)?
                }
                None => render_agent_profile(profile)?,
            };
            agents.insert(id.clone(), yaml);
        }
        Ok(RegistryDiskImage {
            index_yaml,
            subagents_yaml,
            agents,
            deleted_agent_ids,
        })
    }

    async fn validate_mutation(
        &self,
        index: &AgentIndex,
        profiles: &IndexMap<AgentId, AgentProfile>,
        subagents: &SubagentConfig,
        id: &str,
    ) -> Result<(), AgentRegistryError> {
        crate::agents::bootstrap::ensure_default_routes(&self.store).await?;
        let issues = validate_registry_candidate(&self.store, index, profiles, subagents).await;
        let current = issues.get(id).cloned().unwrap_or_default();
        if let Some(issue) = current
            .iter()
            .find(|issue| issue.field == "name" && issue.message.contains("unique"))
        {
            return Err(AgentRegistryError::DuplicateName(issue.message.clone()));
        }
        let all = issues.into_values().flatten().collect::<Vec<_>>();
        if all.is_empty() {
            Ok(())
        } else {
            Err(AgentRegistryError::Invalid(all))
        }
    }

    async fn validate_candidate(
        &self,
        index: &AgentIndex,
        profiles: &IndexMap<AgentId, AgentProfile>,
        subagents: &SubagentConfig,
    ) -> Result<(), AgentRegistryError> {
        let issues = validate_registry_candidate(&self.store, index, profiles, subagents).await;
        let all = issues.into_values().flatten().collect::<Vec<_>>();
        if all.is_empty() {
            Ok(())
        } else {
            Err(AgentRegistryError::Invalid(all))
        }
    }
}

pub fn validate_agent_id(value: &str) -> Result<(), AgentValidationIssue> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(issue(
            "id",
            "agent id must match [a-z][a-z0-9-]{0,63} and cannot end with '-'",
        ))
    }
}

pub async fn validate_profile(store: &Store, profile: &AgentProfile) -> Vec<AgentValidationIssue> {
    let mut issues = Vec::new();
    if profile.schema_version != AGENT_SCHEMA_VERSION {
        issues.push(issue("schema_version", "schema version must be 1"));
    }
    if let Err(value) = validate_agent_id(&profile.id) {
        issues.push(value);
    }
    require_nonblank(&mut issues, "name", &profile.name);
    require_nonblank(&mut issues, "description", &profile.description);
    require_nonblank(&mut issues, "avatar.color", &profile.avatar.color);
    if profile.loop_settings.max_turns == 0 {
        issues.push(issue("loop.max_turns", "max turns must be positive"));
    }
    if profile.loop_settings.max_tool_rounds == 0 {
        issues.push(issue(
            "loop.max_tool_rounds",
            "max tool rounds must be positive",
        ));
    }
    let mut rule_ids = HashSet::new();
    for (index, rule) in profile.permissions.rules.iter().enumerate() {
        require_nonblank(
            &mut issues,
            &format!("permissions.rules[{index}].id"),
            &rule.id,
        );
        require_nonblank(
            &mut issues,
            &format!("permissions.rules[{index}].tool"),
            &rule.tool,
        );
        if !rule_ids.insert(rule.id.trim().to_ascii_lowercase()) {
            issues.push(issue(
                format!("permissions.rules[{index}].id"),
                "permission rule ids must be unique",
            ));
        }
        if rule
            .command_prefix
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            issues.push(issue(
                format!("permissions.rules[{index}].command_prefix"),
                "command prefix cannot be blank",
            ));
        }
    }
    validate_references(&mut issues, "skills", &profile.skills);
    validate_references(&mut issues, "tools.native", &profile.tools.native);
    validate_references(&mut issues, "tools.plugins", &profile.tools.plugins);
    validate_references(&mut issues, "tools.apps", &profile.tools.apps);
    issues.extend(validate_model(store, &profile.model).await);
    issues
}

pub async fn validate_registry_candidate(
    store: &Store,
    index: &AgentIndex,
    profiles: &IndexMap<AgentId, AgentProfile>,
    subagents: &SubagentConfig,
) -> IndexMap<AgentId, Vec<AgentValidationIssue>> {
    let mut result = IndexMap::new();
    for (directory_id, profile) in profiles {
        let mut issues = validate_profile(store, profile).await;
        if directory_id != &profile.id {
            issues.push(issue(
                "id",
                format!(
                    "profile id `{}` must match directory `{directory_id}`",
                    profile.id
                ),
            ));
        }
        result.insert(directory_id.clone(), issues);
    }

    let mut registry_issues = Vec::new();
    if index.schema_version != AGENT_SCHEMA_VERSION {
        registry_issues.push(issue("index.schema_version", "schema version must be 1"));
    }
    if subagents.schema_version != AGENT_SCHEMA_VERSION {
        registry_issues.push(issue(
            "subagents.schema_version",
            "schema version must be 1",
        ));
    }
    let mut order_ids = HashSet::new();
    for id in &index.order {
        if !order_ids.insert(id.clone()) {
            registry_issues.push(issue(
                "index.order",
                format!("agent id `{id}` is duplicated"),
            ));
        }
        if !profiles.contains_key(id) {
            registry_issues.push(issue(
                "index.order",
                format!("agent id `{id}` has no matching profile"),
            ));
        }
    }
    for id in profiles.keys().filter(|id| !order_ids.contains(*id)) {
        result.entry(id.clone()).or_default().push(issue(
            "id",
            format!("agent id `{id}` is missing from index order"),
        ));
    }
    if !profiles.contains_key(&index.default_agent_id) {
        registry_issues.push(issue(
            "index.default_agent_id",
            "default agent must be present in the registry",
        ));
    }

    let mut names = HashMap::<String, Vec<AgentId>>::new();
    for (id, profile) in profiles {
        names
            .entry(profile.name.trim().to_ascii_lowercase())
            .or_default()
            .push(id.clone());
    }
    for duplicates in names.values().filter(|ids| ids.len() > 1) {
        for id in duplicates {
            result.entry(id.clone()).or_default().push(issue(
                "name",
                "trimmed agent names must be unique (case-insensitive)",
            ));
        }
    }
    let subagent_issues = validate_model(store, &subagents.model).await;
    registry_issues.extend(
        subagent_issues
            .into_iter()
            .map(|value| issue(format!("subagents.{}", value.field), value.message)),
    );
    if !registry_issues.is_empty() {
        for issues in result.values_mut() {
            issues.extend(registry_issues.clone());
        }
    }
    result
}

pub fn split_canonical_model(value: &str) -> Result<(&str, &str), AgentValidationIssue> {
    let Some((family, model)) = value.split_once('/') else {
        return Err(issue(
            "model.name",
            "concrete model must use canonical `family/model` syntax",
        ));
    };
    if family.trim().is_empty() || model.trim().is_empty() || model.contains('/') {
        return Err(issue(
            "model.name",
            "concrete model must use canonical `family/model` syntax",
        ));
    }
    Ok((family, model))
}

async fn validate_model(store: &Store, value: &AgentModel) -> Vec<AgentValidationIssue> {
    match value {
        AgentModel::Concrete { name, effort } => {
            let (family, model) = match split_canonical_model(name) {
                Ok(parts) => parts,
                Err(error) => return vec![error],
            };
            if effort.as_ref().is_some_and(|value| value.trim().is_empty()) {
                return vec![issue("model.effort", "effort cannot be blank")];
            }
            let key = ModelPreferenceKey {
                family: family.to_owned(),
                model: model.to_owned(),
            };
            let available = match model_capabilities::concrete_model_is_available(store, &key).await
            {
                Ok(value) => value,
                Err(error) => {
                    return vec![issue(
                        "model.name",
                        format!("failed to inspect model availability: {error}"),
                    )];
                }
            };
            if !available {
                return vec![issue(
                    "model.name",
                    format!("model `{name}` is not served by an enabled connection"),
                )];
            }
            if let Some(effort) = effort {
                match model_capabilities::resolve_for_model(store, &key).await {
                    Ok(capabilities) if !capabilities.supports(effort) => {
                        return vec![issue(
                            "model.effort",
                            format!("effort `{effort}` is unsupported for `{name}`"),
                        )];
                    }
                    Err(error) => {
                        return vec![issue(
                            "model.effort",
                            format!("failed to resolve effort capabilities: {error}"),
                        )];
                    }
                    _ => {}
                }
            }
            Vec::new()
        }
        AgentModel::Route { route } => {
            if route.trim().is_empty() {
                return vec![issue("model.route", "route cannot be blank")];
            }
            match routes::list_model_routes(store).await {
                Ok(values) if routes::route_by_name(&values, route).is_none() => vec![issue(
                    "model.route",
                    format!("route `{route}` does not exist or is not executable"),
                )],
                Ok(_) => Vec::new(),
                Err(error) => vec![issue(
                    "model.route",
                    format!("failed to inspect model routes: {error}"),
                )],
            }
        }
    }
}

fn recover_minimal_profile(raw: &str, directory_id: &str) -> Option<AgentProfile> {
    let value: Value = serde_yaml::from_str(raw).ok()?;
    let mapping = value.as_mapping()?;
    let string = |key: &str| {
        mapping
            .get(Value::String(key.into()))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let id = string("id").unwrap_or_else(|| directory_id.into());
    let name = string("name")?;
    Some(AgentProfile {
        schema_version: AGENT_SCHEMA_VERSION,
        id,
        name,
        description: "Invalid agent profile".into(),
        avatar: AgentAvatar {
            color: "gray".into(),
        },
        model: AgentModel::Concrete {
            name: "invalid/missing".into(),
            effort: None,
        },
        personality: crate::agents::personality::AgentPersonality::default_profile(),
        permissions: AgentPermissions {
            mode: crate::PermMode::Default,
            rules: Vec::new(),
        },
        skills: Vec::new(),
        tools: AgentTools {
            native: Vec::new(),
            plugins: Vec::new(),
            apps: Vec::new(),
        },
        loop_settings: AgentLoop {
            max_turns: 1,
            max_tool_rounds: 1,
        },
    })
}

pub(crate) fn discover_agent_directories(root: &Path) -> anyhow::Result<Vec<AgentId>> {
    let mut ids = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    ids.sort();
    Ok(ids)
}

fn read_to_string(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

fn merge_parse_issues(
    validation: &mut IndexMap<AgentId, Vec<AgentValidationIssue>>,
    parse_issues: IndexMap<AgentId, Vec<AgentValidationIssue>>,
) {
    for (id, issues) in parse_issues {
        validation.entry(id).or_default().extend(issues);
    }
}

fn require_nonblank(issues: &mut Vec<AgentValidationIssue>, field: &str, value: &str) {
    if value.trim().is_empty() {
        issues.push(issue(field, format!("{field} cannot be blank")));
    }
}

fn validate_references(issues: &mut Vec<AgentValidationIssue>, field: &str, values: &[String]) {
    for (index, value) in values.iter().enumerate() {
        if value.trim().is_empty() {
            issues.push(issue(
                format!("{field}[{index}]"),
                "stable reference cannot be blank",
            ));
        }
    }
}

fn issue(field: impl Into<String>, message: impl Into<String>) -> AgentValidationIssue {
    AgentValidationIssue {
        field: field.into(),
        message: message.into(),
    }
}

fn typed_profiles(state: &RegistryState) -> IndexMap<AgentId, AgentProfile> {
    state
        .agents
        .iter()
        .map(|(id, snapshot)| (id.clone(), snapshot.profile.clone()))
        .collect()
}

pub(crate) fn new_agent_id() -> AgentId {
    format!("agent-{}", paths::new_id())
}

fn profile_from_input(id: AgentId, input: AgentMutationInput) -> AgentProfile {
    AgentProfile {
        schema_version: AGENT_SCHEMA_VERSION,
        id,
        name: input.name,
        description: input.description,
        avatar: input.avatar,
        model: input.model,
        personality: input.personality,
        permissions: input.permissions,
        skills: input.skills,
        tools: input.tools,
        loop_settings: input.loop_settings,
    }
}

fn unique_copy_name(source: &str, agents: &IndexMap<AgentId, Arc<AgentSnapshot>>) -> String {
    for number in 1.. {
        let candidate = if number == 1 {
            format!("{source} Copy")
        } else {
            format!("{source} Copy {number}")
        };
        if agents
            .values()
            .all(|agent| !agent.profile.name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
    }
    unreachable!()
}

fn snapshot_from_state(state: &RegistryState) -> AgentRegistrySnapshot {
    AgentRegistrySnapshot {
        agents: state
            .agents
            .values()
            .map(|snapshot| snapshot.as_ref().clone())
            .collect(),
        default_agent_id: state.index.default_agent_id.clone(),
        recovery: state.recovery.clone(),
        subagent_model: state.subagents.model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::agents::knowledge::AgentKnowledgeStore;
    use crate::agents::learning_queue::{LearningEventPayload, LearningQueue, ReviewEvent};
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

    use super::*;

    struct RegistryFixture {
        root: tempfile::TempDir,
        _db: tempfile::NamedTempFile,
        store: Arc<Store>,
    }

    impl RegistryFixture {
        async fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let db = tempfile::NamedTempFile::new().unwrap();
            let store = Arc::new(Store::open(db.path()).await.unwrap());
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
                        models_override: Some(vec![
                            "claude-opus-4-8".into(),
                            "claude-opus-4-5".into(),
                        ]),
                        ..Default::default()
                    },
                    created_at: 0,
                    updated_at: 0,
                },
            )
            .await
            .unwrap();
            let fixture = Self {
                root,
                _db: db,
                store,
            };
            fixture.write_raw(
                "agents/subagents.yaml",
                "schema_version: 1\nmodel: { name: anthropic/claude-opus-4-8, effort: high }\n",
            );
            fixture
        }

        fn config_root(&self) -> PathBuf {
            self.root.path().to_owned()
        }

        fn write_raw(&self, relative: &str, value: &str) {
            let path = self.root.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value).unwrap();
        }

        fn write_index(&self, ids: &[&str], default: &str) {
            self.write_raw(
                "agents/index.yaml",
                &format!(
                    "schema_version: 1\norder: [{}]\ndefault_agent_id: {default}\n",
                    ids.join(", ")
                ),
            );
        }

        fn write_profile(&self, id: &str, profile: String) {
            self.write_raw(&format!("agents/{id}/agent.yaml"), &profile);
        }

        fn write_single(&self, profile: String) {
            self.write_index(&["ryuzi"], "ryuzi");
            self.write_profile("ryuzi", profile);
        }
    }

    fn profile_yaml(id: &str, name: &str, model: &str, effort: Option<&str>) -> String {
        let effort = effort
            .map(|value| format!(", effort: {value}"))
            .unwrap_or_default();
        format!(
            "schema_version: 1\nid: {id}\nname: {name}\ndescription: Test agent.\navatar: {{ color: violet }}\nmodel: {{ name: {model}{effort} }}\npermissions: {{ mode: ask, rules: [] }}\nskills: {{ enabled: [] }}\ntools: {{ native: [read], plugins: [], apps: [] }}\nloop: {{ max_turns: 50, max_tool_rounds: 100 }}\n"
        )
    }

    #[tokio::test]
    async fn load_preserves_invalid_agents_and_rejects_duplicate_names_case_insensitively() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["one", "two"], "one");
        fixture.write_profile(
            "one",
            profile_yaml("one", "Reviewer", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "two",
            profile_yaml("two", " reviewer ", "unknown/missing", None),
        );
        let loaded = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let snapshot = loaded.snapshot().await;
        assert_eq!(snapshot.agents.len(), 2);
        assert!(snapshot.agents.iter().all(|agent| !agent.executable));
        assert!(snapshot.agents[0]
            .validation
            .iter()
            .any(|issue| issue.field == "name" && issue.message.contains("unique")));
        assert!(snapshot.agents[1]
            .validation
            .iter()
            .any(|issue| issue.field == "model.name"));
    }

    #[tokio::test]
    async fn concrete_effort_uses_plan_one_resolver_and_route_effort_is_impossible() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-5",
            Some("xhigh"),
        ));
        let loaded = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let agent = loaded.get("ryuzi").await.unwrap();
        assert!(!agent.executable);
        assert!(agent
            .validation
            .iter()
            .any(|issue| { issue.field == "model.effort" && issue.message.contains("xhigh") }));
    }

    #[tokio::test]
    async fn corrupt_index_recovers_profiles_by_directory_without_hiding_them() {
        let fixture = RegistryFixture::new().await;
        fixture.write_raw("agents/index.yaml", "not: [valid");
        fixture.write_profile(
            "visible",
            profile_yaml(
                "visible",
                "Visible",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        let loaded = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let snapshot = loaded.snapshot().await;
        assert_eq!(snapshot.agents[0].profile.id, "visible");
        assert!(snapshot
            .recovery
            .iter()
            .any(|notice| notice.code == "index-rebuilt"));
    }

    #[tokio::test]
    async fn parser_failure_with_identity_remains_visible() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["broken"], "broken");
        fixture.write_raw(
            "agents/broken/agent.yaml",
            "schema_version: 1\nid: broken\nname: Broken\nmodel: { route: free, effort: high }\n",
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let agent = registry.get("broken").await.unwrap();
        assert_eq!(agent.profile.name, "Broken");
        assert!(!agent.executable);
        assert!(agent
            .validation
            .iter()
            .any(|issue| issue.field == "profile"));
    }

    #[test]
    fn generated_agent_ids_satisfy_registry_validation() {
        let first = new_agent_id();
        let second = new_agent_id();
        assert!(validate_agent_id(&first).is_ok());
        assert!(validate_agent_id(&second).is_ok());
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn mutation_validates_the_complete_candidate() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["one", "two"], "one");
        fixture.write_profile(
            "one",
            profile_yaml("one", "One", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile("two", profile_yaml("two", "Two", "unknown/missing", None));
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let input = AgentMutationInput {
            name: "One Updated".into(),
            description: "Updated".into(),
            avatar: AgentAvatar {
                color: "blue".into(),
            },
            model: AgentModel::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: Some("high".into()),
            },
            personality: crate::agents::personality::AgentPersonality::default_profile(),
            permissions: AgentPermissions {
                mode: crate::PermMode::Default,
                rules: Vec::new(),
            },
            skills: Vec::new(),
            tools: AgentTools {
                native: vec!["read".into()],
                plugins: Vec::new(),
                apps: Vec::new(),
            },
            loop_settings: AgentLoop {
                max_turns: 10,
                max_tool_rounds: 20,
            },
        };
        let error = registry.update("one", input).await.unwrap_err();
        assert!(
            matches!(error, AgentRegistryError::Invalid(issues) if issues.iter().any(|issue| issue.field == "model.name"))
        );
    }

    #[tokio::test]
    async fn delete_unblocks_queue_when_apply_fence_acquisition_fails() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["ryuzi", "worker"], "ryuzi");
        fixture.write_profile(
            "ryuzi",
            profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "worker",
            profile_yaml(
                "worker",
                "Worker",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let knowledge = Arc::new(AgentKnowledgeStore::new(fixture.config_root()));
        let learning = Arc::new(LearningQueue::new(fixture.store.clone(), knowledge));
        registry
            .attach_learning_queue(Arc::downgrade(&learning))
            .unwrap();
        learning.fail_next_apply_fence_for_test();

        let error = registry.delete("worker").await.unwrap_err();

        assert!(error.to_string().contains("injected apply fence failure"));
        assert!(learning
            .enqueue(
                "worker",
                LearningEventPayload::Review(ReviewEvent {
                    title: "retry".into(),
                    description: "queue reopened".into(),
                    body: "fence failure rolled back".into(),
                    tags: Vec::new(),
                }),
            )
            .await
            .is_ok());
        assert!(fixture
            .root
            .path()
            .join("agents/worker/agent.yaml")
            .exists());
        assert!(registry.get("worker").await.is_ok());
    }

    #[tokio::test]
    async fn cancelling_delete_while_waiting_for_apply_fence_unblocks_queue() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["ryuzi", "worker"], "ryuzi");
        fixture.write_profile(
            "ryuzi",
            profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "worker",
            profile_yaml(
                "worker",
                "Worker",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        fixture.write_raw(
            "agents/worker/knowledge/marker.md",
            "must survive cancellation",
        );
        let registry = Arc::new(
            AgentRegistry::load(fixture.config_root(), fixture.store.clone())
                .await
                .unwrap(),
        );
        let knowledge = Arc::new(AgentKnowledgeStore::new(fixture.config_root()));
        let learning = Arc::new(LearningQueue::new(fixture.store.clone(), knowledge));
        registry
            .attach_learning_queue(Arc::downgrade(&learning))
            .unwrap();
        learning
            .enqueue(
                "worker",
                LearningEventPayload::Review(ReviewEvent {
                    title: "active".into(),
                    description: "active apply".into(),
                    body: "must finish before deletion".into(),
                    tags: Vec::new(),
                }),
            )
            .await
            .unwrap();
        let claimed = learning
            .claim_next("worker", "worker-1")
            .await
            .unwrap()
            .unwrap();
        let (apply_entered, release_apply) = learning.pause_next_apply_for_test();
        let apply_queue = Arc::clone(&learning);
        let apply = tokio::spawn(async move { apply_queue.apply_claimed(&claimed).await });
        apply_entered.notified().await;

        let delete_registry = Arc::clone(&registry);
        let delete = tokio::spawn(async move { delete_registry.delete("worker").await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if learning
                    .blocked_agents()
                    .await
                    .unwrap()
                    .contains(&"worker".to_owned())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("delete must durably block before waiting on the apply fence");
        delete.abort();
        assert!(delete.await.unwrap_err().is_cancelled());
        release_apply.notify_one();
        apply.await.unwrap().unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let enqueue = learning
                    .enqueue(
                        "worker",
                        LearningEventPayload::Review(ReviewEvent {
                            title: "after cancellation".into(),
                            description: "queue reopened".into(),
                            body: "delete did not commit".into(),
                            tags: Vec::new(),
                        }),
                    )
                    .await;
                if enqueue.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled delete must unblock without a restart");

        assert!(fixture
            .root
            .path()
            .join("agents/worker/agent.yaml")
            .exists());
        assert_eq!(
            std::fs::read_to_string(
                fixture
                    .root
                    .path()
                    .join("agents/worker/knowledge/marker.md")
            )
            .unwrap(),
            "must survive cancellation"
        );
        assert!(registry.get("worker").await.is_ok());
        assert!(!registry
            .snapshot()
            .await
            .recovery
            .iter()
            .any(|notice| notice.code == "learning_cleanup_pending"));
    }

    #[tokio::test]
    async fn delete_waits_for_active_apply_then_removes_bundle_and_keeps_queue_blocked() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["ryuzi", "worker"], "ryuzi");
        fixture.write_profile(
            "ryuzi",
            profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "worker",
            profile_yaml(
                "worker",
                "Worker",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        let registry = Arc::new(
            AgentRegistry::load(fixture.config_root(), fixture.store.clone())
                .await
                .unwrap(),
        );
        let knowledge = Arc::new(AgentKnowledgeStore::new(fixture.config_root()));
        let learning = Arc::new(LearningQueue::new(fixture.store.clone(), knowledge));
        registry
            .attach_learning_queue(Arc::downgrade(&learning))
            .unwrap();
        let event = learning
            .enqueue(
                "worker",
                LearningEventPayload::Review(ReviewEvent {
                    title: "active".into(),
                    description: "active apply".into(),
                    body: "must finish before deletion".into(),
                    tags: Vec::new(),
                }),
            )
            .await
            .unwrap();
        let claimed = learning
            .claim_next("worker", "worker-1")
            .await
            .unwrap()
            .unwrap();
        let (apply_entered, release_apply) = learning.pause_next_apply_for_test();
        let apply_queue = Arc::clone(&learning);
        let apply = tokio::spawn(async move { apply_queue.apply_claimed(&claimed).await });
        apply_entered.notified().await;

        let delete_registry = Arc::clone(&registry);
        let delete = tokio::spawn(async move { delete_registry.delete("worker").await });
        tokio::task::yield_now().await;
        assert!(!delete.is_finished(), "delete must drain the active apply");

        release_apply.notify_one();
        apply.await.unwrap().unwrap();
        delete.await.unwrap().unwrap();
        assert!(!fixture.root.path().join("agents/worker").exists());
        assert!(learning
            .enqueue(
                "worker",
                LearningEventPayload::Review(ReviewEvent {
                    title: "late".into(),
                    description: "late".into(),
                    body: "late".into(),
                    tags: Vec::new(),
                }),
            )
            .await
            .is_err());
        assert!(learning
            .claim_next("worker", "worker-2")
            .await
            .unwrap()
            .is_none());
        assert!(!fixture.root.path().join("agents/worker").exists());
        assert_ne!(event.event_id, "");
    }

    #[tokio::test]
    async fn committed_delete_reports_cleanup_warning_and_retry_converges() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["ryuzi", "worker"], "ryuzi");
        fixture.write_profile(
            "ryuzi",
            profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "worker",
            profile_yaml(
                "worker",
                "Worker",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let knowledge = Arc::new(AgentKnowledgeStore::new(fixture.config_root()));
        let learning = Arc::new(LearningQueue::new(fixture.store.clone(), knowledge));
        registry
            .attach_learning_queue(Arc::downgrade(&learning))
            .unwrap();
        learning
            .enqueue(
                "worker",
                LearningEventPayload::Review(ReviewEvent {
                    title: "pending".into(),
                    description: "pending".into(),
                    body: "pending".into(),
                    tags: Vec::new(),
                }),
            )
            .await
            .unwrap();
        learning.fail_next_discard_for_test();

        let outcome = registry.delete("worker").await.unwrap();
        assert!(!fixture.root.path().join("agents/worker").exists());
        assert!(outcome.recovery.iter().any(|notice| {
            notice.code == "learning_cleanup_pending" && notice.message.contains("worker")
        }));
        assert!(learning
            .blocked_agents()
            .await
            .unwrap()
            .contains(&"worker".into()));
        let rows_before_retry: i64 = fixture
            .store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM agent_learning_queue \
                     WHERE agent_id='worker' AND status IN ('pending','claimed')",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(rows_before_retry, 1);

        learning.discard_unconsumed("worker").await.unwrap();
        let rows_after_retry: i64 = fixture
            .store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM agent_learning_queue \
                     WHERE agent_id='worker' AND status IN ('pending','claimed')",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(rows_after_retry, 0);
        assert!(learning
            .blocked_agents()
            .await
            .unwrap()
            .contains(&"worker".into()));
    }

    #[tokio::test]
    async fn delete_blocks_queue_discards_rows_and_rolls_back_block_on_file_failure() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["ryuzi", "worker"], "ryuzi");
        fixture.write_profile(
            "ryuzi",
            profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high")),
        );
        fixture.write_profile(
            "worker",
            profile_yaml(
                "worker",
                "Worker",
                "anthropic/claude-opus-4-8",
                Some("high"),
            ),
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let knowledge = Arc::new(AgentKnowledgeStore::new(fixture.config_root()));
        let learning = Arc::new(LearningQueue::new(fixture.store.clone(), knowledge));
        registry
            .attach_learning_queue(Arc::downgrade(&learning))
            .unwrap();
        let payload = |title: &str| {
            LearningEventPayload::Review(ReviewEvent {
                title: title.into(),
                description: format!("{title} description"),
                body: format!("{title} body"),
                tags: Vec::new(),
            })
        };
        learning
            .enqueue("worker", payload("pending"))
            .await
            .unwrap();

        registry
            .failpoint
            .store(1, std::sync::atomic::Ordering::SeqCst);
        assert!(registry.delete("worker").await.is_err());
        assert!(learning
            .enqueue("worker", payload("retry allowed"))
            .await
            .is_ok());
        assert!(fixture.root.path().join("agents/worker").exists());

        registry
            .failpoint
            .store(0, std::sync::atomic::Ordering::SeqCst);
        registry.delete("worker").await.unwrap();
        assert!(learning
            .enqueue("worker", payload("blocked forever"))
            .await
            .is_err());
        assert!(!fixture.root.path().join("agents/worker").exists());
        assert!(!learning
            .pending_agents()
            .await
            .unwrap()
            .contains(&"worker".into()));
    }

    #[tokio::test]
    async fn failure_after_index_replace_restores_disk_and_cache() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        registry
            .failpoint
            .store(2, std::sync::atomic::Ordering::SeqCst);
        assert!(registry
            .update("ryuzi", mutation_input("Not Active"))
            .await
            .is_err());
        assert_eq!(registry.get("ryuzi").await.unwrap().profile.name, "Ryuzi");
        let raw =
            std::fs::read_to_string(fixture.root.path().join("agents/ryuzi/agent.yaml")).unwrap();
        assert!(raw.contains("name: Ryuzi"));
    }

    #[tokio::test]
    async fn update_commits_disk_before_publishing_cache_and_preserves_extensions() {
        let fixture = RegistryFixture::new().await;
        fixture.write_raw(
            "agents/index.yaml",
            "schema_version: 1\norder: [ryuzi]\ndefault_agent_id: ryuzi\nx_sync: manual\n",
        );
        fixture.write_raw(
            "agents/ryuzi/agent.yaml",
            &format!(
                "{}x_vendor:\n  retained: true\n",
                profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high"))
            ),
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let mut input = mutation_input("Ryuzi Prime");
        input.description = "Committed description".into();
        let updated = registry.update("ryuzi", input).await.unwrap();
        assert_eq!(updated.profile.name, "Ryuzi Prime");
        let raw =
            std::fs::read_to_string(fixture.root.path().join("agents/ryuzi/agent.yaml")).unwrap();
        let index = std::fs::read_to_string(fixture.root.path().join("agents/index.yaml")).unwrap();
        assert!(raw.contains("x_vendor:"));
        assert!(index.contains("x_sync: manual"));
    }

    #[tokio::test]
    async fn all_mutations_commit_complete_images_and_preserve_extensions() {
        let fixture = RegistryFixture::new().await;
        fixture.write_raw(
            "agents/index.yaml",
            "schema_version: 1\norder: [ryuzi]\ndefault_agent_id: ryuzi\nx_sync: manual\n",
        );
        fixture.write_raw(
            "agents/subagents.yaml",
            "schema_version: 1\nmodel: { name: anthropic/claude-opus-4-8, effort: high }\nx_vendor: retained\n",
        );
        fixture.write_raw(
            "agents/ryuzi/agent.yaml",
            &format!(
                "{}x_vendor:\n  retained: true\n",
                profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high"))
            ),
        );
        fixture.write_raw("agents/ryuzi/knowledge/note.md", "keep me");
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();

        let created = registry.create(mutation_input("Worker")).await.unwrap();
        let duplicate = registry.duplicate("ryuzi").await.unwrap();
        assert_eq!(duplicate.profile.name, "Ryuzi Copy");
        registry.set_default(&created.profile.id).await.unwrap();
        registry
            .set_subagent_model(AgentModel::Concrete {
                name: "anthropic/claude-opus-4-5".into(),
                effort: Some("high".into()),
            })
            .await
            .unwrap();
        registry.delete("ryuzi").await.unwrap();

        let agents_root = fixture.root.path().join("agents");
        assert!(!agents_root.join("ryuzi").exists());
        assert!(agents_root
            .join(&created.profile.id)
            .join("agent.yaml")
            .exists());
        assert!(agents_root
            .join(&duplicate.profile.id)
            .join("agent.yaml")
            .exists());
        assert!(std::fs::read_to_string(agents_root.join("index.yaml"))
            .unwrap()
            .contains("x_sync: manual"));
        assert!(std::fs::read_to_string(agents_root.join("subagents.yaml"))
            .unwrap()
            .contains("x_vendor: retained"));
    }

    #[tokio::test]
    async fn concurrent_creates_serialize_and_keep_unique_names() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let registry = Arc::new(
            AgentRegistry::load(fixture.config_root(), fixture.store.clone())
                .await
                .unwrap(),
        );

        let (first, second) = tokio::join!(
            registry.create(mutation_input("Worker")),
            registry.create(mutation_input("worker"))
        );

        assert_eq!(
            [first.is_ok(), second.is_ok()]
                .into_iter()
                .filter(|ok| *ok)
                .count(),
            1
        );
        assert_eq!(registry.snapshot().await.agents.len(), 2);
    }

    #[tokio::test]
    async fn stale_registry_rejects_mutation_instead_of_overwriting() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let first = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let second = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();

        first.create(mutation_input("Worker One")).await.unwrap();
        let error = second
            .create(mutation_input("Worker Two"))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("changed on disk"));
        let reloaded = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let names = reloaded
            .snapshot()
            .await
            .agents
            .into_iter()
            .map(|agent| agent.profile.name)
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Worker One".to_owned()));
        assert!(!names.contains(&"Worker Two".to_owned()));
    }

    #[tokio::test]
    async fn foreign_commit_in_the_post_commit_window_marks_the_cache_stale() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        // Failpoint 3 mutates the on-disk index immediately after the
        // transaction commits, simulating a foreign process that grabs the
        // registry lock and commits before this instance refreshes its cache.
        registry
            .failpoint
            .store(3, std::sync::atomic::Ordering::SeqCst);
        registry.create(mutation_input("Worker One")).await.unwrap();
        registry
            .failpoint
            .store(0, std::sync::atomic::Ordering::SeqCst);

        // The cached generation must describe this instance's own committed
        // image, so the next mutation is rejected instead of overwriting the
        // foreign commit with stale in-memory profiles.
        let error = registry
            .create(mutation_input("Worker Two"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed on disk"));
        let index = std::fs::read_to_string(fixture.root.path().join("agents/index.yaml")).unwrap();
        assert!(index.contains("# foreign commit"));
    }

    #[tokio::test]
    async fn duplicate_preserves_source_profile_extensions() {
        let fixture = RegistryFixture::new().await;
        fixture.write_raw(
            "agents/index.yaml",
            "schema_version: 1\norder: [ryuzi]\ndefault_agent_id: ryuzi\n",
        );
        fixture.write_raw(
            "agents/ryuzi/agent.yaml",
            &format!(
                "{}x_vendor:\n  retained: true\n",
                profile_yaml("ryuzi", "Ryuzi", "anthropic/claude-opus-4-8", Some("high"))
            ),
        );
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();

        let duplicate = registry.duplicate("ryuzi").await.unwrap();
        let raw = std::fs::read_to_string(
            fixture
                .root
                .path()
                .join("agents")
                .join(&duplicate.profile.id)
                .join("agent.yaml"),
        )
        .unwrap();
        assert!(raw.contains("x_vendor:"));
        assert!(raw.contains("retained: true"));
    }

    #[tokio::test]
    async fn deleting_default_selects_the_next_ordered_survivor() {
        let fixture = RegistryFixture::new().await;
        fixture.write_index(&["one", "two", "three"], "two");
        for (id, name) in [("one", "One"), ("two", "Two"), ("three", "Three")] {
            fixture.write_profile(
                id,
                profile_yaml(id, name, "anthropic/claude-opus-4-8", Some("high")),
            );
        }
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();

        let snapshot = registry.delete("two").await.unwrap();

        assert_eq!(snapshot.default_agent_id, "three");
    }

    #[tokio::test]
    async fn delete_final_agent_fails_without_creating_a_journal() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert!(matches!(
            registry.delete("ryuzi").await,
            Err(AgentRegistryError::LastAgent)
        ));
        assert!(!fixture.root.path().join("agents/.transactions").exists());
    }

    fn mutation_input(name: &str) -> AgentMutationInput {
        AgentMutationInput {
            name: name.into(),
            description: "Updated".into(),
            avatar: AgentAvatar {
                color: "blue".into(),
            },
            model: AgentModel::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: Some("high".into()),
            },
            personality: crate::agents::personality::AgentPersonality::default_profile(),
            permissions: AgentPermissions {
                mode: crate::PermMode::Default,
                rules: Vec::new(),
            },
            skills: Vec::new(),
            tools: AgentTools {
                native: vec!["read".into()],
                plugins: Vec::new(),
                apps: Vec::new(),
            },
            loop_settings: AgentLoop {
                max_turns: 10,
                max_tool_rounds: 20,
            },
        }
    }

    #[tokio::test]
    async fn candidate_mutation_commits_after_validation() {
        let fixture = RegistryFixture::new().await;
        fixture.write_single(profile_yaml(
            "ryuzi",
            "Ryuzi",
            "anthropic/claude-opus-4-8",
            Some("high"),
        ));
        let registry = AgentRegistry::load(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let before = registry.snapshot().await;
        let input = AgentMutationInput {
            name: "Ryuzi Updated".into(),
            description: "Updated".into(),
            avatar: AgentAvatar {
                color: "blue".into(),
            },
            model: AgentModel::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: Some("high".into()),
            },
            personality: crate::agents::personality::AgentPersonality::default_profile(),
            permissions: AgentPermissions {
                mode: crate::PermMode::Default,
                rules: Vec::new(),
            },
            skills: Vec::new(),
            tools: AgentTools {
                native: vec!["read".into()],
                plugins: Vec::new(),
                apps: Vec::new(),
            },
            loop_settings: AgentLoop {
                max_turns: 10,
                max_tool_rounds: 20,
            },
        };
        let updated = registry.update("ryuzi", input).await.unwrap();
        assert_eq!(updated.profile.name, "Ryuzi Updated");
        assert_ne!(registry.snapshot().await, before);
        assert_eq!(registry.config_root, fixture.config_root());
    }
}
