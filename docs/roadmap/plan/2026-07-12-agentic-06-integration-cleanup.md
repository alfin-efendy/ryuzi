# Agentic Integration and Legacy Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the agentic Cockpit program by destructively removing pre-feature agent configuration, memory, Learning, and orchestration data on the first upgrade while preserving projects, providers, and historical sessions; enforce legacy/deleted-agent history as read-only; remove every obsolete backend/UI contract; and prove the integrated Plans 1–5 release path with generated bindings, end-to-end tests, documentation, CI, and packaging checks.

**Architecture:** Plan 2 is the sole owner of destructive filesystem bootstrap/reset and its transaction journal; Plan 6 adds no filesystem cleanup algorithm, journal phase, or second marker. Append one independently idempotent SQLite migration for old agent-owned rows/tables and exercise it together with Plan 2's already-committed bootstrap crash points: either side may finish first, restart resumes the unfinished side, and neither side repeats after its own durable commit. Consume Plan 4's `Session` ownership fields and `sessions::ownership` read-only policy directly, consume Plan 5's route-target effort contract without another migration, then delete compatibility code and generated commands that no longer have a product caller. Add a repository-level absence checker and mock-IPC Playwright journeys so removed symbols cannot quietly return.

**Tech Stack:** Rust 2021, Tokio, rusqlite/rusqlite-migration, YAML/OKF agent registry from Plan 2, Specta and Tauri 2 binding generation, React 19, TypeScript 5, Zustand 4, Bun, Playwright, Biome, Cargo, GitHub Actions, release-please.

## Global Constraints

- Plans 1, 2, 3, 4, and 5 must be committed and green before Task 1 starts; this plan consumes their interfaces and does not recreate capability resolution, YAML/OKF persistence, management CRUD, session/delegation, or route-effort precedence.
- SQL cleanup and Plan 2 filesystem cleanup are independently idempotent and intentionally not one cross-resource transaction: SQLite's numbered migration is its only completion marker; Plan 2's `agent_persistence_schema=1` plus transaction journal is the filesystem marker. Startup may complete them in either order. Crash tests must cover SQL-committed/filesystem-pending and filesystem-committed/SQL-pending, then prove restart converges without deleting newly created post-commit agent data.
- Plan 2 already owns fresh-install, first-upgrade, and explicit-reset creation of exactly one preconfigured main agent named **Ryuzi** plus deletion of legacy agent configuration and `<ryuzi-config>/memory`; Plan 6 must consume and test that behavior, not implement it again.
- The first-upgrade cleanup is intentionally destructive only for old agent-owned settings, old freeform memory, old Learning/curator operational state, old orchestration state, and the superseded pre-feature agent registry.
- Preserve every project row, provider connection/account/catalog/route row, model preference/status row, session row, message/provider-turn/transcript row, attachment, audit row, gateway, scheduler job, plugin installation, device/pairing row, and unrelated setting.
- Pre-feature sessions retain `primary_agent_id = NULL`, render the exact identity **Legacy agent**, remain readable, and reject every mutating/run operation; they are never assigned Ryuzi retroactively.
- Sessions whose owner was deleted retain Plan 4's immutable identity snapshot, render a Deleted marker, remain readable, and reject every mutating/run operation.
- YAML and per-agent OKF remain the only agent configuration/knowledge authority; SQLite stores references, immutable identity snapshots, operational queues, runs, and delegation provenance only.
- Do not import old global freeform memory into Ryuzi's OKF bundle and do not translate old orchestration DAGs into delegation runs.
- The cleanup must be convergent: Plan 2 retries an interrupted filesystem journal until its marker commits, while SQLite independently retries/atomically commits its numbered migration; reopening after either commit does not delete newly created agent data.
- Do not delete `background_events` wholesale: remove only legacy `kind IN ('learning', 'orch')` rows because the table also carries current non-agent background work.
- Do not delete global skill installations or plugin state; only old `skill_usage`, `curator_state`, and `curator_runs` telemetry owned by the superseded global Learning flow is removed.
- Keep Plan 5's canonical route-target effort fields and suffix cleanup intact; no Plan 6 code parses model-name effort suffixes.
- Keep the repository buildable after every task and use the exact commits listed below during execution. The current request is plan-only: write this file but do not commit it.
- Preserve unrelated worktree changes and never hand-edit `apps/cockpit/src/bindings.ts`; regenerate it with `cargo gen-bindings`.

## Plans 1–5 Interfaces Consumed

The implementation must compile against the committed interfaces rather than adding parallel compatibility types:

```rust
// Plan 1: crates/core/src/llm_router/model_capabilities.rs
pub async fn resolve_for_model(
    store: &Store,
    key: &ModelPreferenceKey,
) -> anyhow::Result<ModelEffortCapabilities>;

// Plan 2: crates/core/src/agents/bootstrap.rs and persistence.rs
pub const AGENT_PERSISTENCE_MARKER: &str = "agent_persistence_schema";
pub enum BootstrapReason { Existing, FreshInstall, FirstUpgrade, Reset, Recovery }
pub async fn initialize_agent_registry(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentBootstrap>;
pub async fn reset_agent_registry(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentBootstrap>;
pub async fn initialize_agent_persistence(
    config_root: PathBuf,
    store: Arc<Store>,
) -> anyhow::Result<AgentPersistence>;

impl AgentRegistry {
    pub async fn snapshot(&self) -> AgentRegistrySnapshot;
}

pub const AGENT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_AGENT_ID: &str = "ryuzi";

// Plan 3: crates/core/src/api/types.rs
pub struct AgentSummaryInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub avatar_color: String,
    pub model: AgentModelInfo,
    pub permission_mode: String,
    pub skill_count: u32,
    pub tool_count: u32,
    pub knowledge_count: u32,
    pub executable: bool,
    pub validation: Vec<AgentValidationInfo>,
    pub is_default: bool,
}
pub struct AgentRegistryInfo { /* agents, default_agent_id, recovery, subagent_model */ }
pub struct AgentDetailInfo { /* YAML-backed profile plus model metadata */ }
pub struct AgentLearningInfo { /* per-agent OKF concepts and Learning state */ }

// Plan 4: crates/core/src/domain.rs
pub struct AgentIdentitySnapshot {
    pub id: String,
    pub name: String,
    pub avatar_color: String,
}

// These exact fields are added to the existing domain::Session; do not add a
// parallel ownership DTO.
pub primary_agent_id: Option<String>;
pub primary_agent_snapshot: Option<AgentIdentitySnapshot>;

// Plan 4: crates/core/src/sessions/ownership.rs
pub enum SessionAgentAccess {
    Executable { agent_id: String },
    LegacyReadOnly,
    DeletedReadOnly { snapshot: AgentIdentitySnapshot },
}

pub async fn resolve_session_agent_access(
    store: &Store,
    registry: &AgentRegistry,
    session_pk: &str,
) -> anyhow::Result<SessionAgentAccess>;

// Plan 4 RPCs/generated commands consumed here:
// get_child_runs/getChildRuns, get_child_transcript/getChildTranscript,
// cancel_child_run/cancelChildRun, retry_child_run/retryChildRun.

// Plan 5: crates/core/src/llm_router/routes.rs
pub struct ModelRouteTarget {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}
```

The signatures and field names above are normative cross-plan contracts; never add an adapter or parallel DTO to hide a mismatch.

## File Structure

- Modify `crates/core/src/store.rs`: append the one-time SQLite cleanup migration and migration regression tests that prove the preservation/deletion boundary and replay safety.
- Verify `crates/core/src/agents/bootstrap.rs` and its existing tests without changing the filesystem algorithm: Plan 2 remains the sole owner of first-upgrade/reset cleanup and the Ryuzi default bundle.
- Create `crates/core/tests/agentic_upgrade_compat.rs`: exercise the complete Store + AgentRegistry upgrade boundary and legacy/deleted session access through public APIs.
- Modify `crates/core/src/api/sessions.rs`, `crates/core/src/control/lifecycle.rs`, and `crates/core/src/api/session_io_api.rs`: route every run/mutation entry point through Plan 4's `SessionAgentAccess` guard while retaining read/export operations.
- Delete obsolete core files only if still present after Plans 1–5: `crates/core/src/agent_settings.rs`, `crates/core/src/orch.rs`, `crates/core/src/api/orch_api.rs`, `crates/core/src/harness/native/tools/app_orchestrate.rs`, and `crates/core/src/harness/native/tools/orch_block.rs`.
- Modify `crates/core/src/lib.rs`, `crates/core/src/api/mod.rs`, `crates/core/src/harness/native/tools/mod.rs`, `crates/core/src/daemon.rs`, and nearby tests: remove exports, registrations, loops, DTOs, events, and store methods that only supported old settings/memory/Learning/orchestration.
- Delete obsolete Tauri files only if still present: `apps/cockpit/src-tauri/src/learning_cmd.rs`; remove old orchestration and global-agent commands from `apps/cockpit/src-tauri/src/lib.rs` and `apps/cockpit/src-tauri/src/agent_cmd.rs`.
- Delete obsolete Cockpit files only if still present: `apps/cockpit/src/store-agent.ts`, `apps/cockpit/src/store-agent.test.ts`, `apps/cockpit/src/store-orch.test.ts`, `apps/cockpit/src/views/LearningView.tsx`, `apps/cockpit/src/components/session/TaskStrip.tsx`, and global-only files under `apps/cockpit/src/components/learning/` that Plan 3 did not move into agent detail.
- Modify `apps/cockpit/src/App.tsx`, `apps/cockpit/src/store.ts`, `apps/cockpit/src/store-nav.ts`, `apps/cockpit/src/components/shell/Sidebar.tsx`, session/composer views, and their tests: remove dead navigation/state/event branches and retain only Plan 3/4 agent surfaces.
- Regenerate `apps/cockpit/src/bindings.ts`: make the generated command/type graph authoritative after deletion.
- Create `scripts/check-agentic-cleanup.ts`: fail on obsolete files, commands, SQLite names, settings keys, UI labels, and generated binding exports.
- Modify `package.json` and `.github/workflows/ci.yml`: expose and run the absence checker in CI.
- Modify `apps/cockpit/e2e/mock-ipc.ts` and `apps/cockpit/e2e/app.e2e.ts`: add integrated agents, ownership, mention/delegation, route effort, and read-only history journeys.
- Modify `README.md`, `docs/development/setup.md`, and `docs/development/plugins.md`: document the new agent model, first-upgrade data deletion, OKF location, delegation, and removal of old orchestration/global memory controls.
- Modify `crates/core/CHANGELOG.md`, `apps/cockpit/src-tauri/CHANGELOG.md`, and `crates/runner/CHANGELOG.md`: add explicit unreleased breaking-change and upgrade notes.
- Verify `.github/workflows/release.yml`, `.github/workflows/cockpit-desktop.yml`, `release-please-config.json`, `.release-please-manifest.json`, `scripts/npm/`, and `npm/`: no packaging behavior change is expected.

---

### Task 1: One-Time SQLite Agent-Data Cleanup Migration

**Files:**
- Modify: `crates/core/src/store.rs`

**Interfaces:**
- Consumes: the schema produced by Plans 2, 4, and 5; `Store::open`; Plan 4 session ownership columns and run/delegation tables.
- Produces: one appended, replay-safe migration that removes only superseded agent-owned SQL state and leaves all durable non-agent/history state byte-for-byte equivalent.

- [ ] **Step 1: Write the failing preservation-boundary migration test**

In `store.rs`'s existing `#[cfg(test)] mod tests`, add a test that opens a current store, seeds representative preserved and removed rows, rewinds only the final migration slot, reopens the store, and compares preserved values rather than row counts alone:

```rust
#[tokio::test]
async fn agentic_cleanup_removes_only_legacy_agent_data_and_preserves_history() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Store::open(tmp.path()).await.unwrap();
    store.with_conn(|c| {
        c.execute_batch(
            r#"
            INSERT INTO projects(project_id,name,workdir,model,effort,perm_mode,created_at)
              VALUES ('p-keep','Keep project','/keep','route:smart','high','default',1);
            INSERT INTO sessions(session_pk,project_id,title,status,created_at,last_active)
              VALUES ('legacy-s','p-keep','Historical chat','idle',2,3);
            INSERT INTO messages(session_pk,seq,role,block_type,payload,created_at)
              VALUES ('legacy-s',1,'user','text','{"text":"keep transcript"}',4);
            INSERT INTO provider_turns(session_pk,seq,role,payload,created_at)
              VALUES ('legacy-s',1,'user','[{"type":"text","text":"keep ledger"}]',4);
            INSERT OR REPLACE INTO provider_connections
              (id,provider,auth_type,label,priority,enabled,data,created_at,updated_at)
              VALUES ('provider-keep','fixture','api_key','Keep provider',0,1,'{}',5,5);
            INSERT OR REPLACE INTO settings(key,value) VALUES
              ('workdir_root','/keep-root'),
              ('agent_model','openai/gpt-old'),
              ('agent_perm_mode','full'),
              ('agent.max_provider_turns','77'),
              ('agent.auto_continue_budget','8'),
              ('memory.nudge_interval','2');
            INSERT INTO background_events
              (id,target_session_pk,kind,payload,created_at)
              VALUES ('keep-event','legacy-s','notification','{}',6),
                     ('drop-learning','legacy-s','learning','{}',6),
                     ('drop-orch','legacy-s','orch','{}',6);
            INSERT INTO skill_usage(name,created_by,use_count,view_count,patch_count,state,pinned,created_at)
              VALUES ('old-learning','agent',1,1,0,'active',0,7);
            INSERT INTO curator_state(id,last_run_at,last_run_id)
              VALUES (1,7,'old-curator');
            INSERT INTO curator_runs(id,started_at,status,transitioned,consolidated)
              VALUES ('old-curator',7,'ok',1,0);
            INSERT INTO orch_tasks(id,project_id,title,body,status,created_at)
              VALUES ('old-orch','p-keep','old','old','done',8);
            INSERT INTO orch_task_deps(task_id,dep_id) VALUES ('old-orch','old-dep');
            "#,
        )?;
        let current: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        c.pragma_update(None, "user_version", current - 1)
    }).await.unwrap();
    drop(store);

    let store = Store::open(tmp.path()).await.unwrap();
    store.with_conn(|c| {
        let project: (String, String, Option<String>, Option<String>) = c.query_row(
            "SELECT name,workdir,model,effort FROM projects WHERE project_id='p-keep'",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        )?;
        assert_eq!(project, ("Keep project".into(), "/keep".into(), Some("route:smart".into()), Some("high".into())));
        assert_eq!(c.query_row("SELECT count(*) FROM sessions WHERE session_pk='legacy-s'", [], |r| r.get::<_, i64>(0))?, 1);
        assert_eq!(c.query_row("SELECT payload FROM messages WHERE session_pk='legacy-s'", [], |r| r.get::<_, String>(0))?, "{\"text\":\"keep transcript\"}");
        assert_eq!(c.query_row("SELECT count(*) FROM provider_turns WHERE session_pk='legacy-s'", [], |r| r.get::<_, i64>(0))?, 1);
        assert_eq!(c.query_row("SELECT count(*) FROM provider_connections WHERE id='provider-keep'", [], |r| r.get::<_, i64>(0))?, 1);
        assert_eq!(c.query_row("SELECT value FROM settings WHERE key='workdir_root'", [], |r| r.get::<_, String>(0))?, "/keep-root");
        assert_eq!(c.query_row("SELECT count(*) FROM background_events WHERE id='keep-event'", [], |r| r.get::<_, i64>(0))?, 1);
        assert_eq!(c.query_row("SELECT count(*) FROM background_events WHERE kind IN ('learning','orch')", [], |r| r.get::<_, i64>(0))?, 0);
        for key in ["agent_model", "agent_perm_mode", "agent.max_provider_turns", "agent.auto_continue_budget", "memory.nudge_interval"] {
            assert_eq!(c.query_row("SELECT count(*) FROM settings WHERE key=?1", [key], |r| r.get::<_, i64>(0))?, 0, "legacy key survived: {key}");
        }
        for table in ["orch_tasks", "orch_task_deps", "skill_usage", "curator_state", "curator_runs"] {
            assert_eq!(c.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get::<_, i64>(0))?, 0, "legacy table survived: {table}");
        }
        Ok(())
    }).await.unwrap();
}
```

Use the exact column lists committed by Plans 2–5 if their tail migration extends these tables; keep the asserted preservation/deletion sets unchanged.

- [ ] **Step 2: Run the migration test and verify it fails**

Run:

```sh
cargo test -p ryuzi-core agentic_cleanup_removes_only_legacy_agent_data_and_preserves_history -- --nocapture
```

Expected: FAIL because the final migration does not yet remove the five settings rows and five legacy tables.

- [ ] **Step 3: Append the cleanup migration**

Append one `M::up_with_hook` to the end of `migrations()`; never insert it into an earlier migration slot:

```rust
M::up_with_hook("", |tx: &rusqlite::Transaction<'_>| {
    tx.execute_batch(
        "DELETE FROM settings WHERE key IN (
            'agent_model',
            'agent_perm_mode',
            'agent.max_provider_turns',
            'agent.auto_continue_budget',
            'memory.nudge_interval'
         );
         DELETE FROM background_events WHERE kind IN ('learning', 'orch');
         DROP TABLE IF EXISTS orch_task_deps;
         DROP TABLE IF EXISTS orch_tasks;
         DROP TABLE IF EXISTS skill_usage;
         DROP TABLE IF EXISTS curator_state;
         DROP TABLE IF EXISTS curator_runs;",
    )?;
    Ok(())
}),
```

Do not add `DELETE` statements for projects, sessions, messages, provider turns, providers/connections, model routes/preferences/status, jobs, plugins, audit, or agentic Plan 2/4 tables.

- [ ] **Step 4: Add replay and fresh-store tests**

Add:

```rust
#[tokio::test]
async fn agentic_cleanup_replay_does_not_remove_new_agentic_state() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Store::open(tmp.path()).await.unwrap();
    store.with_conn(|c| {
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        c.execute("INSERT OR REPLACE INTO settings(key,value) VALUES ('agentic.user.preference','keep')", [])?;
        c.pragma_update(None, "user_version", version - 1)
    }).await.unwrap();
    drop(store);
    let reopened = Store::open(tmp.path()).await.unwrap();
    assert_eq!(reopened.get_setting("agentic.user.preference").await.unwrap().as_deref(), Some("keep"));
}

#[tokio::test]
async fn fresh_store_has_no_legacy_agent_tables_or_settings() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Store::open(tmp.path()).await.unwrap();
    store.with_conn(|c| {
        for name in ["orch_tasks", "orch_task_deps", "skill_usage", "curator_state", "curator_runs"] {
            assert!(!c.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?.exists([name])?);
        }
        Ok(())
    }).await.unwrap();
}
```

- [ ] **Step 5: Run the focused and full migration tests**

Run:

```sh
cargo test -p ryuzi-core agentic_cleanup_ -- --nocapture
cargo test -p ryuzi-core store::tests::migrations_ -- --nocapture
```

Expected: PASS. The first command runs the preservation and replay cases; the second confirms every historical rewind/replay test lands at the new latest schema version. Update existing hard-coded `user_version` expectations and rewind offsets by exactly one where the appended tail requires it.

- [ ] **Step 6: Commit the SQLite cleanup boundary**

```sh
git add crates/core/src/store.rs
git commit -m "feat(core)!: remove legacy agent storage on upgrade"
```

Expected: one commit containing the appended migration and its tests; no filesystem bootstrap or UI changes.

---

### Task 2: Cross-Resource Crash-Order Compatibility Proof

**Files:**
- Create: `crates/core/tests/agentic_upgrade_compat.rs`
- Verify only: `crates/core/src/agents/bootstrap.rs` and Plan 2's existing bootstrap/transaction tests

**Interfaces:**
- Consumes: Task 1's atomic numbered SQL migration and Plan 2's committed `initialize_agent_registry`, `reset_agent_registry`, `AGENT_PERSISTENCE_MARKER`, transaction journal, injected failure points, and `BootstrapReason`.
- Produces: integration proof that the independently idempotent SQL and filesystem cleanup paths converge in either commit order. It produces no new bootstrap type, marker, phase, notice, or deletion algorithm.

- [ ] **Step 1: Write the two crash-order integration tests**

Use Plan 2's public/test bootstrap harness and existing failure injection; do not create journal files by hand and do not modify `bootstrap.rs`. Both tests seed the legacy SQL rows from Task 1, old `<config-root>/memory`, old agent files, and unrelated project/provider/session/plugin sentinels.

1. `sql_cleanup_committed_before_plan2_journal_recovery_converges`: open the store so Task 1 commits; invoke Plan 2 bootstrap with its existing pre-journal-commit failure point; assert SQL tables/settings are already absent while legacy files remain staged/recoverable and `agent_persistence_schema` is absent; restart without failure injection and assert Plan 2 completes one Ryuzi bundle, removes old files, preserves all sentinels, and writes marker `1`.
2. `plan2_journal_committed_before_sql_cleanup_converges`: run Plan 2 bootstrap against the pre-final-migration database using its test store constructor, assert one Ryuzi bundle and marker `1`; then open through normal `Store::open` so Task 1 commits, and assert legacy SQL disappears while the Ryuzi bundle and sentinels remain byte-for-byte unchanged.

After each converged state, create `agents/reviewer/agent.yaml` plus one OKF concept and insert a current non-legacy setting, reopen Store and agent persistence twice, and assert all three survive. This proves each completion marker gates only its own cleanup and neither path replays destructively after commit.

- [ ] **Step 2: Run the crash-order and Plan 2 owner tests**

```sh
cargo test -p ryuzi-core --test agentic_upgrade_compat sql_cleanup_committed_before_plan2_journal_recovery_converges -- --exact --nocapture
cargo test -p ryuzi-core --test agentic_upgrade_compat plan2_journal_committed_before_sql_cleanup_converges -- --exact --nocapture
cargo test -p ryuzi-core agents::bootstrap -- --nocapture
cargo test -p ryuzi-core agents::transaction -- --nocapture
```

Expected: the new integration tests pass after Task 1; all Plan 2 tests remain unchanged and pass. A failure must be fixed in Task 1's SQL idempotence or in the integration fixture—Plan 6 must not add a second filesystem path or alter Plan 2's journal.

- [ ] **Step 3: Commit only the cross-resource tests**

```sh
git add crates/core/tests/agentic_upgrade_compat.rs
git commit -m "test(core): prove agentic cleanup crash convergence"
```

Expected: one test-only commit; `git diff -- crates/core/src/agents` has no output.

---

### Task 3: Public Upgrade Compatibility and Read-Only Historical Sessions

**Files:**
- Modify: `crates/core/tests/agentic_upgrade_compat.rs` (created by Task 2)
- Modify: `crates/core/src/api/sessions.rs`
- Modify: `crates/core/src/control/lifecycle.rs`
- Modify: `crates/core/src/api/session_io_api.rs`

**Interfaces:**
- Consumes: Tasks 1–2 cleanup, Plan 3 registry API, and Plan 4 `resolve_session_agent_access` plus immutable identity snapshots.
- Produces: integration-level proof that historical data survives, legacy and deleted owners are readable/exportable, and every execution/mutation path returns the same read-only conflict.

- [ ] **Step 1: Write the failing upgrade compatibility integration test**

Create a public-API test that seeds a pre-feature database with one project, provider connection, session, message, and provider turn; seeds old memory; boots the daemon/registry through the Plan 2 test constructor; then asserts:

```rust
assert_eq!(store.list_projects().await?.iter().map(|p| p.project_id.as_str()).collect::<Vec<_>>(), ["p-keep"]);
assert!(store.get_provider_connection("provider-keep").await?.is_some());
let legacy = store.get_session("legacy-s").await?.unwrap();
assert_eq!(legacy.primary_agent_id, None);
assert_eq!(legacy.primary_agent_snapshot, None);
assert_eq!(store.list_messages("legacy-s").await?.len(), 1);
assert_eq!(store.list_provider_turns("legacy-s").await?.len(), 1);
assert!(matches!(resolve_session_agent_access(&store, &registry, "legacy-s").await?, SessionAgentAccess::LegacyReadOnly));
assert_eq!(registry.snapshot().await.agents[0].profile.name, "Ryuzi");
```

Use the committed Plan 4 field names and public test constructors. The test must not query private migration helpers.

- [ ] **Step 2: Write table-driven API tests for every blocked operation**

For `legacy-s` and a session whose owner has been deleted after creation, call the command/RPC layer for:

```text
continue_session
steer_session
stop_session
end_session
update_session_runtime
update_session_perm_mode
retry_child_run
cancel_child_run
```

Assert every operation returns conflict code `readOnlySession` with message `Legacy agent history is read-only.` or `{Snapshot Name} was deleted; this history is read-only.` respectively. Also assert `list_sessions`, `list_messages`, `get_child_runs`, `get_child_transcript`, and `export_session` succeed.

- [ ] **Step 3: Run tests and verify at least one mutation leaks through**

Run:

```sh
cargo test -p ryuzi-core --test agentic_upgrade_compat -- --nocapture
```

Expected: FAIL on any lifecycle/API path that has not yet delegated to Plan 4's centralized access guard; no failure may be fixed by special-casing `primary_agent_id.is_none()` at each call site.

- [ ] **Step 4: Apply the centralized guard to all mutation/run entry points**

Add one helper next to Plan 4's ownership resolver:

```rust
pub async fn require_executable_session_agent(
    store: &Store,
    registry: &AgentRegistry,
    session_pk: &str,
) -> Result<String, SessionAccessError> {
    match resolve_session_agent_access(store, registry, session_pk).await? {
        SessionAgentAccess::Executable { agent_id } => Ok(agent_id),
        SessionAgentAccess::LegacyReadOnly => Err(SessionAccessError::ReadOnly(
            "Legacy agent history is read-only.".into(),
        )),
        SessionAgentAccess::DeletedReadOnly { snapshot } => Err(SessionAccessError::ReadOnly(
            format!("{} was deleted; this history is read-only.", snapshot.name),
        )),
    }
}
```

Call it before any state transition or harness invocation in the eight operations. Keep list/read/transcript/export paths on `resolve_session_agent_access` so they can return identity labels without requiring an executable profile.

- [ ] **Step 5: Run ownership, lifecycle, import/export, and integration tests**

Run:

```sh
cargo test -p ryuzi-core sessions::ownership -- --nocapture
cargo test -p ryuzi-core control::lifecycle -- --nocapture
cargo test -p ryuzi-core session_io -- --nocapture
cargo test -p ryuzi-core --test agentic_upgrade_compat -- --nocapture
```

Expected: PASS. Import/export retains nullable legacy ownership and deleted-owner snapshots; no test assigns Ryuzi to old rows.

- [ ] **Step 6: Commit the compatibility guard**

```sh
git add crates/core/tests/agentic_upgrade_compat.rs crates/core/src/api/sessions.rs crates/core/src/control/lifecycle.rs crates/core/src/api/session_io_api.rs crates/core/src/sessions/ownership.rs
git commit -m "fix(core): keep historical agent sessions read only"
```

Expected: one integration/guard commit with no UI changes.

---

### Task 4: Delete Obsolete Core Settings, Memory, Learning, and Orchestration Code

**Files:**
- Delete if present: `crates/core/src/agent_settings.rs`
- Delete if present: `crates/core/src/orch.rs`
- Delete if present: `crates/core/src/api/orch_api.rs`
- Delete if present: `crates/core/src/harness/native/tools/app_orchestrate.rs`
- Delete if present: `crates/core/src/harness/native/tools/orch_block.rs`
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/api/mod.rs`
- Modify: `crates/core/src/api/types.rs`
- Modify: `crates/core/src/domain.rs`
- Modify: `crates/core/src/harness/native/tools/mod.rs`
- Modify: `crates/core/src/harness/native/agents.rs`
- Modify: `crates/core/src/daemon.rs`
- Modify: `crates/core/src/store.rs`

**Interfaces:**
- Consumes: Plan 2 OKF-backed memory/Learning tool implementation, Plan 3 agent management API, Plan 4 unified child-run/delegation API, and Tasks 1–3 compatibility behavior.
- Produces: a core crate with no callable global agent-settings API, old freeform memory API, curator API, orchestration DAG API/runtime/tool, or old SQL accessor; current `task` subagents and main-agent delegation remain.

- [ ] **Step 1: Capture concrete pre-deletion references and establish the expected removal set**

Run:

```sh
rg -n 'agent_settings|get_agent_settings|set_agent_settings|read_memory|write_memory|learning_graph|curator_(status|rollback)|orch_(submit|list_roots|tasks|cancel|retry|answer_block|steer)|app_orchestrate|orch_block|OrchTask|spawn_runner\(Arc::clone\(&cp\)\)' crates/core/src
```

Expected before cleanup: any survivors are exclusively the old modules/registrations/tests listed in this task. References inside Plan 2 per-agent OKF code use `AgentKnowledgeStore`, `KnowledgeConcept`, and `LearningQueue`, not the old command names.

- [ ] **Step 2: Write a compile-time API absence test before deletion**

Add an API dispatch test in `crates/core/src/api/mod.rs`:

```rust
#[tokio::test]
async fn removed_agent_commands_are_not_dispatched() {
    let state = tests_support::state().await;
    for method in [
        "get_agent_settings", "set_agent_settings", "read_memory", "write_memory",
        "learning_graph", "curator_status", "curator_rollback", "orch_submit",
        "orch_list_roots", "orch_tasks", "orch_cancel", "orch_retry",
        "orch_answer_block", "orch_steer",
    ] {
        let error = dispatch(&state, method, serde_json::json!({})).await.unwrap_err();
        assert_eq!(error.code(), "notFound", "removed method still dispatched: {method}");
    }
}
```

- [ ] **Step 3: Run the test and verify old commands still dispatch**

Run:

```sh
cargo test -p ryuzi-core removed_agent_commands_are_not_dispatched -- --nocapture
```

Expected: FAIL for at least one registered legacy method.

- [ ] **Step 4: Delete old modules and registrations**

Remove old module exports from `lib.rs` and `api/mod.rs`; remove old `HANDLES` branches and DTOs; remove old orchestration tool modules, `APP_TOOLS` entries, tool registrations, `AppToolBackend` methods, built-in orchestrator persona, `OrchTask` exports, event variants, and daemon dispatcher handle. Remove old store methods for orchestration DAG, global skill usage/curator state, and old Learning queue only after `rg` proves Plans 2/4 do not call them.

Do not delete:

```text
crates/core/src/harness/native/tools/task.rs
Plan 4 main-agent delegation tool/module
Plan 4 child-run/delegation provenance store methods
Plan 2 AgentKnowledgeStore or OKF-backed memory tool
Plan 2 durable Learning delivery queue
background_events table or non-learning/non-orch events
```

- [ ] **Step 5: Rename only stale comments/copy that describe current code as orchestration**

Use **delegation** for Plan 4 child runs and **Learning** for per-agent OKF. Do not erase historical changelog entries. Remove system prompts that advertise an `orchestrator` subagent, `app_orchestrate`, or `orch_block`; the Plan 4 delegation tool's own prompt remains.

- [ ] **Step 6: Verify core absence and behavior**

Run:

```sh
cargo test -p ryuzi-core removed_agent_commands_are_not_dispatched -- --nocapture
cargo test -p ryuzi-core agents -- --nocapture
cargo test -p ryuzi-core delegation -- --nocapture
cargo test -p ryuzi-core
rg -n 'get_agent_settings|set_agent_settings|read_memory|write_memory|learning_graph|curator_(status|rollback)|orch_(submit|list_roots|tasks|cancel|retry|answer_block|steer)|app_orchestrate|orch_block|OrchTask' crates/core/src --glob '!store.rs' --glob '!api/mod.rs'
```

Expected: all tests PASS; final `rg` exits 1 with no output. `rg -n 'AgentKnowledgeStore|KnowledgeConcept|LearningQueue|delegate' crates/core/src/agents crates/core/src/harness` still finds current implementations.

- [ ] **Step 7: Commit core code deletion**

```sh
git add -A crates/core/src
git commit -m "refactor(core)!: delete legacy agent runtime paths"
```

Expected: one deletion-focused commit; `git show --stat --oneline HEAD` shows net line removal outside current agent/delegation modules.

---

### Task 5: Delete Obsolete Tauri/Cockpit Surfaces and Regenerate Bindings

**Files:**
- Delete if present: `apps/cockpit/src-tauri/src/learning_cmd.rs`
- Modify: `apps/cockpit/src-tauri/src/lib.rs`
- Modify: `apps/cockpit/src-tauri/src/agent_cmd.rs`
- Delete if present: `apps/cockpit/src/store-agent.ts`
- Delete if present: `apps/cockpit/src/store-agent.test.ts`
- Delete if present: `apps/cockpit/src/store-orch.test.ts`
- Delete if present: `apps/cockpit/src/views/LearningView.tsx`
- Delete if present: `apps/cockpit/src/components/session/TaskStrip.tsx`
- Modify: `apps/cockpit/src/App.tsx`
- Modify: `apps/cockpit/src/store.ts`
- Modify: `apps/cockpit/src/store-nav.ts`
- Modify: `apps/cockpit/src/components/shell/Sidebar.tsx`
- Modify: `apps/cockpit/src/views/HomeView.tsx`
- Modify: `apps/cockpit/src/views/SessionView.tsx`
- Modify: `apps/cockpit/src/bindings.ts` (generated only)
- Test: nearby `*.test.ts` and `*.test.tsx` files for each modified module

**Interfaces:**
- Consumes: Plan 3 `useAgents`/agent detail Learning store, Plan 4 primary-agent picker/mentions/child roster/read-only session UI, and Plan 5 route editor.
- Produces: generated command bindings and Cockpit navigation/state with no global Learning, chat-level model/effort/permission/orchestration, or old task-strip flow.

- [ ] **Step 1: Add failing UI absence assertions**

Extend `Sidebar.test.tsx`, `HomeView.test.tsx`, `SessionView.test.tsx`, and `SettingsView.test.tsx` with exact negative assertions:

```tsx
expect(screen.queryByText("Learning", { selector: "nav *" })).toBeNull();
expect(screen.queryByRole("button", { name: /Orchestrate/i })).toBeNull();
expect(screen.queryByRole("button", { name: "Model and effort" })).toBeNull();
expect(screen.queryByRole("combobox", { name: /permission/i })).toBeNull();
expect(screen.queryByText("Default model")).toBeNull();
expect(screen.queryByText("Max provider turns")).toBeNull();
expect(screen.queryByText("Auto-continues")).toBeNull();
```

Positive assertions must still find **Agents**, the New session primary-agent combobox, the running-session primary-agent identity, mention autocomplete, and Active/Done child-run tabs.

- [ ] **Step 2: Run focused tests and verify stale UI/state remains**

Run:

```sh
bun test apps/cockpit/src/components/shell/Sidebar.test.tsx apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx apps/cockpit/src/views/SettingsView.test.tsx
```

Expected: FAIL on any surviving old control/import/navigation branch.

- [ ] **Step 3: Remove obsolete Tauri commands and frontend state**

Delete old global Learning proxies and remove these command registrations if Plans 3–4 have not already done so:

```text
get_agent_settings, set_agent_settings
read_memory, write_memory, learning_graph, curator_status, curator_rollback
orch_submit, orch_list_roots, orch_tasks, orch_cancel, orch_retry, orch_answer_block, orch_steer
```

Keep Plan 3 commands such as `list_agents`, `get_agent`, `get_agent_learning`, and knowledge mutations; keep Plan 4 child-run/delegation commands; keep `list_selectable_models` only if a current Agents or Models consumer uses it.

Delete old Zustand slices (`orchTasks`, `loadOrchTasks`, `startOrchestration`, `orchAnswerBlock`) and CoreEvent branches (`orchTaskChanged`). Delete `NavTarget.kind === "learning"`, the global Learning route, old Settings agent section/loop card, `TaskStrip`, and obsolete imports/tests. Do not replace removed controls with disabled placeholders.

- [ ] **Step 4: Regenerate bindings and inspect the generated diff**

Run:

```sh
cargo gen-bindings
git diff -- apps/cockpit/src/bindings.ts
```

Expected: generation succeeds; the diff removes old command functions/types and retains Plan 3 agent CRUD/Learning, Plan 4 session/delegation, and Plan 5 route-target `effort: string | null`. There are no manual formatting-only edits outside generator output.

- [ ] **Step 5: Run frontend and Tauri contract checks**

Run:

```sh
cargo test -p ryuzi-cockpit
bun test apps/cockpit/src/components/shell/Sidebar.test.tsx apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx apps/cockpit/src/views/SettingsView.test.tsx apps/cockpit/src/store-agents.test.ts
bun run typecheck
bun run --cwd apps/cockpit build
```

Expected: PASS. TypeScript has no references to removed generated commands or DTOs; Vite emits a production frontend bundle.

- [ ] **Step 6: Run concrete source and generated-binding absence checks**

Run:

```sh
rg -n 'getAgentSettings|setAgentSettings|readMemory|writeMemory|learningGraph|curator(Status|Rollback)|orch(Submit|ListRoots|Tasks|Cancel|Retry|AnswerBlock|Steer)' apps/cockpit/src apps/cockpit/src-tauri/src
rg -n 'kind: "learning"|LearningView|TaskStrip|Orchestrate|/orchestrate' apps/cockpit/src
```

Expected: both commands exit 1 with no output. Per-agent `AgentLearningTab` and `getAgentLearning` are not matched and remain present.

- [ ] **Step 7: Commit UI cleanup and generated bindings**

```sh
git add -A apps/cockpit/src apps/cockpit/src-tauri/src
git commit -m "refactor(cockpit)!: remove legacy agent controls"
```

Expected: one commit containing Tauri/frontend deletion, adjusted tests, and generator-produced bindings.

---

### Task 6: Permanent Repository Absence Guard and CI Wiring

**Files:**
- Create: `scripts/check-agentic-cleanup.ts`
- Modify: `package.json`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Tasks 4–5 deletion set and Bun runtime conventions.
- Produces: deterministic local/CI checks that fail if an obsolete file, command, schema artifact, settings key, or product label returns.

- [ ] **Step 1: Write the checker with exact path and regex contracts**

Create `scripts/check-agentic-cleanup.ts`:

```ts
import { existsSync } from "node:fs";
import { Glob } from "bun";

const forbiddenFiles = [
  "crates/core/src/agent_settings.rs",
  "crates/core/src/orch.rs",
  "crates/core/src/api/orch_api.rs",
  "crates/core/src/harness/native/tools/app_orchestrate.rs",
  "crates/core/src/harness/native/tools/orch_block.rs",
  "apps/cockpit/src-tauri/src/learning_cmd.rs",
  "apps/cockpit/src/store-agent.ts",
  "apps/cockpit/src/store-agent.test.ts",
  "apps/cockpit/src/store-orch.test.ts",
  "apps/cockpit/src/views/LearningView.tsx",
  "apps/cockpit/src/components/session/TaskStrip.tsx",
];

const scans = [
  {
    glob: "crates/core/src/**/*.{rs,sql}",
    pattern: /\b(get_agent_settings|set_agent_settings|read_memory|write_memory|learning_graph|curator_status|curator_rollback|orch_submit|orch_list_roots|orch_tasks|orch_cancel|orch_retry|orch_answer_block|orch_steer|app_orchestrate|orch_block)\b/,
  },
  {
    glob: "apps/cockpit/src/**/*.{ts,tsx}",
    pattern: /\b(getAgentSettings|setAgentSettings|readMemory|writeMemory|learningGraph|curatorStatus|curatorRollback|orchSubmit|orchListRoots|orchTasks|orchCancel|orchRetry|orchAnswerBlock|orchSteer|LearningView|TaskStrip)\b/,
  },
  {
    glob: "apps/cockpit/src-tauri/src/**/*.rs",
    pattern: /\b(get_agent_settings|set_agent_settings|read_memory|write_memory|learning_graph|curator_status|curator_rollback|orch_submit|orch_list_roots|orch_tasks|orch_cancel|orch_retry|orch_answer_block|orch_steer)\b/,
  },
];

const allowedHistoricalReferences = new Set([
  // Migration assertions and the dispatcher-negative test must name removed wire/storage contracts.
  "crates/core/src/store.rs",
  "crates/core/src/api/mod.rs",
]);

const failures: string[] = forbiddenFiles.filter(existsSync).map((path) => `obsolete file exists: ${path}`);
const forbiddenStoreAccessors = /\b(record_skill_use|record_skill_view|record_skill_patch|set_skill_state|set_skill_pinned|get_skill_usage|list_skill_usage|insert_curator_run|finish_curator_run|list_curator_runs|create_orch_task|list_orch_tasks|get_orch_task|update_orch_task|delete_orch_task)\b/;
const storeSource = await Bun.file("crates/core/src/store.rs").text();
if (forbiddenStoreAccessors.test(storeSource)) {
  failures.push(`obsolete live Store accessor in crates/core/src/store.rs: ${forbiddenStoreAccessors.source}`);
}


for (const scan of scans) {
  for await (const path of new Glob(scan.glob).scan({ cwd: ".", onlyFiles: true })) {
    if (allowedHistoricalReferences.has(path.replaceAll("\\", "/"))) continue;
    const text = await Bun.file(path).text();
    if (scan.pattern.test(text)) failures.push(`obsolete symbol in ${path}: ${scan.pattern.source}`);
  }
}

if (failures.length > 0) {
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log("agentic cleanup absence checks: PASS");
```

The checker intentionally excludes docs and changelogs, where historical migration language is valid. `store.rs` is exempt only from the broad historical-name scan so migration SQL/tests may name dropped tables; the separate `forbiddenStoreAccessors` scan still rejects live methods for removed `skill_usage`, curator, and orchestration storage. Keep `store-agent.test.ts` in `forbiddenFiles` so the obsolete test cannot survive after its store is deleted.

- [ ] **Step 2: Run the checker before cleanup is complete and verify it catches survivors**

Run:

```sh
bun scripts/check-agentic-cleanup.ts
```

Expected before Tasks 4–5 are complete: exit 1 with explicit obsolete paths/symbols. Expected after Tasks 4–5: `agentic cleanup absence checks: PASS`.

- [ ] **Step 3: Add the package script and CI step**

Add to root `package.json` scripts:

```json
"check:agentic-cleanup": "bun scripts/check-agentic-cleanup.ts"
```

Add immediately after `shellcheck install.sh` in `.github/workflows/ci.yml`'s `lint` job:

```yaml
      - run: bun run check:agentic-cleanup
```

Because `.github/**` already activates every path filter and the lint job is unconditional, no filter changes are needed.

- [ ] **Step 4: Verify formatting and CI-local behavior**

Run:

```sh
bun run check:agentic-cleanup
bunx biome check scripts/check-agentic-cleanup.ts package.json
# Review the YAML edit with git diff; Biome is not invoked on .github/workflows/ci.yml.
git diff --check -- .github/workflows/ci.yml
git diff -- .github/workflows/ci.yml
```

Expected: checker and Biome PASS; `git diff --check` reports no whitespace errors, and manual diff review shows only the intended CI run step.

- [ ] **Step 5: Commit the permanent guard**

```sh
git add scripts/check-agentic-cleanup.ts package.json .github/workflows/ci.yml
git commit -m "test: guard removed agentic compatibility paths"
```

Expected: one tooling/CI commit; no lockfile change because no dependency was added.

---

### Task 7: Integrated Mock-IPC Playwright Journeys

**Files:**
- Modify: `apps/cockpit/e2e/mock-ipc.ts`
- Modify: `apps/cockpit/e2e/app.e2e.ts`

**Interfaces:**
- Consumes: generated Plans 3–5 DTOs and commands, Task 5 cleaned UI, and Playwright's existing `installMockIPC`/`mockCalls` fixture pattern.
- Produces: browser-level acceptance coverage for agent management, session ownership, delegation/child transcripts, route effort, and legacy/deleted read-only history without a live Tauri backend.

- [ ] **Step 1: Add generated-type-checked agent/session fixtures**

Import generated DTO types and define:

```ts
export const RYUZI_AGENT = {
  id: "ryuzi",
  name: "Ryuzi",
  description: "General-purpose coding agent",
  avatarColor: "violet",
  model: { kind: "concrete", name: "fixture/model-alpha", effort: "high" },
  permissionMode: "ask",
  skillCount: 1,
  toolCount: 4,
  knowledgeCount: 1,
  executable: true,
  validation: [],
  isDefault: true,
} satisfies AgentSummaryInfo;

export const REVIEWER_AGENT = {
  ...RYUZI_AGENT,
  id: "reviewer",
  name: "Reviewer",
  description: "Reviews implementation quality and regressions",
  avatarColor: "amber",
  model: { kind: "route", route: "safe" },
  isDefault: false,
} satisfies AgentSummaryInfo;
```

Add registry/detail/Learning fixtures and sessions typed with the generated existing `Session` DTO. The owned active session sets `primaryAgentId: "ryuzi"` and `primaryAgentSnapshot: { id: "ryuzi", name: "Ryuzi", avatarColor: "violet" }`; the legacy session sets both fields to `null`; the deleted-owner session keeps `primaryAgentId: "reviewer"` and `primaryAgentSnapshot: { id: "reviewer", name: "Reviewer", avatarColor: "amber" }`. Derive **Legacy agent** and **Deleted** only through Plan 4's access/display logic—do not add fixture-only `agent`, `displayIdentity`, or deletion fields. Also add one active delegate child run, one done subagent child run, and child transcript rows.

- [ ] **Step 2: Extend mock command state transitions**

Return fixtures for Plan 3/4 commands and mutate in-memory fixture state for create/update/duplicate/delete, session start, mention send, child-run status, and transcript navigation. Record calls through existing `mockCalls`. Removed commands must not be added to `FIXTURES`; unknown invocations of old names should throw so accidental UI calls fail tests immediately.

- [ ] **Step 3: Write the full agent management and start-chat journey**

Add a Playwright test that:

1. Opens **Agents** and sees **Main Agent**/**Sub Agent**.
2. Opens Reviewer detail and sees Overview, Model, Permissions, Skills & Tools, Learning, Advanced.
3. Uses only the action menu to select **Start chat**.
4. Verifies New session primary-agent combobox contains Reviewer and no model/effort/permission/Orchestrate control.
5. Sends a prompt and asserts `start_chat_session` carries `primaryAgentId: "reviewer"`.

Expected UI strings and command argument assertions must be exact; do not use snapshots.

- [ ] **Step 4: Write mention delegation and child transcript journey**

Start a Ryuzi session, type `@Rev`, keyboard-select Reviewer, send, and assert the structured mention payload contains `{ agentId: "reviewer", labelSnapshot: "Reviewer", startUtf16: 0, endUtf16: 9 }` exactly once. Open the right panel, verify **Active** and **Done**, distinguish **Main agent** from **Subagent**, open Reviewer, assert its full transcript appears, press Back, and assert the roster returns without losing the parent transcript.

- [ ] **Step 5: Write legacy and deleted-owner history journeys**

Open `Legacy agent` history and assert transcript text is visible while composer, retry, cancel, permission, model, and effort controls are absent. Open deleted Reviewer history and assert `Reviewer`, `Deleted`, and historical transcript text are visible while the same mutating controls are absent. Assert neither navigation causes `continue_session`, `steer_session`, `retry_child_run`, or `cancel_child_run` calls.

- [ ] **Step 6: Update route-effort e2e coverage to the Plan 5 contract**

In the existing Models route editor journey, select a concrete target supporting `high`, save, reopen, and assert target effort remains `High`; switch to a model with no effort support and assert the effort picker disappears and the saved payload contains `effort: null`. Assert no model value includes `-high`, `-medium`, or another virtual suffix.

- [ ] **Step 7: Run targeted and complete Playwright suites**

Run:

```sh
bun run --cwd apps/cockpit build
bun run --cwd apps/cockpit e2e:ci --grep "agents:|delegation:|history:|route effort:"
bun run --cwd apps/cockpit e2e:ci
```

Expected: all commands PASS. The focused command runs the four new named journeys; the complete suite retains project/provider/account/session coverage.

- [ ] **Step 8: Commit integrated e2e coverage**

```sh
git add apps/cockpit/e2e/mock-ipc.ts apps/cockpit/e2e/app.e2e.ts
git commit -m "test(cockpit): cover integrated agentic journeys"
```

Expected: one e2e-only commit using generated types for fixture drift detection.

---

### Task 8: Upgrade Documentation and Changelogs

**Files:**
- Modify: `README.md`
- Modify: `docs/development/setup.md`
- Modify: `docs/development/plugins.md`
- Modify: `crates/core/CHANGELOG.md`
- Modify: `apps/cockpit/src-tauri/CHANGELOG.md`
- Modify: `crates/runner/CHANGELOG.md`

**Interfaces:**
- Consumes: completed user-visible behavior from Tasks 1–7 and the approved design.
- Produces: exact operator/developer upgrade guidance and release notes with no obsolete product instructions.

- [ ] **Step 1: Replace README's single-agent/configuration description**

Document that Cockpit manages persistent main agents under the cross-platform Ryuzi config directory:

```text
agents/index.yaml
agents/subagents.yaml
agents/<agent-id>.yaml
agents/<agent-id>/knowledge/
```

State that YAML and per-agent OKF Markdown are portable and credential-free, while SQLite remains authoritative for projects, providers, sessions, transcripts, runs, queues, and provenance. Add Quick start steps: configure a provider, open Agents, repair/select Ryuzi, choose the primary agent on New session, and use `@AgentName` for explicit delegation.

- [ ] **Step 2: Add an explicit destructive upgrade warning**

Add this callout verbatim to README and `docs/development/setup.md`:

```markdown
> **Agent data reset on first upgrade:** The first launch of this agent schema permanently removes the previous global agent settings, freeform memory files, Learning/curator state, and orchestration DAG data, then creates one main agent named **Ryuzi**. Projects, provider accounts/routes, and historical sessions/transcripts are preserved. Pre-upgrade sessions appear as read-only **Legacy agent** history and are not assigned to Ryuzi.
```

Also state that later launches do not repeat cleanup and that an explicit agent-data reset is destructive by design.

- [ ] **Step 3: Rewrite obsolete setup/plugin architecture sections**

Remove instructions for the Orchestrate toggle, `/orchestrate`, `app_orchestrate`, `orch_block`, global `MEMORY.md`/`USER.md`, `memory.nudge_interval`, global Learning sidebar, and Settings agent defaults. Replace them with Plan 4 structured mentions/main-agent delegation, runtime-only memoryless subagents, per-agent OKF concepts, durable Learning delivery queue, child-run provenance, and right-panel Active/Done transcript navigation.

Retain plugin-category documentation where `memory` describes third-party plugin capabilities; that catalog vocabulary is not the deleted built-in global memory store.

- [ ] **Step 4: Add unreleased changelog entries**

At the top of all three changelogs add `## [Unreleased]`. Core must contain **Features** for YAML/OKF agents and unified delegation plus **Breaking Changes** for destructive first-upgrade removal and read-only legacy sessions. Cockpit must contain **Features** for Agents hub/detail, primary-agent selection, mentions, child transcripts, and route effort plus **Removed** for global Learning, Settings agent controls, composer model/effort/permission, and Orchestrate. Runner must contain an **Upgrade Notes** item stating startup performs the one-time cleanup while preserving projects/providers/history.

Do not change Cargo package versions, `.release-please-manifest.json`, or `tauri.conf.json`; release-please owns version stamping.

- [ ] **Step 5: Verify docs contain current terms and no live obsolete instructions**

Run:

```sh
rg -n 'Agent data reset on first upgrade|Legacy agent|agents/index.yaml|@AgentName|Active|Done' README.md docs/development/setup.md docs/development/plugins.md
rg -n 'Orchestrate toggle|/orchestrate|app_orchestrate|orch_block|memory\.nudge_interval|Settings.*Default model|top-level Learning' README.md docs/development/setup.md docs/development/plugins.md
```

Expected: first command finds all required concepts. Second command exits 1 with no output; historical changelogs are deliberately outside this absence scan.

- [ ] **Step 6: Commit docs and release notes**

```sh
git add README.md docs/development/setup.md docs/development/plugins.md crates/core/CHANGELOG.md apps/cockpit/src-tauri/CHANGELOG.md crates/runner/CHANGELOG.md
git commit -m "docs: explain agentic upgrade and legacy cleanup"
```

Expected: one documentation-only commit; no manifests or lockfiles changed.

---

### Task 9: Full CI, Release, Packaging, and Scope Verification

**Files:**
- Verify only: `.github/workflows/ci.yml`
- Verify only: `.github/workflows/release.yml`
- Verify only: `.github/workflows/cockpit-desktop.yml`
- Verify only: `release-please-config.json`
- Verify only: `.release-please-manifest.json`
- Verify only: `scripts/npm/`
- Verify only: `npm/`
- Verify all files changed in Tasks 1–8

**Interfaces:**
- Consumes: every previous task.
- Produces: formatted, lint-clean, test-clean, buildable, package-compatible Plan 6 with explicit proof of deletion and preservation; no implementation commit is required unless formatters change scoped files.

- [ ] **Step 1: Regenerate bindings once more and prove determinism**

Run:

```sh
cargo gen-bindings
git diff --exit-code -- apps/cockpit/src/bindings.ts
```

Expected: exit 0 and no diff. A diff means Task 5 committed stale generated bindings; regenerate, rerun affected frontend tests, and amend with a new `fix(cockpit): refresh agentic bindings` commit rather than hand-editing.

- [ ] **Step 2: Run formatters and formatting checks**

Run:

```sh
cargo fmt
bun run format
cargo fmt --check
bunx biome ci .
```

Expected: checks exit 0. Review formatter changes and keep only scoped mechanical edits.

- [ ] **Step 3: Run all Rust tests and strict lint**

Run:

```sh
cargo test -p ryuzi-core -p ryuzi-runner -p ryuzi-plugin-sdk -p ryuzi-cockpit
cargo clippy -p ryuzi-core -p ryuzi-runner -p ryuzi-plugin-sdk --all-targets -- -D warnings
```

Expected: PASS with zero failures and zero warnings. This includes destructive migration replay, bootstrap interruption, ownership compatibility, delegation, route effort, API absence, runner startup, and Tauri export tests.

- [ ] **Step 4: Run all TypeScript tests, types, cleanup guard, and frontend build**

Run:

```sh
bun run typecheck
bun test
bun run check:agentic-cleanup
bun run --cwd apps/cockpit build
```

Expected: PASS; cleanup guard prints `agentic cleanup absence checks: PASS`.

- [ ] **Step 5: Run the complete Cockpit e2e suite**

Run:

```sh
bunx playwright install chromium
bun run --cwd apps/cockpit e2e:ci
```

Expected: PASS with no failed tests; Playwright report is not generated as a failure artifact.

- [ ] **Step 6: Run the runner release smoke path**

Run on Unix-like shells exactly as CI does:

```sh
cargo build -p ryuzi-runner
./target/debug/ryuzi --version
./target/debug/ryuzi --help
```

On Windows PowerShell use:

```powershell
cargo build -p ryuzi-runner
.\target\debug\ryuzi.exe --version
.\target\debug\ryuzi.exe --help
```

Expected: build exits 0; version prints one semantic version line; help lists `setup`, `start`, `status`, `service`, `doctor`, and `config` and does not expose an interactive CLI/TUI or legacy orchestration command.

- [ ] **Step 7: Inspect CI/release/package contracts without changing them**

Run:

```sh
git diff -- .github/workflows/release.yml .github/workflows/cockpit-desktop.yml release-please-config.json .release-please-manifest.json scripts/npm npm Cargo.lock bun.lock
rg -n 'crates/runner|apps/cockpit/src-tauri|crates/core|cargo-workspace' release-please-config.json .release-please-manifest.json
rg -n -- '--version|--help|ryuzi-' .github/workflows/release.yml .github/workflows/cockpit-desktop.yml scripts/npm npm
```

Expected: first command has no output except the intentional `.github/workflows/ci.yml` change is outside this list; manifests, lockfiles, release workflows, npm launcher, and packaging scripts are unchanged. Remaining commands show all three release-please packages and existing runner/Cockpit artifact smoke contracts.

- [ ] **Step 8: Prove preservation and absence directly**

Run:

```sh
cargo test -p ryuzi-core agentic_cleanup_removes_only_legacy_agent_data_and_preserves_history -- --nocapture
cargo test -p ryuzi-core --test agentic_upgrade_compat -- --nocapture
bun run check:agentic-cleanup
rg -n 'primary_agent_id|primary_agent_snapshot|LegacyReadOnly|DeletedReadOnly' crates/core/src crates/core/tests
rg -n 'ModelRouteTarget|effort: Option<String>' crates/core/src/llm_router/routes.rs
```

Expected: tests/checker PASS; source searches find nullable ownership/snapshots and Plan 5 effort fields. No output from the checker identifies an obsolete path.

- [ ] **Step 9: Review the implementation commit/file boundary and confirm no plan commit**

Run:

```sh
git status --short
git log --oneline --decorate -9
git diff main...HEAD --name-status
git diff --check
```

Expected: implementation history contains the eight task commits (plus a formatter/binding fix only if needed); `git diff --check` exits 0. The tracked roadmap plan at `docs/roadmap/plan/2026-07-12-agentic-06-integration-cleanup.md` remains unchanged and is not included in the implementation commits. Preserve any unrelated pre-existing worktree entries and report them separately.

- [ ] **Step 10: Commit formatter-only changes if and only if verification produced them**

First inspect:

```sh
git status --short
git diff --check
```

If scoped source/docs files changed only through `cargo fmt`/Biome, commit exactly those files:

```sh
git add crates/core apps/cockpit scripts package.json .github/workflows/ci.yml README.md docs/development
git commit -m "style: format agentic integration cleanup"
```

Expected: no commit when the worktree was already formatted. Never add `docs/roadmap/plan/2026-07-12-agentic-06-integration-cleanup.md`, unrelated files, lockfiles, release manifests, or release workflows to this optional commit.
