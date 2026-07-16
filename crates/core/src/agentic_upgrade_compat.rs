//! Cross-resource crash-order compatibility proof (Plan 6 Task 2).
//!
//! Plan 6 retires the legacy single-agent / Learning-hub / orchestrator state
//! through two *independently* idempotent cleanup paths that commit against two
//! different resources with two different completion markers:
//!
//!   * The SQL path is Task 1's numbered migration 39. Its marker is the
//!     SQLite `user_version`: once the DB opens at v39 the five legacy tables
//!     and five legacy settings keys are gone and re-opening never replays the
//!     destructive statements against non-legacy rows.
//!   * The filesystem path is Plan 2's `initialize_agent_registry`. Its marker
//!     is the `agent_persistence_schema` setting: once it is `"1"` the built-in
//!     Ryuzi bundle exists and bootstrap never repeats destructive filesystem
//!     work (`memory/` deletion, agent-directory replacement).
//!
//! A real upgrade can crash *between* the two commits in either order. These
//! tests prove the two paths converge on the same end state regardless of which
//! marker lands first, and that after convergence neither marker triggers a
//! destructive replay that would wipe freshly created agentic state.
//!
//! This module lives in-crate (not under `tests/`) on purpose: the crash
//! injection it relies on — `BootstrapFailpoint` / `TransactionFailpoint` — is
//! only read by bootstrap under `#[cfg(test)]`, so an external integration test
//! crate could never arm it. It touches nothing under `crates/core/src/agents`.

use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::agents::bootstrap::testing::BootstrapFailpoint;
use crate::agents::bootstrap::{
    initialize_agent_persistence, initialize_agent_registry, BootstrapReason,
    AGENT_PERSISTENCE_MARKER,
};
use crate::agents::okf::{ConceptArea, KnowledgeConceptInput, KnowledgeScope};
use crate::agents::transaction::TransactionFailpoint;
use crate::api::dispatch;
use crate::api::tests_support::state;
use crate::domain::{
    AgentIdentitySnapshot, AgentRunKind, AgentRunStatus, NewAgentRun, NewMessage, NewProviderTurn,
    PermMode, Project, Session, SessionKind, SessionStatus,
};
use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget};
use crate::sessions::ownership::{resolve_session_agent_access, SessionAgentAccess};
use crate::store::{PluginInstallRecord, Store};
use serde_json::json;

/// A valid, non-registered agent profile used to prove that bundle files
/// created after convergence are never destructively replayed away.
const REVIEWER_PROFILE_YAML: &str = "schema_version: 1\nid: reviewer\nname: Reviewer\ndescription: Post-convergence agent.\navatar: { color: violet }\nmodel: { route: smart }\npermissions: { mode: ask, rules: [] }\nskills: { enabled: [] }\ntools: { native: [], plugins: [], apps: [] }\nloop: { max_turns: 50, max_tool_rounds: 100 }\n";

/// Owns the temporary config root and DB file. The store itself is created and
/// re-opened as a local in each test so its `Arc<Store>` reference count can
/// drop to zero before `Store::open` re-runs migrations — the same
/// drop-then-reopen idiom `store.rs`'s migration tests rely on.
struct Fixture {
    root: tempfile::TempDir,
    db: tempfile::NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            db: tempfile::NamedTempFile::new().unwrap(),
        }
    }

    fn config_root(&self) -> PathBuf {
        self.root.path().to_owned()
    }

    /// Opens (or re-opens) the store, running every migration to the latest
    /// version — including migration 39 — on each open.
    async fn open_store(&self) -> Arc<Store> {
        Arc::new(Store::open(self.db.path()).await.unwrap())
    }

    fn failpoint(&self) -> BootstrapFailpoint {
        BootstrapFailpoint::for_root(self.root.path())
    }

    /// Seeds the legacy `<config>/memory` directory with a couple of old
    /// single-agent files. Its mere existence drives the first-upgrade path.
    fn write_legacy_memory(&self) {
        let memory = self.root.path().join("memory");
        std::fs::create_dir_all(memory.join("user")).unwrap();
        std::fs::write(memory.join("MEMORY.md"), "old single-agent memory").unwrap();
        std::fs::write(memory.join("user").join("notes.md"), "old user notes").unwrap();
    }
}

/// The two named routes that make the built-in Ryuzi profile executable, so
/// convergence lands on exactly one executable Ryuzi bundle (a non-executable
/// Ryuzi would trip `append_default_ryuzi` and produce a second bundle).
async fn seed_routes(store: &Store) {
    for name in ["smart", "fast"] {
        routes::save_model_route(
            store,
            ModelRouteInfo {
                id: String::new(),
                name: name.into(),
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
    }
}

/// Unrelated project / provider / session / plugin rows plus one unrelated
/// setting. None of them is legacy agent state; every cleanup path must leave
/// them untouched.
async fn seed_sentinels(store: &Store) {
    store
        .insert_project(Project {
            project_id: "proj-keep".into(),
            name: "Keep project".into(),
            workdir: "/keep".into(),
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
        store,
        ConnectionRow {
            id: "provider-keep".into(),
            provider: "anthropic".into(),
            auth_type: "api_key".into(),
            label: "Keep provider".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData::default(),
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    store
        .insert_session(Session {
            session_pk: "sess-keep".into(),
            primary_agent_id: None,
            primary_agent_snapshot: None,
            project_id: Some("proj-keep".into()),
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: Some("Historical chat".into()),
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
        })
        .await
        .unwrap();
    store
        .upsert_plugin_install(&PluginInstallRecord {
            plugin_id: "plugin-keep".into(),
            kind: "connector".into(),
            source_spec: "github:example/keep".into(),
            resolved_commit: Some("abc123".into()),
            fingerprint: "fp".into(),
            installed_at: 1,
            updated_at: 1,
            pinned: false,
            pin_reason: None,
            trust_tier: "trusted".into(),
            trust_ack_at: Some(1),
            trust_ack_summary: Some("ok".into()),
        })
        .await
        .unwrap();
    store
        .set_setting_raw("workdir_root", "/keep-root")
        .await
        .unwrap();
}

/// Asserts every unrelated sentinel survives, byte-for-byte where it has a body.
async fn assert_sentinels_survive(store: &Store) {
    let project = store.get_project("proj-keep").await.unwrap();
    assert!(project.is_some(), "project sentinel was removed");
    assert_eq!(project.unwrap().workdir, "/keep");
    assert!(
        connections::get_connection(store, "provider-keep")
            .await
            .unwrap()
            .is_some(),
        "provider sentinel was removed"
    );
    let session = store.get_session("sess-keep").await.unwrap();
    assert!(session.is_some(), "session sentinel was removed");
    assert_eq!(session.unwrap().title.as_deref(), Some("Historical chat"));
    assert!(
        store
            .get_plugin_install("plugin-keep")
            .await
            .unwrap()
            .is_some(),
        "plugin sentinel was removed"
    );
    assert_eq!(
        store.get_setting("workdir_root").await.unwrap().as_deref(),
        Some("/keep-root"),
        "unrelated setting was removed"
    );
}

/// Re-creates the five legacy tables (matching the shapes migrations 11/12/28
/// gave them), inserts a legacy row into each, writes the five legacy settings
/// keys plus one legacy `background_events` row, then rewinds `user_version` by
/// one so the next `Store::open` replays migration 39 for real. This is the
/// exact recreate-then-rewind idiom `store.rs`'s
/// `agentic_cleanup_removes_only_legacy_agent_data_and_preserves_history`
/// uses to get around the fact that a plain open already dropped the tables.
///
/// Requires the `sess-keep` session sentinel to already exist (the
/// `background_events` row references it).
async fn seed_pre_migration_legacy_sql(store: &Store) {
    store
        .with_conn(|c| {
            c.execute_batch(
                r#"
                CREATE TABLE orch_tasks (
                    id TEXT PRIMARY KEY,
                    root_id TEXT,
                    project_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body TEXT NOT NULL,
                    agent TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'todo',
                    session_pk TEXT,
                    result TEXT,
                    error TEXT,
                    created_at INTEGER,
                    finished_at INTEGER
                );
                CREATE TABLE orch_task_deps (
                    task_id TEXT NOT NULL,
                    dep_id TEXT NOT NULL,
                    PRIMARY KEY (task_id, dep_id)
                );
                CREATE TABLE skill_usage (
                    name TEXT PRIMARY KEY NOT NULL,
                    created_by TEXT,
                    use_count INTEGER NOT NULL DEFAULT 0,
                    view_count INTEGER NOT NULL DEFAULT 0,
                    patch_count INTEGER NOT NULL DEFAULT 0,
                    last_used_at INTEGER,
                    last_viewed_at INTEGER,
                    last_patched_at INTEGER,
                    state TEXT NOT NULL DEFAULT 'active',
                    pinned INTEGER NOT NULL DEFAULT 0,
                    archived_at INTEGER,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE curator_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_run_at INTEGER,
                    last_run_id TEXT
                );
                CREATE TABLE curator_runs (
                    id TEXT PRIMARY KEY NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER,
                    status TEXT NOT NULL,
                    transitioned INTEGER NOT NULL DEFAULT 0,
                    consolidated INTEGER NOT NULL DEFAULT 0,
                    snapshot_path TEXT,
                    error TEXT,
                    log TEXT
                );
                INSERT INTO orch_tasks(id,project_id,title,body,status,created_at)
                  VALUES ('legacy-orch','proj-keep','old','old','done',1);
                INSERT INTO orch_task_deps(task_id,dep_id) VALUES ('legacy-orch','legacy-dep');
                INSERT INTO skill_usage(name,created_by,use_count,view_count,patch_count,state,pinned,created_at)
                  VALUES ('legacy-skill','agent',1,1,0,'active',0,1);
                INSERT INTO curator_state(id,last_run_at,last_run_id) VALUES (1,1,'legacy-run');
                INSERT INTO curator_runs(id,started_at,status,transitioned,consolidated)
                  VALUES ('legacy-run',1,'ok',1,0);
                INSERT OR REPLACE INTO settings(key,value) VALUES
                  ('agent_model','openai/gpt-old'),
                  ('agent_perm_mode','full'),
                  ('agent.max_provider_turns','77'),
                  ('agent.auto_continue_budget','8'),
                  ('memory.nudge_interval','2');
                INSERT INTO background_events(id,target_session_pk,kind,payload,created_at)
                  VALUES ('keep-notification','sess-keep','notification','{}',1),
                         ('drop-learning','sess-keep','learning','{}',1);
                "#,
            )?;
            let current: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            c.pragma_update(None, "user_version", current - 1)
        })
        .await
        .unwrap();
}

/// Number of the five legacy tables that currently exist.
async fn present_legacy_table_count(store: &Store) -> i64 {
    store
        .with_conn(|c| {
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('orch_tasks','orch_task_deps','skill_usage','curator_state','curator_runs')",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap()
}

async fn setting_present(store: &Store, key: &str) -> bool {
    store.get_setting(key).await.unwrap().is_some()
}

async fn background_event_present(store: &Store, id: &str) -> bool {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                "SELECT count(*) FROM background_events WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .map(|count| count > 0)
        })
        .await
        .unwrap()
}

/// The single executable Ryuzi bundle assertion shared by both convergence
/// paths.
async fn assert_single_executable_ryuzi(registry: &crate::agents::registry::AgentRegistry) {
    let snapshot = registry.snapshot().await;
    assert_eq!(
        snapshot.agents.len(),
        1,
        "expected exactly one agent bundle"
    );
    assert_eq!(snapshot.agents[0].profile.name, "Ryuzi");
    assert!(
        snapshot.agents[0].executable,
        "the converged Ryuzi bundle must be executable"
    );
}

/// Shared post-convergence tail: create a brand-new agent file, a brand-new OKF
/// concept, and a brand-new non-legacy setting, then reopen the Store and agent
/// persistence twice. All three must survive — proving each completion marker
/// gates only its own cleanup and neither replays destructively.
async fn assert_markers_gate_only_their_own_cleanup(fixture: &Fixture, store: Arc<Store>) {
    let config_root = fixture.config_root();

    // Brand-new OKF concept on the converged default agent.
    let persistence = initialize_agent_persistence(config_root.clone(), store.clone())
        .await
        .unwrap();
    let agent_id = persistence.registry.default_agent_id().await;
    let concept = persistence
        .knowledge
        .for_agent(&agent_id)
        .unwrap()
        .create(KnowledgeConceptInput {
            area: ConceptArea::Memory(KnowledgeScope::User),
            title: "Post-convergence".into(),
            description: "must survive reopen".into(),
            body: "durable concept body".into(),
            tags: Vec::new(),
            extensions: IndexMap::new(),
        })
        .await
        .unwrap();
    let concept_id = concept.id.clone();
    drop(persistence);

    // Brand-new agent file that no marker/cleanup owns.
    let reviewer = config_root
        .join("agents")
        .join("reviewer")
        .join("agent.yaml");
    std::fs::create_dir_all(reviewer.parent().unwrap()).unwrap();
    std::fs::write(&reviewer, REVIEWER_PROFILE_YAML).unwrap();
    let reviewer_bytes = std::fs::read(&reviewer).unwrap();

    // Brand-new non-legacy setting.
    store
        .set_setting_raw("agentic.reviewer.pref", "keep")
        .await
        .unwrap();

    // Reopen Store + agent persistence twice; every reopen re-runs migration 39
    // and re-enters bootstrap, and each must adopt (never replay-destroy) the
    // committed state.
    let mut store = store;
    for pass in 0..2 {
        drop(store);
        store = fixture.open_store().await;
        let persistence = initialize_agent_persistence(config_root.clone(), store.clone())
            .await
            .unwrap();
        assert_eq!(
            persistence.reason,
            BootstrapReason::Existing,
            "reopen pass {pass} must adopt the existing registry, not re-bootstrap"
        );
        let agent_id = persistence.registry.default_agent_id().await;

        assert!(
            reviewer.exists(),
            "reviewer agent file vanished on pass {pass}"
        );
        assert_eq!(
            std::fs::read(&reviewer).unwrap(),
            reviewer_bytes,
            "reviewer agent file changed on pass {pass}"
        );

        let restored = persistence
            .knowledge
            .for_agent(&agent_id)
            .unwrap()
            .read(&concept_id)
            .await
            .unwrap();
        assert_eq!(
            restored.body, "durable concept body",
            "OKF concept lost on pass {pass}"
        );

        assert_eq!(
            store
                .get_setting("agentic.reviewer.pref")
                .await
                .unwrap()
                .as_deref(),
            Some("keep"),
            "non-legacy setting lost on pass {pass}"
        );

        drop(persistence);
    }
}

/// SQL cleanup (migration 39) commits FIRST; the Plan 2 filesystem journal then
/// crashes at its pre-journal-commit failure point and only converges on a
/// clean restart.
#[tokio::test]
async fn sql_cleanup_committed_before_plan2_journal_recovery_converges() {
    let fixture = Fixture::new();

    // Opening the store runs migration 39 immediately: the SQL cleanup is
    // already committed before Plan 2 ever touches the filesystem.
    let store = fixture.open_store().await;
    seed_routes(&store).await;
    seed_sentinels(&store).await;
    // Legacy *filesystem* state remains — this is what still drives the upgrade.
    fixture.write_legacy_memory();

    // SQL cleanup already done: legacy tables/settings absent from the start.
    assert_eq!(present_legacy_table_count(&store).await, 0);
    for key in [
        "agent_model",
        "agent_perm_mode",
        "agent.max_provider_turns",
        "agent.auto_continue_budget",
        "memory.nudge_interval",
    ] {
        assert!(
            !setting_present(&store, key).await,
            "legacy key {key} present"
        );
    }

    // Arm the pre-journal-commit failure point. `BeforeIndexReplace` bails
    // inside `commit_before_marker` *before* the journal is marked Committed,
    // so `commit()` rolls the whole prepared transaction back
    // (`rollback_after_failure` -> `rollback_prepared`): the staged Ryuzi
    // directory is removed and the trashed `memory/` directory is restored to
    // its original location. Bootstrap returns Err and never reaches the
    // marker write. This is exactly the state Plan 2's own
    // `failed_bootstrap_does_not_set_marker_and_retries` (bootstrap.rs) asserts
    // for the same failpoint.
    let failpoint = fixture.failpoint();
    failpoint.set(TransactionFailpoint::BeforeIndexReplace);
    assert!(
        initialize_agent_registry(fixture.config_root(), store.clone())
            .await
            .is_err(),
        "injected pre-journal-commit failure must abort bootstrap"
    );

    // Marker absent, legacy files recovered (rolled back), no registry written.
    assert_eq!(
        store.get_setting(AGENT_PERSISTENCE_MARKER).await.unwrap(),
        None,
        "marker must not be set after a crashed bootstrap"
    );
    assert!(
        fixture
            .config_root()
            .join("memory")
            .join("MEMORY.md")
            .exists(),
        "legacy memory must be recoverable after rollback"
    );
    assert!(
        !fixture
            .config_root()
            .join("agents")
            .join("index.yaml")
            .exists(),
        "no registry index must exist after rollback"
    );
    // Cross-resource invariant: the failed filesystem attempt left the
    // already-committed SQL cleanup exactly as it was.
    assert_eq!(present_legacy_table_count(&store).await, 0);

    // Restart with no injection: bootstrap converges.
    failpoint.clear();
    let converged = initialize_agent_registry(fixture.config_root(), store.clone())
        .await
        .unwrap();
    assert_eq!(converged.reason, BootstrapReason::FirstUpgrade);
    assert_single_executable_ryuzi(&converged.registry).await;
    assert!(
        !fixture.config_root().join("memory").exists(),
        "legacy memory must be removed after convergence"
    );
    assert_eq!(
        store
            .get_setting(AGENT_PERSISTENCE_MARKER)
            .await
            .unwrap()
            .as_deref(),
        Some("1"),
        "marker must be committed after convergence"
    );
    assert_sentinels_survive(&store).await;
    drop(converged);

    assert_markers_gate_only_their_own_cleanup(&fixture, store).await;
}

/// The Plan 2 filesystem journal commits FIRST (bootstrap runs to completion
/// against a still-pre-migration-39 database); the SQL cleanup only lands on the
/// next `Store::open`.
#[tokio::test]
async fn plan2_journal_committed_before_sql_cleanup_converges() {
    let fixture = Fixture::new();

    let store = fixture.open_store().await;
    seed_routes(&store).await;
    seed_sentinels(&store).await;
    // Put the DB back at pre-migration-39 with the legacy rows present.
    seed_pre_migration_legacy_sql(&store).await;
    // A legacy memory directory rides alongside the legacy SQL.
    fixture.write_legacy_memory();

    // Precondition: legacy SQL really is present before bootstrap.
    assert_eq!(present_legacy_table_count(&store).await, 5);

    // Run Plan 2 bootstrap to completion (no failpoint). Its filesystem journal
    // commits and its own `delete_legacy_agent_settings` removes only
    // agent_model/agent_perm_mode; the *migration-39-owned* legacy SQL is
    // untouched because migration 39 has not run against this handle.
    let bootstrap = initialize_agent_registry(fixture.config_root(), store.clone())
        .await
        .unwrap();
    assert_eq!(bootstrap.reason, BootstrapReason::FirstUpgrade);
    assert_single_executable_ryuzi(&bootstrap.registry).await;
    assert_eq!(
        store
            .get_setting(AGENT_PERSISTENCE_MARKER)
            .await
            .unwrap()
            .as_deref(),
        Some("1"),
        "filesystem marker must be committed"
    );
    // Capture the converged bundle bytes before the SQL cleanup runs.
    let agents_dir = fixture.config_root().join("agents");
    let ryuzi_bytes = std::fs::read(agents_dir.join("ryuzi").join("agent.yaml")).unwrap();
    let index_bytes = std::fs::read(agents_dir.join("index.yaml")).unwrap();
    let subagents_bytes = std::fs::read(agents_dir.join("subagents.yaml")).unwrap();
    // Release the registry's `Arc<Store>` clone so the re-open can fully close.
    drop(bootstrap);

    // Legacy SQL is STILL present: migration 39 has not run yet. The two
    // bootstrap-owned keys are already gone; everything migration 39 owns
    // remains.
    assert_eq!(present_legacy_table_count(&store).await, 5);
    for key in [
        "agent.max_provider_turns",
        "agent.auto_continue_budget",
        "memory.nudge_interval",
    ] {
        assert!(
            setting_present(&store, key).await,
            "legacy key {key} missing"
        );
    }
    assert!(background_event_present(&store, "drop-learning").await);

    // Now the SQL cleanup lands: re-open runs migration 39.
    drop(store);
    let store = fixture.open_store().await;

    // Legacy SQL disappears...
    assert_eq!(present_legacy_table_count(&store).await, 0);
    for key in [
        "agent_model",
        "agent_perm_mode",
        "agent.max_provider_turns",
        "agent.auto_continue_budget",
        "memory.nudge_interval",
    ] {
        assert!(
            !setting_present(&store, key).await,
            "legacy key {key} survived"
        );
    }
    assert!(
        !background_event_present(&store, "drop-learning").await,
        "legacy learning background_event survived migration 39"
    );
    // ...the non-legacy background_event survives migration 39.
    assert!(
        background_event_present(&store, "keep-notification").await,
        "non-legacy background_event was removed by migration 39"
    );

    // ...while the Ryuzi bundle and sentinels remain byte-for-byte unchanged.
    assert_eq!(
        std::fs::read(agents_dir.join("ryuzi").join("agent.yaml")).unwrap(),
        ryuzi_bytes,
        "Ryuzi profile changed after SQL cleanup"
    );
    assert_eq!(
        std::fs::read(agents_dir.join("index.yaml")).unwrap(),
        index_bytes,
        "registry index changed after SQL cleanup"
    );
    assert_eq!(
        std::fs::read(agents_dir.join("subagents.yaml")).unwrap(),
        subagents_bytes,
        "subagents config changed after SQL cleanup"
    );
    assert_sentinels_survive(&store).await;

    assert_markers_gate_only_their_own_cleanup(&fixture, store).await;
}

// --------------------------------------------------------------------------
// Plan 6 Task 3: public upgrade compatibility + read-only historical sessions.
//
// The two tests above prove destructive cleanup converges without wiping
// unrelated history. These two prove the *positive* upgrade contract a public
// user experiences: a pre-feature session survives the upgrade untouched, is
// still readable/exportable, and yet rejects every run/mutation through one
// centralized read-only guard — legacy (no owner) and deleted-owner alike.
// --------------------------------------------------------------------------

/// A pre-feature (Plan-4-unaware) session: no primary-agent ownership at all.
fn legacy_session(session_pk: &str, project_id: Option<&str>) -> Session {
    Session {
        session_pk: session_pk.into(),
        primary_agent_id: None,
        primary_agent_snapshot: None,
        project_id: project_id.map(str::to_string),
        agent_session_id: None,
        worktree_path: None,
        branch: None,
        title: Some("Historical chat".into()),
        status: SessionStatus::Idle,
        perm_mode: PermMode::Default,
        started_by: None,
        created_at: Some(1),
        last_active: Some(1),
        resume_attempts: 0,
        branch_owned: false,
        kind: SessionKind::Chat,
        speaker: None,
        agent: None,
        parent_session_pk: None,
    }
}

/// A session whose primary owner has since been deleted: the owner id and its
/// immutable identity snapshot both persist, but the agent is gone from the
/// registry.
fn deleted_owner_session(session_pk: &str, snapshot: AgentIdentitySnapshot) -> Session {
    Session {
        primary_agent_id: Some(snapshot.id.clone()),
        primary_agent_snapshot: Some(snapshot),
        ..legacy_session(session_pk, None)
    }
}

/// A run row for the read-only session, used only so the child-run read/control
/// paths have a real target to resolve.
fn historical_run(session_pk: &str, run_id: &str, parent_run_id: Option<&str>) -> NewAgentRun {
    NewAgentRun {
        run_id: run_id.into(),
        session_pk: session_pk.into(),
        parent_run_id: parent_run_id.map(str::to_string),
        retry_of: None,
        primary_agent_id: "reviewer".into(),
        executing_agent_id: Some("reviewer".into()),
        executing_agent_name_snapshot: "Reviewer".into(),
        agent_kind: if parent_run_id.is_some() {
            AgentRunKind::Subagent
        } else {
            AgentRunKind::Primary
        },
        task: "history".into(),
        status: AgentRunStatus::Failed,
        resolved_model: None,
        resolved_effort: None,
    }
}

/// Step 1: a pre-feature database survives the Plan 2 upgrade bootstrap
/// unchanged, the legacy session resolves as read-only against the upgraded
/// registry, and the upgrade still lands exactly one built-in `Ryuzi` agent.
/// No row is ever re-owned by `Ryuzi`.
#[tokio::test]
async fn legacy_pre_feature_database_survives_upgrade_and_resolves_read_only() {
    let fixture = Fixture::new();
    let store = fixture.open_store().await;
    seed_routes(&store).await;

    store
        .insert_project(Project {
            project_id: "p-keep".into(),
            name: "Keep project".into(),
            workdir: "/keep".into(),
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
        &store,
        ConnectionRow {
            id: "provider-keep".into(),
            provider: "anthropic".into(),
            auth_type: "api_key".into(),
            label: "Keep provider".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData::default(),
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    store
        .insert_session(legacy_session("legacy-s", Some("p-keep")))
        .await
        .unwrap();
    store
        .insert_message(NewMessage::block(
            "legacy-s",
            "user",
            "text",
            json!({ "text": "old single-agent turn" }),
        ))
        .await
        .unwrap();
    store
        .insert_provider_turn(NewProviderTurn::new(
            "legacy-s",
            "user",
            json!([{ "type": "text", "text": "old single-agent turn" }]),
        ))
        .await
        .unwrap();
    fixture.write_legacy_memory();

    let bootstrap = initialize_agent_registry(fixture.config_root(), store.clone())
        .await
        .unwrap();
    assert_eq!(bootstrap.reason, BootstrapReason::FirstUpgrade);

    // Historical data is preserved byte-for-byte.
    assert_eq!(
        store
            .list_projects()
            .await
            .unwrap()
            .iter()
            .map(|p| p.project_id.as_str())
            .collect::<Vec<_>>(),
        ["p-keep"]
    );
    assert!(connections::get_connection(&store, "provider-keep")
        .await
        .unwrap()
        .is_some());
    let legacy = store.get_session("legacy-s").await.unwrap().unwrap();
    assert_eq!(legacy.primary_agent_id, None);
    assert_eq!(legacy.primary_agent_snapshot, None);
    assert_eq!(store.list_messages("legacy-s").await.unwrap().len(), 1);
    assert_eq!(
        store.list_provider_turns("legacy-s").await.unwrap().len(),
        1
    );

    // The legacy session resolves as read-only against the upgraded registry.
    assert!(matches!(
        resolve_session_agent_access(&store, &bootstrap.registry, "legacy-s")
            .await
            .unwrap(),
        SessionAgentAccess::LegacyReadOnly
    ));

    // The upgrade lands exactly one built-in Ryuzi; nothing is re-owned by it.
    let snapshot = bootstrap.registry.snapshot().await;
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].profile.name, "Ryuzi");
    assert_eq!(
        store
            .get_session("legacy-s")
            .await
            .unwrap()
            .unwrap()
            .primary_agent_id,
        None,
        "the upgrade must never assign an owner to an old row"
    );
}

/// Step 2: every run/mutation entry point rejects a read-only session with a
/// 409 conflict and the exact per-case message (legacy vs. deleted-owner),
/// while every read/list/export path still succeeds for both.
#[tokio::test]
async fn historical_sessions_reject_runs_and_mutations_but_stay_readable() {
    let state = state().await;
    let store = state.cp.store().clone();

    store
        .insert_session(legacy_session("legacy-s", None))
        .await
        .unwrap();
    store
        .insert_session(deleted_owner_session(
            "deleted-s",
            AgentIdentitySnapshot {
                id: "reviewer".into(),
                name: "Reviewer".into(),
                avatar_color: "violet".into(),
            },
        ))
        .await
        .unwrap();

    // Seed one message and one child run per read-only session so the read /
    // list / transcript paths have something real to resolve, and the child-run
    // controls have a concrete target the read-only guard must still refuse.
    for pk in ["legacy-s", "deleted-s"] {
        store
            .insert_message(NewMessage::block(
                pk,
                "user",
                "text",
                json!({ "text": "history" }),
            ))
            .await
            .unwrap();
        let root = store
            .insert_primary_agent_run(historical_run(pk, &format!("{pk}-root"), None))
            .await
            .unwrap();
        store
            .insert_agent_run(historical_run(
                pk,
                &format!("{pk}-child"),
                Some(&root.run_id),
            ))
            .await
            .unwrap();
    }

    // The resolver classifies each session as the expected read-only variant.
    assert!(matches!(
        resolve_session_agent_access(&store, &state.agents, "legacy-s")
            .await
            .unwrap(),
        SessionAgentAccess::LegacyReadOnly
    ));
    assert!(matches!(
        resolve_session_agent_access(&store, &state.agents, "deleted-s")
            .await
            .unwrap(),
        SessionAgentAccess::DeletedReadOnly { .. }
    ));

    for (pk, expected) in [
        ("legacy-s", "Legacy agent history is read-only."),
        (
            "deleted-s",
            "Reviewer was deleted; this history is read-only.",
        ),
    ] {
        let child = format!("{pk}-child");
        let blocked: Vec<(&str, serde_json::Value)> = vec![
            (
                "continue_session",
                json!({ "sessionPk": pk, "turn": { "text": "", "attachments": [] } }),
            ),
            ("steer", json!({ "session_pk": pk, "text": "again" })),
            ("stop_session", json!({ "session_pk": pk })),
            ("end_session", json!({ "session_pk": pk })),
            (
                "update_session_runtime",
                json!({ "session_pk": pk, "model": null, "effort": null }),
            ),
            (
                "update_session_perm_mode",
                json!({ "session_pk": pk, "perm_mode": "default" }),
            ),
            (
                "retry_child_run",
                json!({ "session_pk": pk, "run_id": child }),
            ),
            (
                "cancel_child_run",
                json!({ "session_pk": pk, "run_id": child }),
            ),
            (
                "enqueue_session_message",
                json!({ "session_pk": pk, "prompt": "queued turn", "options": null }),
            ),
            (
                "remove_session_message",
                json!({ "session_pk": pk, "id": format!("{pk}-queue-item") }),
            ),
        ];
        for (method, params) in blocked {
            let error = dispatch(&state, method, params).await.unwrap_err();
            assert_eq!(error.status, 409, "{method} on {pk} must be a 409 conflict");
            assert_eq!(error.message, expected, "{method} on {pk} message");
        }

        // Read / list / transcript / export paths remain open on the same
        // read-only session.
        dispatch(&state, "list_sessions", json!({ "project_id": null }))
            .await
            .unwrap();
        dispatch(&state, "list_messages", json!({ "session_pk": pk }))
            .await
            .unwrap();
        dispatch(&state, "get_child_runs", json!({ "session_pk": pk }))
            .await
            .unwrap();
        dispatch(
            &state,
            "get_child_transcript",
            json!({ "session_pk": pk, "run_id": child }),
        )
        .await
        .unwrap();
        dispatch(&state, "export_session", json!({ "session_pk": pk }))
            .await
            .unwrap();
    }
}
