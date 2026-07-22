//! Startup bootstrap for the YAML agent registry. Decides between fresh
//! install, first upgrade from the legacy single-agent settings, reset, and
//! recovery; performs journaled destructive cleanup limited to legacy agent
//! data (the `agent_model`/`agent_perm_mode` settings keys and the legacy
//! `<config>/memory` directory); creates the built-in Ryuzi templates; and
//! sets the retry-safe schema marker only after the files are valid.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use indexmap::IndexMap;

use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget};
use crate::paths;
use crate::store::Store;

use super::knowledge::{ensure_bundle_at, AgentKnowledgeStore};
use super::learning_queue::LearningQueue;
use super::registry::{discover_agent_directories, new_agent_id, AgentRegistry};
use super::transaction::{atomic_write, recover_transactions, AgentTransaction};
use super::types::*;
use super::yaml::{
    parse_agent_index_document, render_agent_index, render_agent_index_document,
    render_agent_profile, render_subagent_config,
};

/// Settings key marking that the YAML agent registry schema has been
/// bootstrapped. Set with `set_setting_raw` only after the default files are
/// committed and loadable, so a crash before it retries idempotently.
pub const AGENT_PERSISTENCE_MARKER: &str = "agent_persistence_schema";

/// The only schema value this build understands.
const AGENT_PERSISTENCE_SCHEMA: &str = "1";

/// Settings marker: the one-time auto-seed of the MiMo/OpenCode free-tier
/// connections has run. Kept separate from the agent-persistence marker so a
/// user who deletes the seeded rows is not re-seeded on the next boot.
const FREE_PROVIDERS_SEEDED_MARKER: &str = "free_providers_seeded_v1";

/// Legacy settings-KV keys that once held the single native agent's default
/// model / permission mode. The runtime that read them is gone; only
/// [`legacy_agent_data_exists`] still probes these rows to decide whether a
/// first-upgrade cleanup is due. A fresh baseline database never carries
/// them; only a pre-existing install migrated from an older build could.
const LEGACY_AGENT_MODEL_KEY: &str = "agent_model";
const LEGACY_AGENT_PERM_MODE_KEY: &str = "agent_perm_mode";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapReason {
    Existing,
    FreshInstall,
    FirstUpgrade,
    /// Bootstrap filesystem committed, but legacy SQL cleanup/final marker did
    /// not. This phase never repeats destructive filesystem work.
    FirstUpgradeCleanup,
    Reset,
    Recovery,
}

pub struct AgentBootstrap {
    pub registry: Arc<AgentRegistry>,
    pub reason: BootstrapReason,
}

#[derive(Clone)]
pub struct AgentPersistence {
    pub registry: Arc<AgentRegistry>,
    pub knowledge: Arc<AgentKnowledgeStore>,
    pub learning: Arc<LearningQueue>,
    pub reason: BootstrapReason,
}

#[derive(Clone)]
pub struct AgentPersistenceHandles {
    pub registry: Arc<AgentRegistry>,
    pub knowledge: Arc<AgentKnowledgeStore>,
    pub learning: Arc<LearningQueue>,
}

impl AgentPersistence {
    /// Builds an isolated persistence graph for tests and short-lived embedding hosts.
    /// The temporary root is intentionally outside the user's configuration directory.
    pub async fn temporary(store: Arc<Store>) -> anyhow::Result<Self> {
        initialize_agent_persistence(tempfile::tempdir()?.keep(), store).await
    }

    pub fn handles(&self) -> AgentPersistenceHandles {
        AgentPersistenceHandles {
            registry: self.registry.clone(),
            knowledge: self.knowledge.clone(),
            learning: self.learning.clone(),
        }
    }
}

pub async fn initialize_agent_persistence(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentPersistence> {
    let bootstrap = initialize_agent_registry(config_root.clone(), store.clone()).await?;
    let knowledge = Arc::new(AgentKnowledgeStore::new(config_root));
    for agent in bootstrap.registry.snapshot().await.agents {
        knowledge
            .recover_agent_bundle(&agent.profile.id)
            .await
            .with_context(|| {
                format!(
                    "failed to recover knowledge bundle for agent `{}`",
                    agent.profile.id
                )
            })?;
    }
    let learning = Arc::new(LearningQueue::new(store, knowledge.clone()));
    bootstrap
        .registry
        .attach_learning_queue(Arc::downgrade(&learning))?;

    let active: std::collections::HashSet<_> = bootstrap
        .registry
        .snapshot()
        .await
        .agents
        .into_iter()
        .map(|agent| agent.profile.id)
        .collect();
    for agent_id in &active {
        learning.unblock(agent_id).await?;
    }
    for agent_id in learning.blocked_agents().await? {
        if !active.contains(&agent_id) {
            learning.discard_unconsumed(&agent_id).await?;
        }
    }
    learning.reclaim_stale(paths::now_ms()).await?;

    Ok(AgentPersistence {
        registry: bootstrap.registry,
        knowledge,
        learning,
        reason: bootstrap.reason,
    })
}

/// The built-in Ryuzi profile template. YAML permission mode `ask` is the
/// runtime `PermMode::Default`; empty native tools mean runtime defaults.
pub fn default_ryuzi_profile(agent_id: String) -> AgentProfile {
    AgentProfile {
        schema_version: AGENT_SCHEMA_VERSION,
        id: agent_id,
        name: "Ryuzi".into(),
        description: "General-purpose coding and operations agent.".into(),
        avatar: AgentAvatar {
            color: "blue".into(),
        },
        model: AgentModel::Route {
            route: "free".into(),
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
    }
}

/// The built-in shared subagent configuration template.
pub fn default_subagent_config() -> SubagentConfig {
    SubagentConfig {
        schema_version: AGENT_SCHEMA_VERSION,
        model: AgentModel::Route {
            route: "free".into(),
        },
    }
}

/// Materialize a default named route with one target when the user first
/// chooses an agent or subagent model. The target must be a selectable native
/// model, which guarantees its provider connection is enabled, credentialed,
/// and runnable by the native harness.
pub(crate) async fn ensure_default_route(store: &Store, name: &str) -> anyhow::Result<()> {
    let routes = routes::list_model_routes(store).await?;
    if routes
        .iter()
        .any(|route| route.name.eq_ignore_ascii_case(name))
    {
        return Ok(());
    }

    let Some(model) = crate::llm_router::client::selectable_native_models(store)
        .await?
        .into_iter()
        .find(|model| {
            matches!(
                model.kind,
                crate::llm_router::model_effort::SelectableModelKind::Concrete
            )
        })
    else {
        return Ok(());
    };
    let Some((provider, model)) = model.request_value.split_once('/') else {
        return Ok(());
    };

    routes::save_model_route_if_name_absent(
        store,
        ModelRouteInfo {
            id: String::new(),
            name: name.into(),
            enabled: true,
            strategy: ModelRouteStrategy::Fallback,
            targets: vec![ModelRouteTarget {
                provider: provider.into(),
                model: model.into(),
                effort: None,
            }],
            created_at: 0,
            updated_at: 0,
        },
    )
    .await?;
    Ok(())
}

pub(crate) async fn ensure_default_routes(store: &Store) -> anyhow::Result<()> {
    ensure_default_route(store, "free").await?;
    Ok(())
}

/// Idempotently create enabled, credential-less `free` connections for the
/// MiMo and OpenCode free tiers so a fresh install has working models with no
/// "Add account" click. Guarded by [`FREE_PROVIDERS_SEEDED_MARKER`] so deleting
/// the rows is respected.
pub(crate) async fn ensure_free_providers_seeded(store: &Store) -> anyhow::Result<()> {
    if store
        .get_setting_raw(FREE_PROVIDERS_SEEDED_MARKER)
        .await?
        .is_some()
    {
        return Ok(());
    }
    for (provider, label) in [
        ("mimo-free", "MiMo (free)"),
        ("opencode-free", "OpenCode (free)"),
    ] {
        let now = crate::paths::now_ms();
        connections::add_connection(
            store,
            ConnectionRow {
                id: crate::paths::new_id(),
                provider: provider.into(),
                auth_type: "free".into(),
                label: label.into(),
                priority: 0,
                enabled: true,
                data: ConnectionData::default(),
                created_at: now,
                updated_at: now,
            },
        )
        .await?;
    }
    store
        .set_setting_raw(FREE_PROVIDERS_SEEDED_MARKER, "1")
        .await?;
    Ok(())
}

pub async fn initialize_agent_registry(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentBootstrap> {
    let marker = store.get_setting(AGENT_PERSISTENCE_MARKER).await?;
    let agents_exists = paths::agents_dir_in(&config_root)
        .join("index.yaml")
        .exists();
    let reason = match (marker.as_deref(), agents_exists) {
        (Some("1"), true) => BootstrapReason::Existing,
        (Some("1"), false) => BootstrapReason::Recovery,
        (None, false) if legacy_agent_data_exists(&config_root, &store).await? => {
            BootstrapReason::FirstUpgrade
        }
        (None, false) => BootstrapReason::FreshInstall,
        (None, true) if legacy_agent_data_exists(&config_root, &store).await? => {
            BootstrapReason::FirstUpgradeCleanup
        }
        (None, true) => BootstrapReason::Existing,
        (Some(other), _) => anyhow::bail!("unsupported agent persistence schema `{other}`"),
    };
    match reason {
        BootstrapReason::FreshInstall | BootstrapReason::FirstUpgrade => {
            bootstrap_defaults(config_root, store, reason).await
        }
        BootstrapReason::FirstUpgradeCleanup => {
            finish_first_upgrade_cleanup(config_root, store).await
        }
        BootstrapReason::Existing | BootstrapReason::Recovery => {
            load_existing(config_root, store, reason).await
        }
        BootstrapReason::Reset => unreachable!("reset only enters through reset_agent_registry"),
    }
}

/// Replaces the whole agent registry with the built-in defaults. Destroys
/// agent data only: every agent directory, the registry index/subagent files,
/// the legacy `<config>/memory` directory, and the two legacy settings keys.
/// Projects, providers, sessions, and every other settings row survive.
pub async fn reset_agent_registry(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentBootstrap> {
    bootstrap_defaults(config_root, store, BootstrapReason::Reset).await
}

async fn legacy_agent_data_exists(config_root: &Path, store: &Store) -> anyhow::Result<bool> {
    if store.get_setting(LEGACY_AGENT_MODEL_KEY).await?.is_some()
        || store
            .get_setting(LEGACY_AGENT_PERM_MODE_KEY)
            .await?
            .is_some()
    {
        return Ok(true);
    }
    Ok(config_root.join("memory").exists())
}

async fn bootstrap_defaults(
    config_root: PathBuf,
    store: Arc<Store>,
    reason: BootstrapReason,
) -> anyhow::Result<AgentBootstrap> {
    // Settle pending journals before staging destructive cleanup so a stale
    // committed journal cannot resurrect deleted directories afterwards.
    recover_transactions(&config_root)?;

    let agents_root = paths::agents_dir_in(&config_root);
    let existing = discover_agent_directories(&agents_root)?;
    // A surviving or invalid user-owned `ryuzi` directory must never be
    // overwritten by the template; fall back to a unique generated id.
    let agent_id = if existing.iter().any(|id| id == "ryuzi") {
        new_agent_id()
    } else {
        "ryuzi".to_owned()
    };
    let destructive = matches!(
        reason,
        BootstrapReason::FirstUpgrade | BootstrapReason::Reset
    );
    let deleted_agent_ids = if destructive { existing } else { Vec::new() };

    let profile = default_ryuzi_profile(agent_id.clone());
    let index = AgentIndex {
        schema_version: AGENT_SCHEMA_VERSION,
        order: vec![agent_id.clone()],
        default_agent_id: agent_id.clone(),
        extensions: IndexMap::new(),
    };
    let image = RegistryDiskImage {
        index_yaml: render_agent_index(&index)?,
        subagents_yaml: render_subagent_config(&default_subagent_config())?,
        agents: IndexMap::from([(agent_id, render_agent_profile(&profile)?)]),
        deleted_agent_ids,
    };
    let transaction =
        AgentTransaction::prepare_with_legacy_cleanup(&config_root, &image, None, destructive)?;
    #[cfg(test)]
    let transaction = transaction.with_failpoint(testing::current(&config_root));
    transaction.commit()?;

    let registry = Arc::new(AgentRegistry::load(config_root.clone(), store.clone()).await?);
    ensure_knowledge_bundles(&config_root, &registry).await?;

    if destructive {
        store.delete_legacy_agent_settings().await?;
    }
    // Marker last: a crash anywhere above leaves either the old state (the
    // transaction rolled back) or valid new files whose remaining SQL cleanup
    // the next startup completes without replacing the filesystem graph.
    store
        .set_setting_raw(AGENT_PERSISTENCE_MARKER, AGENT_PERSISTENCE_SCHEMA)
        .await?;
    Ok(AgentBootstrap { registry, reason })
}

async fn finish_first_upgrade_cleanup(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentBootstrap> {
    // The registry and knowledge defaults committed on the prior attempt.
    // Adopt them exactly as they are and perform only the outstanding SQL
    // cleanup before writing the marker last.
    let registry = Arc::new(AgentRegistry::load(config_root, store.clone()).await?);
    store.delete_legacy_agent_settings().await?;
    store
        .set_setting_raw(AGENT_PERSISTENCE_MARKER, AGENT_PERSISTENCE_SCHEMA)
        .await?;
    Ok(AgentBootstrap {
        registry,
        reason: BootstrapReason::FirstUpgradeCleanup,
    })
}

async fn load_existing(
    config_root: PathBuf,
    store: Arc<Store>,
    reason: BootstrapReason,
) -> anyhow::Result<AgentBootstrap> {
    // A wiped registry may be missing subagents.yaml entirely, which the
    // loader treats as fatal. Restoring the default template is idempotent,
    // so it happens directly rather than through a journaled transaction.
    let subagents_path = paths::agents_dir_in(&config_root).join("subagents.yaml");
    if !subagents_path.exists() {
        atomic_write(
            &subagents_path,
            render_subagent_config(&default_subagent_config())?.as_bytes(),
        )?;
    }
    let registry = AgentRegistry::load(config_root.clone(), store.clone()).await?;
    let snapshot = registry.snapshot().await;
    let registry = if snapshot.agents.iter().any(|agent| agent.executable) {
        Arc::new(registry)
    } else {
        append_default_ryuzi(&config_root, &store, &snapshot).await?
    };
    ensure_knowledge_bundles(&config_root, &registry).await?;
    store
        .set_setting_raw(AGENT_PERSISTENCE_MARKER, AGENT_PERSISTENCE_SCHEMA)
        .await?;
    Ok(AgentBootstrap { registry, reason })
}

/// Creates (idempotently) every agent's knowledge bundle skeleton: the
/// concept directories plus empty generated `index.md`/`log.md` files.
/// Existing bundle content is never modified.
async fn ensure_knowledge_bundles(
    config_root: &Path,
    registry: &AgentRegistry,
) -> anyhow::Result<()> {
    let knowledge = AgentKnowledgeStore::new(config_root.to_owned());
    for agent in registry.snapshot().await.agents {
        knowledge
            .recover_agent_bundle(&agent.profile.id)
            .await
            .with_context(|| {
                format!(
                    "failed to recover knowledge bundle for agent `{}`",
                    agent.profile.id
                )
            })?;
        let bundle = paths::agent_knowledge_dir_in(config_root, &agent.profile.id);
        ensure_bundle_at(&bundle).with_context(|| {
            format!(
                "failed to create knowledge bundle for agent `{}`",
                agent.profile.id
            )
        })?;
    }
    Ok(())
}

/// Appends a new UUID-identified Ryuzi built from the template without
/// touching any existing directory or name, then reloads so the published
/// state reflects the committed files.
async fn append_default_ryuzi(
    config_root: &Path,
    store: &Arc<Store>,
    snapshot: &AgentRegistrySnapshot,
) -> anyhow::Result<Arc<AgentRegistry>> {
    let agent_id = new_agent_id();
    let mut profile = default_ryuzi_profile(agent_id.clone());
    profile.name = unique_default_name(snapshot);
    let mut order: Vec<AgentId> = snapshot
        .agents
        .iter()
        .map(|agent| agent.profile.id.clone())
        .collect();
    order.push(agent_id.clone());
    let index = AgentIndex {
        schema_version: AGENT_SCHEMA_VERSION,
        order,
        // Every existing agent failed validation (that is why this path
        // runs), so the fresh default becomes the registry default.
        default_agent_id: agent_id.clone(),
        extensions: IndexMap::new(),
    };
    let index_path = paths::agents_dir_in(config_root).join("index.yaml");
    let index_yaml = match std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|raw| parse_agent_index_document(&raw).ok())
    {
        Some(mut document) => {
            document.merge_typed(index);
            render_agent_index_document(&document)?
        }
        None => render_agent_index(&index)?,
    };
    let subagents_path = paths::agents_dir_in(config_root).join("subagents.yaml");
    let subagents_yaml = std::fs::read_to_string(&subagents_path)
        .with_context(|| format!("failed to read {}", subagents_path.display()))?;
    let image = RegistryDiskImage {
        index_yaml,
        subagents_yaml,
        agents: IndexMap::from([(agent_id, render_agent_profile(&profile)?)]),
        deleted_agent_ids: Vec::new(),
    };
    let transaction = AgentTransaction::prepare(config_root, &image, None)?;
    #[cfg(test)]
    let transaction = transaction.with_failpoint(testing::current(config_root));
    transaction.commit()?;

    let reloaded = AgentRegistry::load(config_root.to_owned(), store.clone()).await?;
    let mut notices = snapshot.recovery.clone();
    notices.push(AgentRecoveryNotice {
        code: "default-created".into(),
        message: format!(
            "Created default agent `{}` because no executable agent existed.",
            profile.name
        ),
    });
    reloaded.append_recovery_notices(notices).await;
    Ok(Arc::new(reloaded))
}

/// The template name, uniquified against existing (possibly invalid) agents
/// so the appended default never collides case-insensitively.
fn unique_default_name(snapshot: &AgentRegistrySnapshot) -> String {
    let taken: Vec<String> = snapshot
        .agents
        .iter()
        .map(|agent| agent.profile.name.trim().to_ascii_lowercase())
        .collect();
    for number in 1u32.. {
        let candidate = if number == 1 {
            "Ryuzi".to_owned()
        } else {
            format!("Ryuzi {number}")
        };
        if !taken.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    unreachable!()
}

/// Test-only failpoint plumbing: bootstrap creates its transactions
/// internally, so tests register a per-config-root transaction failpoint
/// that `bootstrap_defaults`/`append_default_ryuzi` pick up.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    use super::super::transaction::TransactionFailpoint;

    fn map() -> &'static Mutex<HashMap<PathBuf, TransactionFailpoint>> {
        static MAP: OnceLock<Mutex<HashMap<PathBuf, TransactionFailpoint>>> = OnceLock::new();
        MAP.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) struct BootstrapFailpoint {
        root: PathBuf,
    }

    impl BootstrapFailpoint {
        pub(crate) fn for_root(root: &Path) -> Self {
            Self {
                root: root.to_owned(),
            }
        }

        pub(crate) fn set(&self, failpoint: TransactionFailpoint) {
            map().lock().unwrap().insert(self.root.clone(), failpoint);
        }

        pub(crate) fn clear(&self) {
            map().lock().unwrap().remove(&self.root);
        }
    }

    impl Drop for BootstrapFailpoint {
        fn drop(&mut self) {
            self.clear();
        }
    }

    pub(super) fn current(root: &Path) -> TransactionFailpoint {
        map()
            .lock()
            .unwrap()
            .get(root)
            .copied()
            .unwrap_or(TransactionFailpoint::None)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::domain::{PermMode, Project, Session, SessionKind, SessionStatus};
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget};
    use crate::store::Store;

    use super::super::transaction::TransactionFailpoint;
    use super::testing::BootstrapFailpoint;
    use super::*;

    struct BootstrapFixture {
        root: tempfile::TempDir,
        _db: tempfile::NamedTempFile,
        store: Arc<Store>,
        failpoint: BootstrapFailpoint,
    }

    impl BootstrapFixture {
        async fn base() -> Self {
            let root = tempfile::tempdir().unwrap();
            let db = tempfile::NamedTempFile::new().unwrap();
            let store = Arc::new(Store::open(db.path()).await.unwrap());
            routes::save_model_route(
                &store,
                ModelRouteInfo {
                    id: String::new(),
                    name: "free".into(),
                    enabled: true,
                    strategy: ModelRouteStrategy::Fallback,
                    targets: vec![ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-opus-4-8".into(),
                        effort: None,
                    }],
                    created_at: 0,
                    updated_at: 0,
                },
            )
            .await
            .unwrap();
            let failpoint = BootstrapFailpoint::for_root(root.path());
            Self {
                root,
                _db: db,
                store,
                failpoint,
            }
        }

        async fn fresh() -> Self {
            Self::base().await
        }

        /// A legacy install: no `agents/` registry and no schema marker. The
        /// legacy settings keys and memory directory are written by tests.
        async fn legacy() -> Self {
            Self::base().await
        }

        /// Marker present, `agents/index.yaml` missing, and a user-owned
        /// `agents/ryuzi` directory whose profile validates as non-executable.
        async fn invalid_ryuzi_directory() -> Self {
            let fixture = Self::base().await;
            fixture
                .store
                .set_setting_raw(AGENT_PERSISTENCE_MARKER, "1")
                .await
                .unwrap();
            fixture.write_raw(
                "agents/ryuzi/agent.yaml",
                "schema_version: 1\nid: ryuzi\nname: Legacy Ryuzi\ndescription: Broken agent.\navatar: { color: violet }\nmodel: { route: nonexistent }\npermissions: { mode: ask, rules: [] }\nskills: { enabled: [] }\ntools: { native: [], plugins: [], apps: [] }\nloop: { max_turns: 50, max_tool_rounds: 100 }\n",
            );
            fixture
        }

        async fn initialized_with_two_agents() -> Self {
            let fixture = Self::base().await;
            fixture
                .store
                .set_setting_raw(AGENT_PERSISTENCE_MARKER, "1")
                .await
                .unwrap();
            fixture.write_raw(
                "agents/index.yaml",
                "schema_version: 1\norder: [ryuzi, helper]\ndefault_agent_id: ryuzi\n",
            );
            fixture.write_raw("agents/ryuzi/agent.yaml", &profile_yaml("ryuzi", "Ryuzi"));
            fixture.write_raw(
                "agents/helper/agent.yaml",
                &profile_yaml("helper", "Helper"),
            );
            fixture.write_raw(
                "agents/subagents.yaml",
                "schema_version: 1\nmodel: { route: free }\n",
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

        fn write_legacy_memory(&self, name: &str, content: &str) {
            let dir = self.root.path().join("memory");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), content).unwrap();
        }

        async fn insert_project_provider_and_session(&self) {
            self.store
                .insert_project(Project {
                    project_id: "proj-1".into(),
                    name: "Project".into(),
                    workdir: "/tmp/proj".into(),
                    source: None,
                    model: None,
                    effort: None,
                    perm_mode: PermMode::Default,
                    created_at: Some(1),
                    is_git: false,
                })
                .await
                .unwrap();
            connections::add_connection(
                &self.store,
                ConnectionRow {
                    id: "anthropic-live".into(),
                    provider: "anthropic".into(),
                    auth_type: "api_key".into(),
                    label: "Anthropic".into(),
                    priority: 0,
                    enabled: true,
                    data: ConnectionData::default(),
                    created_at: 0,
                    updated_at: 0,
                },
            )
            .await
            .unwrap();
            self.store
                .insert_session(Session {
                    session_pk: "sess-1".into(),
                    primary_agent_id: None,
                    primary_agent_snapshot: None,
                    project_id: Some("proj-1".into()),
                    agent_session_id: None,
                    worktree_path: None,
                    branch: None,
                    title: None,
                    status: SessionStatus::Idle,
                    perm_mode: PermMode::Default,
                    started_by: None,
                    created_at: Some(1),
                    last_active: Some(1),
                    resume_attempts: 0,
                    branch_owned: false,
                    kind: SessionKind::Project,
                    speaker: None,
                    agent: None,
                    parent_session_pk: None,
                    archived_at: None,
                })
                .await
                .unwrap();
        }

        async fn assert_project_provider_and_session_survive(&self) {
            assert!(self.store.get_project("proj-1").await.unwrap().is_some());
            assert!(connections::get_connection(&self.store, "anthropic-live")
                .await
                .unwrap()
                .is_some());
            assert!(self.store.get_session("sess-1").await.unwrap().is_some());
        }
    }

    fn profile_yaml(id: &str, name: &str) -> String {
        format!(
            "schema_version: 1\nid: {id}\nname: {name}\ndescription: Test agent.\navatar: {{ color: violet }}\nmodel: {{ route: free }}\npermissions: {{ mode: ask, rules: [] }}\nskills: {{ enabled: [] }}\ntools: {{ native: [], plugins: [], apps: [] }}\nloop: {{ max_turns: 50, max_tool_rounds: 100 }}\n"
        )
    }

    #[tokio::test]
    async fn first_upgrade_removes_only_legacy_agent_data_and_preserves_operational_data() {
        let fixture = BootstrapFixture::legacy().await;
        fixture
            .store
            .set_setting_raw("agent_model", "free")
            .await
            .unwrap();
        fixture
            .store
            .set_setting_raw("agent_perm_mode", "full")
            .await
            .unwrap();
        fixture
            .store
            .set_setting_raw("unrelated", "keep")
            .await
            .unwrap();
        fixture.write_legacy_memory("MEMORY.md", "old fact");
        fixture.insert_project_provider_and_session().await;
        let result = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(result.reason, BootstrapReason::FirstUpgrade);
        assert_eq!(
            result.registry.snapshot().await.agents[0].profile.name,
            "Ryuzi"
        );
        assert_eq!(
            fixture.store.get_setting("agent_model").await.unwrap(),
            None
        );
        assert_eq!(
            fixture.store.get_setting("agent_perm_mode").await.unwrap(),
            None
        );
        assert_eq!(
            fixture
                .store
                .get_setting("unrelated")
                .await
                .unwrap()
                .as_deref(),
            Some("keep")
        );
        fixture.assert_project_provider_and_session_survive().await;
        assert!(!fixture.config_root().join("memory").exists());
    }

    #[tokio::test]
    async fn sql_cleanup_failure_continues_without_replacing_new_registry_or_operational_data() {
        let fixture = BootstrapFixture::legacy().await;
        fixture
            .store
            .set_setting_raw("agent_model", "free")
            .await
            .unwrap();
        fixture.insert_project_provider_and_session().await;
        fixture.store.fail_next_legacy_agent_settings_delete();
        assert!(
            initialize_agent_registry(fixture.config_root(), fixture.store.clone())
                .await
                .is_err()
        );
        let profile = fixture.config_root().join("agents/ryuzi/agent.yaml");
        let mut customized = std::fs::read_to_string(&profile).unwrap();
        customized = customized.replace("name: Ryuzi", "name: Ryuzi Customized");
        std::fs::write(&profile, &customized).unwrap();
        std::fs::write(
            fixture
                .config_root()
                .join("agents/ryuzi/knowledge/memory/user/new.md"),
            "post-upgrade data",
        )
        .unwrap();

        let resumed = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(resumed.reason, BootstrapReason::FirstUpgradeCleanup);
        assert!(std::fs::read_to_string(profile)
            .unwrap()
            .contains("Ryuzi Customized"));
        assert!(fixture
            .config_root()
            .join("agents/ryuzi/knowledge/memory/user/new.md")
            .exists());
        fixture.assert_project_provider_and_session_survive().await;
        assert_eq!(
            fixture.store.get_setting("agent_model").await.unwrap(),
            None
        );
        assert_eq!(
            fixture
                .store
                .get_setting(AGENT_PERSISTENCE_MARKER)
                .await
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }

    #[tokio::test]
    async fn failed_bootstrap_does_not_set_marker_and_retries() {
        let fixture = BootstrapFixture::fresh().await;
        fixture
            .failpoint
            .set(TransactionFailpoint::BeforeIndexReplace);
        assert!(
            initialize_agent_registry(fixture.config_root(), fixture.store.clone())
                .await
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .get_setting("agent_persistence_schema")
                .await
                .unwrap(),
            None
        );
        fixture.failpoint.clear();
        assert!(
            initialize_agent_registry(fixture.config_root(), fixture.store.clone())
                .await
                .is_ok()
        );
        assert_eq!(
            fixture
                .store
                .get_setting("agent_persistence_schema")
                .await
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }

    #[tokio::test]
    async fn recovery_creates_unique_ryuzi_without_overwriting_invalid_directory() {
        let fixture = BootstrapFixture::invalid_ryuzi_directory().await;
        let result = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(result.reason, BootstrapReason::Recovery);
        let snapshot = result.registry.snapshot().await;
        assert!(snapshot
            .agents
            .iter()
            .any(|a| a.profile.id == "ryuzi" && !a.executable));
        assert!(snapshot
            .agents
            .iter()
            .any(|a| a.profile.name == "Ryuzi" && a.executable && a.profile.id != "ryuzi"));
        assert!(snapshot
            .recovery
            .iter()
            .any(|n| n.code == "default-created"));
        // The invalid user-owned directory is preserved byte-for-byte.
        let raw =
            std::fs::read_to_string(fixture.config_root().join("agents/ryuzi/agent.yaml")).unwrap();
        assert!(raw.contains("name: Legacy Ryuzi"));
    }

    #[tokio::test]
    async fn reset_replaces_agents_but_preserves_projects_providers_and_sessions() {
        let fixture = BootstrapFixture::initialized_with_two_agents().await;
        fixture.insert_project_provider_and_session().await;
        let reset = reset_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(reset.reason, BootstrapReason::Reset);
        let snapshot = reset.registry.snapshot().await;
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].profile.name, "Ryuzi");
        fixture.assert_project_provider_and_session_survive().await;
    }

    #[tokio::test]
    async fn default_route_seeding_preserves_existing_user_routes() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
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
        routes::save_model_route(
            &store,
            ModelRouteInfo {
                id: "custom-smart".into(),
                name: "Smart".into(),
                enabled: true,
                strategy: ModelRouteStrategy::Fallback,
                targets: vec![ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "custom".into(),
                    effort: None,
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        ensure_default_routes(&store).await.unwrap();

        let routes = routes::list_model_routes(&store).await.unwrap();
        assert_eq!(routes.len(), 2);
        let smart = routes.iter().find(|route| route.name == "Smart").unwrap();
        assert!(smart.enabled);
        assert_eq!(smart.targets[0].model, "custom");
        assert!(routes::route_by_name(&routes, "free").is_some());
    }

    #[tokio::test]
    async fn fresh_install_creates_the_exact_ryuzi_and_subagent_templates() {
        let fixture = BootstrapFixture::fresh().await;
        let result = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(result.reason, BootstrapReason::FreshInstall);
        let snapshot = result.registry.snapshot().await;
        assert_eq!(snapshot.agents.len(), 1);
        let agent = &snapshot.agents[0];
        assert!(agent.executable);
        assert_eq!(agent.profile.name, "Ryuzi");
        assert_eq!(
            agent.profile.description,
            "General-purpose coding and operations agent."
        );
        assert_eq!(agent.profile.avatar.color, "blue");
        assert_eq!(
            agent.profile.model,
            AgentModel::Route {
                route: "free".into()
            }
        );
        assert_eq!(agent.profile.permissions.mode, PermMode::Default);
        assert!(agent.profile.permissions.rules.is_empty());
        assert!(agent.profile.skills.is_empty());
        assert!(agent.profile.tools.native.is_empty());
        assert!(agent.profile.tools.plugins.is_empty());
        assert!(agent.profile.tools.apps.is_empty());
        assert_eq!(
            agent.profile.personality,
            crate::agents::personality::AgentPersonality::default_profile()
        );
        assert_eq!(
            snapshot.subagent_model,
            AgentModel::Route {
                route: "free".into()
            }
        );
        // A second startup adopts the created files.
        let again = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(again.reason, BootstrapReason::Existing);
    }

    #[tokio::test]
    async fn bootstrap_recovers_prepared_knowledge_before_ensuring_bundle() {
        let fixture = BootstrapFixture::fresh().await;
        let persistence =
            initialize_agent_persistence(fixture.config_root(), fixture.store.clone())
                .await
                .unwrap();
        let agent_id = persistence.registry.default_agent_id().await;
        let knowledge = persistence.knowledge.for_agent(&agent_id).unwrap();
        let created = knowledge
            .create(super::super::okf::KnowledgeConceptInput {
                area: super::super::okf::ConceptArea::Memory(
                    super::super::okf::KnowledgeScope::User,
                ),
                title: "Original".into(),
                description: "Original description".into(),
                body: "original body".into(),
                tags: Vec::new(),
                extensions: IndexMap::new(),
            })
            .await
            .unwrap();
        let prepared = knowledge.prepare_bundle_transaction().unwrap();
        std::fs::rename(knowledge.root(), prepared.join("backup")).unwrap();
        drop(persistence);

        let restarted = initialize_agent_persistence(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let restored = restarted
            .knowledge
            .for_agent(&agent_id)
            .unwrap()
            .read(&created.id)
            .await
            .unwrap();
        assert_eq!(restored.body, "original body");
        assert!(!prepared.exists());
    }

    #[tokio::test]
    async fn bootstrap_creates_knowledge_bundle_with_generated_indexes_and_log() {
        let fixture = BootstrapFixture::fresh().await;
        let result = initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        let agent_id = result.registry.snapshot().await.agents[0]
            .profile
            .id
            .clone();
        let bundle = crate::paths::agent_knowledge_dir_in(&fixture.config_root(), &agent_id);
        for directory in [
            "memory/global",
            "memory/user",
            "memory/projects",
            "learning/skills",
            "learning/reviews",
            "learning/journey",
            "curator",
            "curator/history",
        ] {
            assert!(bundle.join(directory).is_dir(), "missing {directory}");
        }
        for generated in [
            "index.md",
            "memory/index.md",
            "learning/index.md",
            "curator/index.md",
            "log.md",
        ] {
            let path = bundle.join(generated);
            assert!(path.is_file(), "missing {generated}");
            assert!(std::fs::read_to_string(&path).unwrap().is_empty());
        }
        // A second startup does not disturb the existing bundle.
        std::fs::write(bundle.join("log.md"), "- existing entry\n").unwrap();
        initialize_agent_registry(fixture.config_root(), fixture.store.clone())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(bundle.join("log.md")).unwrap(),
            "- existing entry\n"
        );
    }

    #[tokio::test]
    async fn unsupported_schema_marker_fails_bootstrap() {
        let fixture = BootstrapFixture::fresh().await;
        fixture
            .store
            .set_setting_raw(AGENT_PERSISTENCE_MARKER, "2")
            .await
            .unwrap();
        let error =
            match initialize_agent_registry(fixture.config_root(), fixture.store.clone()).await {
                Ok(_) => panic!("bootstrap must fail for an unsupported schema"),
                Err(error) => error.to_string(),
            };
        assert!(error.contains("unsupported agent persistence schema"));
    }

    #[test]
    fn default_ryuzi_profile_uses_default_personality() {
        let profile = default_ryuzi_profile("ryuzi".into());
        assert_eq!(
            profile.personality,
            crate::agents::personality::AgentPersonality::default_profile()
        );
    }

    #[tokio::test]
    async fn ensure_free_providers_seeded_adds_mimo_and_opencode_once() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();

        ensure_free_providers_seeded(&store).await.unwrap();
        let conns = crate::llm_router::connections::list_connections(&store)
            .await
            .unwrap();
        let providers: std::collections::HashSet<_> =
            conns.iter().map(|c| c.provider.as_str()).collect();
        assert!(providers.contains("mimo-free"));
        assert!(providers.contains("opencode-free"));
        assert!(conns.iter().all(|c| c.enabled && c.auth_type == "free"));
        let seeded = conns.len();

        // Idempotent: a second call adds nothing.
        ensure_free_providers_seeded(&store).await.unwrap();
        let again = crate::llm_router::connections::list_connections(&store)
            .await
            .unwrap();
        assert_eq!(again.len(), seeded);
    }

    #[tokio::test]
    async fn ensure_free_providers_seeded_does_not_readd_after_user_deletes() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(db.path()).await.unwrap();
        ensure_free_providers_seeded(&store).await.unwrap();
        let conns = crate::llm_router::connections::list_connections(&store)
            .await
            .unwrap();
        for c in &conns {
            crate::llm_router::connections::remove_connection(&store, &c.id)
                .await
                .unwrap();
        }
        // Marker is set, so re-seeding is a no-op even though the rows are gone.
        ensure_free_providers_seeded(&store).await.unwrap();
        let after = crate::llm_router::connections::list_connections(&store)
            .await
            .unwrap();
        assert!(after.is_empty());
    }
}
