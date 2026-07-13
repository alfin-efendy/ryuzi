# Agentic Session Ownership and Delegation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lock each new session to one persistent main agent, replace chat-level runtime/orchestration controls with structured delegation, persist primary/main-delegate/subagent provenance, and expose live child transcripts in Cockpit.

**Architecture:** Plans 1–3 are hard prerequisites. Plan 4 adds a nullable immutable ownership snapshot to sessions, a SQLite run tree, and a `DelegationRuntime` shared by explicit mentions, autonomous main-agent delegation, and the existing runtime-only subagent tool. The control plane creates one `primary` provenance root for every user turn; Cockpit sends UTF-16 mention spans through generated bindings and reads child runs through the exact interfaces consumed by Plan 6. The legacy root-goal orchestration product is removed completely, while generic task delegation, the background rail's `Delegation` kind, cancellation, retry, and result delivery remain.

**Tech Stack:** Rust 2021, Tokio, rusqlite/rusqlite-migration, serde/specta, Tauri 2, React 19, TypeScript 5, Zustand 4, `@ryuzi/ui`, Lucide React, Bun test, Testing Library, Vite.

## Global Constraints

- Plans 1–3 must be implemented and green first. Consume Plan 2's `AgentRegistry`, `AgentSnapshot`, `AgentProfile`, `AgentModel`, and `SubagentConfig`, and Plan 3's `AgentSummaryInfo`/generated agent store. Do not invent `ResolvedAgentSnapshot`, `AgentKind`, `AgentModelSelection`, or a second agent DTO.
- YAML/OKF remain configuration and knowledge authority. SQLite stores only session references/identity snapshots, transcript, run audit/provenance, and operational state.
- The primary agent is selected before first send, persisted once, and immutable for the session. Delegation never changes ownership.
- Pre-feature sessions have `primary_agent_id = NULL` and `primary_agent_snapshot = NULL`, display **Legacy agent**, remain readable, and reject new turns. Deleted-owner sessions retain the snapshot, display **Deleted**, remain readable, and reject new turns.
- Remove model, effort, permission, Orchestrate, and special `/orchestrate` behavior from both composers. Project, branch/worktree, context, voice, and attachments remain.
- Mention fields are exactly `agent_id`, `label_snapshot`, `start_utf16`, and `end_utf16` in Rust (`agentId`, `labelSnapshot`, `startUtf16`, `endUtf16` in generated TypeScript). The backend trusts only `agent_id`; offsets use JavaScript UTF-16 code units end-to-end and are converted to Rust byte boundaries only during validation.
- Explicit mentions exclude the primary and unknown/invalid/non-executable agents, deduplicate by `agent_id`, dispatch all unique targets concurrently, remove only validated mention spans, and preserve every other code unit. The primary is coordinator-only for that turn: it does not execute the delegated task, waits for every child terminal result, and performs exactly one synthesis after all results/errors arrive.
- Autonomous delegation supports single or batch dispatch. Foreground calls await terminal results; background calls return queued run identities immediately and deliver terminal results/errors into the originating primary run without blocking unrelated primary work.
- Main-agent delegation depth is a fixed maximum of 4 main-agent edges. Every root primary run permits at most 8 concurrently active descendants across both main delegates and subagents. Per-agent loop settings do not override these constants.
- Runtime-only subagents retain existing built-in types/prompts, `SubagentSpawner`, bounded native tools, and unattended permission policy. They use Plan 2 `subagents.yaml` model selection; inherit project/worktree and explicitly passed task/context; and inherit no persistent memory, Apps/MCP grants, attachments, or full main-agent profile.
- Run kinds are exactly `primary`, `main-delegate`, and `subagent`. Run statuses are exactly `queued`, `running`, `completed`, `failed`, `cancelled`, and `interrupted`.
- Stopping a child cancels that run and descendants only. Retry is allowed only for `failed`, `cancelled`, or `interrupted`; creates a sibling attempt with the same parent and `retry_of`; resolves the latest valid target profile/config; and preserves the old attempt.
- Startup atomically changes every persisted `queued`/`running` run to `interrupted`, with `error = "Ryuzi restarted before this run completed."`; runs never resume automatically.
- Approval events and pending approval keys carry `run_id` and requesting-agent identity. Cards render in the parent session and a decision resolves only `(run_id, request_id)`.
- The right-panel Agents tab has Active/Done rosters. Selecting a card replaces the roster with a full child transcript and **Back to Agents** navigation.
- Never hand-edit `apps/cockpit/src/bindings.ts`; regenerate with the repository alias `cargo gen-bindings` whenever a Rust command/type surface changes.
- Use `@ryuzi/ui` primitives rather than raw interactive controls. Preserve right-panel dimensions/maximize behavior and unrelated worktree changes.
- Each task below ends with a compilable, independently green checkpoint. During implementation, make the listed task commits; this planning request itself must not be committed.

## Plans 1–3 Interfaces Consumed

```rust
// Plan 2: crates/core/src/agents/types.rs and registry.rs
pub struct AgentSnapshot {
    pub profile: AgentProfile,
    pub executable: bool,
    pub validation: Vec<AgentValidationIssue>,
}

pub struct SubagentConfig { pub schema_version: u32, pub model: AgentModel }

impl AgentRegistry {
    pub async fn snapshot(&self) -> AgentRegistrySnapshot;
    pub async fn get(&self, agent_id: &str) -> anyhow::Result<AgentSnapshot>;
    pub async fn resolved_snapshot(&self, agent_id: &str)
        -> anyhow::Result<Arc<AgentSnapshot>>;
}
```

`resolved_snapshot` is Plan 2's immutable cloned `Arc<AgentSnapshot>` and must reject a non-executable profile. A main delegate holds that complete snapshot for its lifetime. Do not persist that profile in SQLite.

```rust
// Plan 3: crates/core/src/api/types.rs
pub struct AgentSummaryInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub avatar_color: String,
    pub executable: bool,
    // remaining management fields omitted here only because Plan 4 does not consume them
}
```

## Exact Plan 6 Session-Ownership Interface Produced

Task 1 must create these exact public contracts; Plan 6 imports them directly and must not add an adapter:

```rust
// crates/core/src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdentitySnapshot {
    pub id: String,
    pub name: String,
    pub avatar_color: String,
}

// Added to Session:
pub primary_agent_id: Option<String>,
pub primary_agent_snapshot: Option<AgentIdentitySnapshot>,
```

```rust
// crates/core/src/sessions/ownership.rs
#[derive(Debug, Clone, PartialEq, Eq)]
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
```

Resolution is deterministic: missing session is an error; both ownership fields absent is legacy; exactly one absent is corrupt data and an error; an existing registry ID returns `Executable` (later execution still calls `resolved_snapshot` and may report current validation errors); a missing registry ID returns `DeletedReadOnly` with the persisted snapshot. Renames never alter the snapshot. There is no persisted `primary_agent_deleted` column and no delete-time scan: deletion is derived from stable-ID registry lookup, so a filesystem deletion and SQLite update cannot diverge.

Plan 6 also consumes these exact RPC names from Task 6: `get_child_runs`, `get_child_transcript`, `cancel_child_run`, and `retry_child_run` (generated as `getChildRuns`, `getChildTranscript`, `cancelChildRun`, and `retryChildRun`).

## File Structure

- Create `crates/core/src/sessions/mod.rs` and `crates/core/src/sessions/ownership.rs`: own the Plan 6 access contract.
- Modify `crates/core/src/domain.rs` and `crates/core/src/store.rs`: add immutable ownership, run types/table, run-message mapping, and transactional persistence methods.
- Create `crates/core/src/delegation.rs`: own run limits, lifecycle, execution dispatch, cancellation, retry, and restart recovery.
- Create `crates/core/src/mentions.rs`: validate UTF-16 spans and resolve trusted IDs.
- Modify `crates/core/src/control.rs`, `control/lifecycle.rs`, `daemon.rs`, `harness/mod.rs`, and native runtime files to create primary runs and execute complete target snapshots; update every `SessionCtx` literal returned by the documented `rg` search in Task 3.
- Create `crates/core/src/harness/native/tools/delegate.rs`: autonomous main-agent tool; retain and provenance-wrap `tools/task.rs` for ephemeral subagents.
- Create `crates/core/src/api/delegation_api.rs` and `apps/cockpit/src-tauri/src/delegation_cmd.rs`: exact Plan 6 child-run query/control surface.
- Create `apps/cockpit/src/lib/mentions.ts` and composer mention components: maintain UTF-16 spans with the text draft.
- Create `apps/cockpit/src/store-delegation.ts` and child roster/detail components: scope run state by runner/session/run.
- Delete the legacy orchestration modules/tools/UI; retain generic `BackgroundKind::Delegation`, background delivery, and existing `task` subagent machinery.

---

### Task 1: Session Ownership, Run Schema, and Plan 6 Access Contract

**Files:**
- Create: `crates/core/src/sessions/mod.rs`
- Create: `crates/core/src/sessions/ownership.rs`
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/domain.rs`
- Modify: `crates/core/src/store.rs`
- Modify: every Rust fixture returned by `rg -l "Session \\{" crates apps --glob '*.rs'`

**Interfaces:**
- Consumes: Plan 2 `AgentRegistry`/`AgentSnapshot`; existing `Store::with_conn`, `Store::insert_session`, `Store::insert_message`, `SESSION_COLS`, and `row_to_session`.
- Produces: the exact Plan 6 ownership interface above, `AgentRun`, run persistence methods, and no changes to `Message`/`NewMessage` literals.

- [ ] **Step 1: Add failing migration and ownership tests**

Add store tests that open a pre-migration database, assert old sessions deserialize with both ownership fields `None`, insert an owned session, reopen it, and verify the identity snapshot survives a registry rename. Add `sessions::ownership` tests for executable, legacy, deleted, mismatched-null, and missing-session cases.

Use this run fixture:

```rust
let root = NewAgentRun {
    run_id: "root".into(),
    session_pk: "owned".into(),
    parent_run_id: None,
    retry_of: None,
    primary_agent_id: "ada".into(),
    executing_agent_id: Some("ada".into()),
    executing_agent_name_snapshot: "Ada".into(),
    agent_kind: AgentRunKind::Primary,
    task: "ship it".into(),
    status: AgentRunStatus::Queued,
    resolved_model: Some("anthropic/claude-opus-4-8".into()),
    resolved_effort: Some("high".into()),
};
```

- [ ] **Step 2: Run tests to verify RED**

```sh
cargo test -p ryuzi-core agentic_session_migration_preserves_legacy_history_and_run_tree -- --nocapture
cargo test -p ryuzi-core sessions::ownership -- --nocapture
```

Expected: compilation fails because ownership/run contracts do not exist.

- [ ] **Step 3: Add exact domain types and database spellings**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRunKind { Primary, MainDelegate, Subagent }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AgentRunStatus { Queued, Running, Completed, Failed, Cancelled, Interrupted }

impl AgentRunKind {
    pub fn as_db(self) -> &'static str { /* primary | main-delegate | subagent */ }
    pub fn from_db(value: &str) -> rusqlite::Result<Self>;
}
impl AgentRunStatus {
    pub fn as_db(self) -> &'static str { /* exact lower-case status */ }
    pub fn from_db(value: &str) -> rusqlite::Result<Self>;
    pub fn is_active(self) -> bool;
    pub fn is_terminal(self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub run_id: String,
    pub session_pk: String,
    pub parent_run_id: Option<String>,
    pub retry_of: Option<String>,
    pub primary_agent_id: String,
    pub executing_agent_id: Option<String>,
    pub executing_agent_name_snapshot: String,
    pub agent_kind: AgentRunKind,
    pub task: String,
    pub status: AgentRunStatus,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub tool_count: u32,
    pub resolved_model: Option<String>,
    pub resolved_effort: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}
```

`NewAgentRun` contains the same fields through `resolved_effort` and omits store-assigned timestamps/count/result/error. Add `AgentIdentitySnapshot` and the two exact `Session` fields. Update `SESSION_COLS`, `row_to_session`, both session insert SQL statements, import/export conversion, and every `Session { ... }` literal in the `rg` output so this task compiles.

- [ ] **Step 4: Append one migration using repository conventions**

Append to the existing `Migrations::new(vec![...])`; do not edit historical migrations. Add nullable `primary_agent_id TEXT` and `primary_agent_snapshot TEXT` to `sessions`. Store the snapshot as JSON produced by `serde_json`; reject malformed JSON on read.

Create:

```sql
CREATE TABLE agent_runs (
  run_id TEXT PRIMARY KEY,
  session_pk TEXT NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,
  parent_run_id TEXT REFERENCES agent_runs(run_id),
  retry_of TEXT REFERENCES agent_runs(run_id),
  primary_agent_id TEXT NOT NULL,
  executing_agent_id TEXT,
  executing_agent_name_snapshot TEXT NOT NULL,
  agent_kind TEXT NOT NULL CHECK(agent_kind IN ('primary','main-delegate','subagent')),
  task TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('queued','running','completed','failed','cancelled','interrupted')),
  started_at INTEGER,
  finished_at INTEGER,
  tool_count INTEGER NOT NULL DEFAULT 0 CHECK(tool_count >= 0),
  resolved_model TEXT,
  resolved_effort TEXT,
  result TEXT,
  error TEXT
);
CREATE INDEX agent_runs_parent_idx ON agent_runs(session_pk,parent_run_id,started_at);
CREATE INDEX agent_runs_status_idx ON agent_runs(session_pk,status);
CREATE TABLE agent_run_messages (
  session_pk TEXT NOT NULL,
  message_seq INTEGER NOT NULL,
  run_id TEXT NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,
  PRIMARY KEY(session_pk,message_seq),
  FOREIGN KEY(session_pk,message_seq) REFERENCES messages(session_pk,seq) ON DELETE CASCADE
);
CREATE INDEX agent_run_messages_run_idx ON agent_run_messages(run_id,message_seq);
```

Use the mapping table instead of adding `run_id` to `Message`/`NewMessage`; this preserves all current message constructors while making child transcript ownership explicit.

- [ ] **Step 5: Implement transactional store methods through `Store::with_conn`**

```rust
pub async fn insert_owned_session_with_primary_run(
    &self,
    session: Session,
    identity: AgentIdentitySnapshot,
    run: NewAgentRun,
) -> anyhow::Result<AgentRun>;
pub async fn insert_agent_run(&self, run: NewAgentRun) -> anyhow::Result<AgentRun>;
pub async fn get_agent_run(&self, run_id: &str) -> anyhow::Result<Option<AgentRun>>;
pub async fn list_session_agent_runs(&self, session_pk: &str) -> anyhow::Result<Vec<AgentRun>>;
pub async fn list_descendant_agent_runs(&self, root_run_id: &str) -> anyhow::Result<Vec<AgentRun>>;
pub async fn transition_agent_run(
    &self,
    run_id: &str,
    allowed_from: &[AgentRunStatus],
    to: AgentRunStatus,
    result: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<bool>;
pub async fn increment_agent_run_tool_count(&self, run_id: &str) -> anyhow::Result<()>;
pub async fn interrupt_incomplete_agent_runs(&self, reason: &str) -> anyhow::Result<u64>;
pub async fn insert_run_message(&self, run_id: &str, message: NewMessage) -> anyhow::Result<i64>;
pub async fn list_run_messages(&self, session_pk: &str, run_id: &str) -> anyhow::Result<Vec<Message>>;
```

`insert_owned_session_with_primary_run` validates `session.session_pk == run.session_pk`, `run.kind == Primary`, no parent/retry, and matching IDs; inserts session ownership and root run in one SQLite transaction. `insert_agent_run` validates the parent exists in the same session/root and non-primary runs have a parent. `insert_run_message` verifies run/session equality, allocates the existing per-session message sequence, inserts the existing message row, and inserts its mapping in one transaction. `transition_agent_run` sets `started_at` only on first transition to `running`, `finished_at` only on terminal transition, and never permits leaving a terminal state.

- [ ] **Step 6: Implement and export `sessions::ownership`**

Implement the exact Plan 6 contract stated above. Tests must prove rename keeps the persisted display snapshot while deletion is detected by registry ID absence.

- [ ] **Step 7: Verify the checkpoint**

```sh
cargo fmt --check
cargo test -p ryuzi-core store::tests -- --nocapture
cargo test -p ryuzi-core sessions::ownership -- --nocapture
cargo check -p ryuzi-core
```

Expected: all pass; no `Message` or `NewMessage` field was added.

- [ ] **Step 8: Commit the persistence slice**

```sh
git add crates/core/src/domain.rs crates/core/src/store.rs crates/core/src/lib.rs crates/core/src/sessions
# Add only fixture files changed to initialize the two new Session fields.
git commit -m "feat(core): persist session ownership and agent runs"
```

---

### Task 2: Bounded Delegation Runtime and Restart Lifecycle

**Files:**
- Create: `crates/core/src/delegation.rs`
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/control.rs`
- Modify: `crates/core/src/daemon.rs`
- Modify: every constructor call returned by `rg -l "ControlPlane::new(_with_telemetry|_full)?\\(" crates apps --glob '*.rs'`

**Interfaces:**
- Consumes: Task 1 store methods; Plan 2 `AgentRegistry::resolved_snapshot` and `AgentSnapshot`.
- Produces: fixed limits, in-flight immutable snapshots, run state events, cancellation, terminal delivery, retry, and startup interruption.

- [ ] **Step 1: Write failing lifecycle/limit tests**

Test self-delegation, an ancestry cycle, fifth main-agent edge rejection, ninth concurrently active descendant rejection (mixed main/subagent), subtree-only cancellation, terminal-state immutability, retry using a changed latest profile, and restart interruption with the exact reason.

- [ ] **Step 2: Run tests to verify RED**

```sh
cargo test -p ryuzi-core delegation::tests -- --nocapture
```

Expected: module/types do not exist.

- [ ] **Step 3: Implement exact runtime boundary**

```rust
pub const MAX_MAIN_DELEGATION_DEPTH: usize = 4;
pub const MAX_ACTIVE_CHILD_RUNS: usize = 8;
pub const RESTART_INTERRUPTION_REASON: &str =
    "Ryuzi restarted before this run completed.";

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

impl DelegationRuntime {
    pub fn new(
        store: Arc<Store>,
        registry: Arc<AgentRegistry>,
        events: broadcast::Sender<CoreEvent>,
    ) -> Arc<Self>;
    pub async fn recover_after_restart(&self) -> anyhow::Result<u64>;
    pub async fn begin_primary(
        &self,
        session_pk: &str,
        snapshot: Arc<AgentSnapshot>,
        task: &str,
    ) -> anyhow::Result<RunHandle>;
    pub async fn activate_persisted_primary(
        &self,
        run: AgentRun,
        snapshot: Arc<AgentSnapshot>,
    ) -> anyhow::Result<RunHandle>;
    pub async fn queue_main(&self, request: MainDelegationRequest) -> anyhow::Result<RunHandle>;
    pub async fn queue_subagent(&self, request: SubagentRunRequest) -> anyhow::Result<RunHandle>;
    pub async fn mark_running(&self, run_id: &str) -> anyhow::Result<()>;
    pub async fn complete(&self, run_id: &str, result: &str) -> anyhow::Result<()>;
    pub async fn fail(&self, run_id: &str, error: &str) -> anyhow::Result<()>;
    pub async fn cancel_child(&self, session_pk: &str, run_id: &str) -> anyhow::Result<()>;
    pub async fn retry_child(&self, session_pk: &str, run_id: &str) -> anyhow::Result<AgentRun>;
}
```

The live map is `Mutex<HashMap<String, InFlightRun>>`; it holds snapshots/tokens/joins only. Derive root, ancestry, active-descendant count, and main-agent edge depth from persisted rows under one runtime admission mutex, then insert before releasing admission. `begin_primary` inserts a new root for a continued turn; `activate_persisted_primary` registers the root atomically inserted with a new session and must not insert it again. `queue_main` resolves and clones the target before insertion and rejects self/cycle by stable IDs. `queue_subagent` snapshots Plan 2's shared config plus existing built-in type at insertion.

`CoreEvent::AgentRunChanged` contains `session_pk`, `run_id`, `parent_run_id`, and exact status. Emit only after commit. Completion/failure is delivered to the originating root primary run; nested child completion is also available to its immediate delegating tool call, but never starts a new session turn.

- [ ] **Step 4: Wire real ownership into `ControlPlane` and daemon startup**

`ControlPlane` is `crates/core/src/control.rs`, not `control/mod.rs`. Add `registry: Arc<AgentRegistry>` and `delegation: Arc<DelegationRuntime>` to it. Change `ControlPlane::new`, `new_with_telemetry`, and `new_full` to accept Plan 2 `AgentPersistence`; update every constructor call returned by the Files search in this same task. Production daemon initialization calls `initialize_agent_persistence(config_root, store.clone())` once and passes the result into the control plane; tests construct Plan 2 persistence under a `TempDir` and never load the real user directory. Invoke `recover_after_restart()` exactly once from daemon startup after Store/registry initialization and before accepting RPC traffic.

- [ ] **Step 5: Verify the checkpoint**

```sh
cargo fmt --check
cargo test -p ryuzi-core delegation::tests -- --nocapture
cargo test -p ryuzi-core control::tests -- --nocapture
cargo check -p ryuzi-core
```

- [ ] **Step 6: Commit the runtime**

```sh
git add crates/core/src/delegation.rs crates/core/src/lib.rs crates/core/src/control.rs crates/core/src/daemon.rs
# Add every constructor-caller file changed by this task so the checkpoint compiles.
git commit -m "feat(core): add bounded delegation runtime"
```

---

### Task 3: Ownership-Aware Session Commands and Primary Runs

**Files:**
- Modify: `crates/core/src/api/types.rs`
- Modify: `crates/core/src/api/sessions.rs`
- Modify: `crates/core/src/control/lifecycle.rs`
- Modify: `crates/core/src/harness/mod.rs`
- Modify: every `SessionCtx` literal returned by `rg -l "SessionCtx \\{" crates apps --glob '*.rs'`
- Modify: `apps/cockpit/src-tauri/src/commands.rs`
- Modify: `apps/cockpit/src-tauri/src/lib.rs`
- Test: nearby API/control/Tauri tests

**Interfaces:**
- Consumes: Tasks 1–2 and existing `TurnPrompt`, `ChatContextArg`, `GitOptions`, `AttachmentRef`, `SessionGitOptions`.
- Produces: exact ownership-aware start/continue commands, generated DTOs, and `pub async fn list_recent_sessions_for_agent(store: &Store, agent_id: &str, limit: u32) -> anyhow::Result<Vec<Session>>`, ordered by last activity descending for the Plan 3 Overview.

- [ ] **Step 1: Write failing DTO and ownership tests**

Test camel-case mention serialization, first send with an executable owner, immutable owner after rename, one new primary run per continued turn, rejection of legacy/deleted/currently-invalid owner before inserting a user message, and `list_recent_sessions_for_agent` filtering by stable `primary_agent_id` and ordering by `updated_at DESC` with a hard limit.

- [ ] **Step 2: Replace chat boundary DTOs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMention {
    pub agent_id: String,
    pub label_snapshot: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TurnInput {
    pub text: String,
    #[serde(default)]
    pub mentions: Vec<AgentMention>,
    pub context: Option<ChatContextArg>,
    #[serde(default)]
    pub attachments: Vec<String>,
    pub git: Option<GitOptions>,
}
```

RPC params are exactly:

```rust
struct StartChatP { primary_agent_id: String, turn: TurnInput }
struct StartP { project_id: String, primary_agent_id: String, turn: TurnInput }
struct ContinueP { session_pk: String, turn: TurnInput }
```

Delete `ChatRequestOptions` and its old serializer tests/re-export, and replace all Rust/Tauri references in `rg -n "ChatRequestOptions" crates apps --glob '*.rs'` during this task. Delete `prompt/options` and model/effort/permission overrides from these three RPC paths only. Preserve existing context, attachment staging, and git conversion.

- [ ] **Step 3: Add ownership-aware control methods**

```rust
pub async fn start_agent_session_with_prompt(
    &self,
    project_id: Option<&str>,
    primary_agent_id: &str,
    prompt: TurnPrompt,
    started_by: &str,
    attachments: &[AttachmentRef],
    git: Option<SessionGitOptions>,
) -> anyhow::Result<Session>;

pub async fn continue_agent_session_with_prompt(
    &self,
    session_pk: &str,
    prompt: TurnPrompt,
) -> anyhow::Result<String>; // new primary run_id
```

Start resolves `Arc<AgentSnapshot>`, constructs `AgentIdentitySnapshot`, derives runtime model/effort/permissions/tools/skills/memory/apps from that immutable snapshot, and calls Task 1's transactional owned-session/root-run insert before dispatch. Continue calls `resolve_session_agent_access`, resolves the current valid profile for the same stable ID, inserts a new parentless `primary` run, and then dispatches. Profile changes affect later turns only; existing in-flight snapshots do not change.

Add `primary_agent: Arc<AgentSnapshot>`, `run_id: String`, and `delegation: Arc<DelegationRuntime>` to `SessionCtx`. Update all `SessionCtx` fixtures in this task so it compiles. Add `list_recent_sessions_for_agent` beside the existing Store session queries, selecting owned sessions by `primary_agent_id`, ordering by the persisted last-activity/updated timestamp descending, and clamping `limit` to `1..=50`. Expose it through a thin `list_agent_sessions(agent_id, limit)` core/Tauri command so Cockpit does not filter the full session list client-side.

- [ ] **Step 4: Change thin Tauri proxies and verify Rust independently**

```rust
pub async fn start_chat_session(
    engine: Engine<'_>, runner_id: Option<String>,
    primary_agent_id: String, turn: TurnInput,
) -> R<Session>;
pub async fn start_session(
    engine: Engine<'_>, runner_id: Option<String>, project_id: String,
    primary_agent_id: String, turn: TurnInput,
) -> R<Session>;
pub async fn continue_session(
    engine: Engine<'_>, runner_id: Option<String>, session_pk: String,
    turn: TurnInput,
) -> R<()>;
pub async fn list_agent_sessions(
    engine: Engine<'_>, runner_id: Option<String>, agent_id: String, limit: u32,
) -> R<Vec<Session>>;
```

Do not regenerate bindings yet: the old generated file remains compatible with the still-unchanged frontend, and this task's independent checkpoint is Rust/Tauri compilation. Task 6 regenerates once after the child-run commands exist and before any frontend task consumes the new boundary.

- [ ] **Step 5: Verify the checkpoint**

```sh
cargo fmt --check
cargo test -p ryuzi-core api::sessions::tests -- --nocapture
cargo test -p ryuzi-core control::tests -- --nocapture
cargo test -p ryuzi-cockpit
cargo check -p ryuzi-core -p ryuzi-cockpit
```

- [ ] **Step 6: Commit the session boundary**

```sh
git add crates/core/src/api/types.rs crates/core/src/api/sessions.rs crates/core/src/control/lifecycle.rs crates/core/src/harness/mod.rs apps/cockpit/src-tauri/src/commands.rs apps/cockpit/src-tauri/src/lib.rs
# Add every SessionCtx fixture file changed by this task so the checkpoint compiles.
git commit -m "feat(core): lock sessions to primary agents"
```

---

### Task 4: UTF-16 Structured Mentions and Coordinator-Only Turns

**Files:**
- Create: `crates/core/src/mentions.rs`
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/api/sessions.rs`
- Modify: `crates/core/src/control/lifecycle.rs`
- Test: `crates/core/src/mentions.rs`
- Test: `crates/core/src/control/tests.rs`

**Interfaces:**
- Consumes: Task 3 `AgentMention`, Plan 2 registry, Task 2 runtime.
- Produces: exact mention resolver and explicit coordinator dispatch.

- [ ] **Step 1: Write failing Unicode/span/coordination tests**

```rust
#[test]
fn mentions_use_utf16_offsets_dedupe_ids_and_preserve_other_text() {
    let text = "😀 ask @Ada, then @Bob and @Ada about café";
    let mentions = vec![m("a", "Ada", 7, 11), m("b", "Bob", 18, 22), m("a", "Old", 27, 31)];
    let got = resolve_mentions(text, &mentions, "primary", &registry()).unwrap();
    assert_eq!(got.target_agent_ids, vec!["a", "b"]);
    assert_eq!(got.task, "😀 ask , then  and  about café");
}
```

Also test spoofed labels, out-of-order input, overlap, out-of-bounds UTF-16, offsets splitting a surrogate pair, range text not equal to `@{label_snapshot}`, empty delegated body, primary ID, unknown ID, and invalid/non-executable ID. Every invalid request returns a 400 and dispatches no child.

- [ ] **Step 2: Implement the exact resolver**

```rust
pub struct ResolvedMentions {
    pub task: String,
    pub target_agent_ids: Vec<String>,
    pub targets: Vec<Arc<AgentSnapshot>>,
}

pub async fn resolve_mentions(
    text: &str,
    mentions: &[AgentMention],
    primary_agent_id: &str,
    registry: &AgentRegistry,
) -> Result<ResolvedMentions, MentionError>;
```

Build a UTF-16-boundary-to-byte-index table once. Validate ranges in submitted order after sorting a copy; require exact token text `format!("@{}", label_snapshot)` only to detect stale spans, never to resolve identity. Resolve every unique ID through `resolved_snapshot`, retain first-mention order, and remove byte ranges right-to-left without trimming/normalizing. Reject a task containing only whitespace after span removal.

- [ ] **Step 3: Implement explicit coordinator semantics**

No mentions: run the primary normally. Mentions: set the primary run `running`; queue every target concurrently under it; do not provide the delegated task as an ordinary primary prompt; await all terminal outcomes; append run-scoped coordinator context entries as each arrives; then invoke the primary exactly once with:

```text
The user explicitly assigned this task to the delegated main agents below. Do not redo their task. Synthesize one answer from every result, identify each agent by name, and state every partial failure explicitly.
```

Include all child names/tasks/results/errors in structured context following that instruction. Failed/cancelled/interrupted children do not cancel siblings. Complete the primary only after synthesis succeeds; if synthesis fails, mark the primary failed while preserving child outcomes.

- [ ] **Step 4: Verify and commit**

```sh
cargo fmt --check
cargo test -p ryuzi-core mentions::tests -- --nocapture
cargo test -p ryuzi-core explicit_mentions -- --nocapture
cargo check -p ryuzi-core
git add crates/core/src/mentions.rs crates/core/src/lib.rs crates/core/src/api/sessions.rs crates/core/src/control/lifecycle.rs crates/core/src/control/tests.rs
git commit -m "feat(core): coordinate structured agent mentions"
```

---

### Task 5: Autonomous Main-Agent Tool and Provenanced Existing Subagents

**Files:**
- Create: `crates/core/src/harness/native/tools/delegate.rs`
- Modify: `crates/core/src/harness/native/tools/mod.rs`
- Modify: `crates/core/src/harness/native/mod.rs`
- Modify: `crates/core/src/harness/native/runner.rs`
- Modify: `crates/core/src/harness/native/tools/task.rs`
- Modify: `crates/core/src/domain.rs`
- Modify: `crates/core/src/approval.rs`
- Modify: every approval event/request literal returned by `rg -l "ApprovalRequest \\{|ApprovalRequested \\{" crates apps --glob '*.rs'`

**Interfaces:**
- Consumes: `DelegationRuntime`, complete `AgentSnapshot`, Plan 2 `SubagentConfig`, and existing `SubagentSpawner`/`TaskTool`.
- Produces: `delegate_agent`, `MainAgentSpawner`, full-profile delegated execution, and run-recorded existing subagent execution.

- [ ] **Step 1: Write failing schema/profile/isolation tests**

The tool accepts exactly one of:

```json
{"agent_id":"reviewer","task":"audit","context":"optional","background":false}
```

or:

```json
{"delegations":[{"agent_id":"reviewer","task":"audit","context":"optional"},{"agent_id":"tester","task":"test"}],"background":false}
```

Reject mixed/empty forms, duplicate IDs in one batch, and `background` inside batch items. Test foreground waits; background returns run IDs; complete target model/effort/permissions/rules/skills/tools/apps/memory/loop settings are used; and existing `task` subagents remain memoryless/app-less/attachment-less with shared model and bounded built-in tool set.

- [ ] **Step 2: Add `MainAgentSpawner` beside the existing spawner**

```rust
#[async_trait]
pub trait MainAgentSpawner: Send + Sync {
    fn available(&self) -> Vec<(String, String, String)>; // id, name, description
    async fn run_one(&self, request: MainDelegationRequest) -> MainDelegationResult;
    async fn run_many(&self, requests: Vec<MainDelegationRequest>) -> Vec<MainDelegationResult>;
}
```

Inject `main_agent_spawn: Option<Arc<dyn MainAgentSpawner>>` into `ToolCtx`; do not replace `spawn: Option<Arc<dyn SubagentSpawner>>`. `available()` excludes the executing agent and invalid profiles and is rendered into the native system tool description.

- [ ] **Step 3: Execute main delegates with complete immutable profiles**

Construct a native child harness from the `RunHandle.agent_snapshot`; use its model/route+effort, permission mode/rules, skills, native/plugin tools, Apps/MCP, isolated memory/knowledge, project/worktree, and loop limits. Do not inherit parent attachments. A delegated main agent receives both `delegate_agent` and existing `task`, so it may create main delegates/subagents subject to the same root limits.

Foreground results return directly to the invoking tool call. Background results enqueue/deliver through generic `BackgroundKind::Delegation` to the originating primary run context, not as a new user turn and not through `BackgroundKind::Orch`.

- [ ] **Step 4: Wrap the existing `task` spawner with run provenance**

Before each existing single/batch/background subagent starts, call `queue_subagent`; pass its run ID through the native child context; map start/tool events/messages/terminal outcome to Task 1 methods. Preserve all existing built-in subagent types/prompts and unattended policy. Do not add a second subagent tool.

- [ ] **Step 5: Add run-scoped approval identity**

Extend `ApprovalRequest`, `CoreEvent::ApprovalRequested`, and `ApprovalHub` pending keys with `run_id`, `requesting_agent_id`, and `requesting_agent_name`. Root approvals use the primary run ID. Child cards are emitted on the parent session bus. Resolution validates both `run_id` and `request_id`; remove any global request-ID-only resolution path. Update every approval literal/consumer found by the Files search in this same task; non-agent compatibility producers must supply their owning primary run identity rather than placeholders.

- [ ] **Step 6: Verify and commit**

```sh
cargo fmt --check
cargo test -p ryuzi-core harness::native::tools::delegate::tests -- --nocapture
cargo test -p ryuzi-core harness::native::tools::task::tests -- --nocapture
cargo test -p ryuzi-core harness::native::permission::tests -- --nocapture
cargo check -p ryuzi-core
git add crates/core/src/domain.rs crates/core/src/approval.rs crates/core/src/harness crates/core/src/delegation.rs
# Add every approval literal/consumer file changed by this task so the checkpoint compiles.
git commit -m "feat(core): unify main and subagent delegation provenance"
```

---

### Task 6: Exact Plan 6 Child-Run RPC and Generated Commands

**Files:**
- Create: `crates/core/src/api/delegation_api.rs`
- Modify: `crates/core/src/api/mod.rs`
- Create: `apps/cockpit/src-tauri/src/delegation_cmd.rs`
- Modify: `apps/cockpit/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces: exact Plan 6 RPCs `get_child_runs`, `get_child_transcript`, `cancel_child_run`, `retry_child_run` and Tauri command functions that Task 7 exports together with the changed session signatures.

- [ ] **Step 1: Write failing scope and lifecycle API tests**

Test active-first ordering (`queued`, `running`, then terminal descending `finished_at`), omission of primary roots, session/run mismatch as 404, primary cancellation/retry rejection, active retry rejection, terminal retry success, deleted/invalid latest profile retry as 400 without mutation, and run-scoped transcript ownership.

- [ ] **Step 2: Implement exact engine handles**

```rust
pub(crate) const HANDLES: &[&str] = &[
    "get_child_runs",
    "get_child_transcript",
    "cancel_child_run",
    "retry_child_run",
];
```

Params are `{ session_pk }`, `{ session_pk, run_id }`, `{ session_pk, run_id }`, and `{ session_pk, run_id }`. Reads verify both IDs and return `Vec<AgentRun>`/`Vec<Message>`. Mutations call `DelegationRuntime::cancel_child`/`retry_child` only after ownership scope checks.

- [ ] **Step 3: Add thin Tauri module and register commands**

```rust
pub async fn get_child_runs(
    engine: Engine<'_>, runner_id: Option<String>, session_pk: String,
) -> R<Vec<AgentRun>>;
pub async fn get_child_transcript(
    engine: Engine<'_>, runner_id: Option<String>, session_pk: String, run_id: String,
) -> R<Vec<Message>>;
pub async fn cancel_child_run(
    engine: Engine<'_>, runner_id: Option<String>, session_pk: String, run_id: String,
) -> R<()>;
pub async fn retry_child_run(
    engine: Engine<'_>, runner_id: Option<String>, session_pk: String, run_id: String,
) -> R<AgentRun>;
```

Use `engine.client(runner_id.as_deref().unwrap_or("local"))` like existing command modules and register all four in `collect_commands!`.

- [ ] **Step 4: Verify independently without exporting stale frontend bindings**

```sh
cargo fmt --check
cargo test -p ryuzi-core delegation_api -- --nocapture
cargo test -p ryuzi-cockpit
cargo check -p ryuzi-core -p ryuzi-cockpit
```

Expected: Rust core/Tauri compile with all four command functions registered. Task 7 regenerates bindings and migrates all frontend call sites atomically.

- [ ] **Step 5: Commit the complete Rust boundary**

```sh
git add crates/core/src/api/delegation_api.rs crates/core/src/api/mod.rs apps/cockpit/src-tauri/src/delegation_cmd.rs apps/cockpit/src-tauri/src/lib.rs
git commit -m "feat(cockpit): expose child run controls"
```

---

### Task 7: Cockpit Primary Selection and Composer Simplification

**Files:**
- Modify: `apps/cockpit/src/store-nav.ts`
- Modify: `apps/cockpit/src/store.ts`
- Modify: `apps/cockpit/src/store-agents.ts`
- Modify: `apps/cockpit/src/views/HomeView.tsx`
- Modify: `apps/cockpit/src/views/SessionView.tsx`
- Modify: `apps/cockpit/src/views/AgentDetailView.tsx`
- Regenerate: `apps/cockpit/src/bindings.ts`
- Test: corresponding store/view tests

**Interfaces:**
- Consumes: Plan 3 generated `AgentSummaryInfo` (`id`, not `agentId`), `useAgents`, pending Start-chat ID, Task 3 generated session commands/fields.
- Produces: deterministic primary selection, ownership-only sends, immutable header, and read-only legacy/deleted composer behavior.

- [ ] **Step 1: Write failing selection/removal/read-only tests**

Test priority `pending Start chat ID -> last valid local preference -> registry default -> first executable in registry order`; pending selection is consumed once. Assert project/context/voice/attachments remain and model/effort/permission/Orchestrate controls are absent. Assert owned running header uses `primaryAgentSnapshot`; legacy and deleted sessions disable textarea/send/queue but retain transcript/review/files/terminal. Add an Agent Overview test that loads `commands.listAgentSessions("local", agentId, 10)`, renders sessions ordered by last activity, and shows the empty state only when the returned list is empty.

- [ ] **Step 2: Add exact selector**

```ts
export const LAST_PRIMARY_AGENT_KEY = "cockpit.lastPrimaryAgentId";
export function choosePrimaryAgent(
  agents: AgentSummaryInfo[],
  requestedId: string | null,
  lastId: string | null,
  defaultId: string | null,
): string | null {
  const valid = (id: string | null) =>
    id !== null && agents.some((agent) => agent.id === id && agent.executable);
  return [requestedId, lastId, defaultId].find(valid) ??
    agents.find((agent) => agent.executable)?.id ?? null;
}
```

Persist only after a successful first send.

- [ ] **Step 3: Generate once, then remove old composer state and call generated signatures**

Run `cargo gen-bindings` first. This atomically exports Task 3's session signatures and Task 6's child commands. Then delete composer model/effort, permission override/update, Orchestrate toggle, `/orchestrate` branch, `startOrchestration`, and `ComposerModelEffortMenu` use. Calls are exactly:

```ts
commands.startChatSession(runnerId, primaryAgentId, turn)
commands.startSession(runnerId, projectId, primaryAgentId, turn)
commands.continueSession(runnerId, sessionPk, turn)
```

No executable agent disables send and shows a `Button` navigating to Plan 3's Agents repair view. Extend `useAgents` with `recentSessionsByAgent` and `loadRecentSessions(agentId)` calling `commands.listAgentSessions("local", agentId, 10)`. Replace Plan 3's Overview placeholder in `AgentDetailView` with the returned stable-ID-owned rows; clicking one focuses/navigates to that session.

- [ ] **Step 4: Verify and commit**

```sh
bun test apps/cockpit/src/store-nav.test.ts apps/cockpit/src/store.test.ts apps/cockpit/src/store-agents.test.ts apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx apps/cockpit/src/views/AgentDetailView.test.tsx
bun run typecheck
bun run --cwd apps/cockpit build
git add apps/cockpit/src/bindings.ts apps/cockpit/src/store-nav.ts apps/cockpit/src/store-nav.test.ts apps/cockpit/src/store.ts apps/cockpit/src/store.test.ts apps/cockpit/src/store-agents.ts apps/cockpit/src/store-agents.test.ts apps/cockpit/src/views/HomeView.tsx apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.tsx apps/cockpit/src/views/SessionView.test.tsx apps/cockpit/src/views/AgentDetailView.tsx apps/cockpit/src/views/AgentDetailView.test.tsx
git commit -m "feat(cockpit): lock chats to selected primary agents"
```

---

### Task 8: Structured Mention Composer UX

**Files:**
- Create: `apps/cockpit/src/lib/mentions.ts`
- Create: `apps/cockpit/src/lib/mentions.test.ts`
- Create: `apps/cockpit/src/components/composer/AgentMentionMenu.tsx`
- Create: `apps/cockpit/src/components/composer/AgentMentionMenu.test.tsx`
- Modify: `apps/cockpit/src/views/HomeView.tsx`
- Modify: `apps/cockpit/src/views/SessionView.tsx`

**Interfaces:**
- Consumes: generated `AgentMention`/`AgentSummaryInfo`, Task 7 selected/locked primary ID.
- Produces: UTF-16-correct `MentionDraft` and autocomplete.

- [ ] **Step 1: Write failing pure tests with surrogate pairs**

```ts
test("mention fields and offsets match generated contract", () => {
  const inserted = insertMention(
    { text: "😀 ask @ad", mentions: [] },
    { startUtf16: 7, endUtf16: 10, query: "ad" },
    agent("a", "Ada"),
  );
  expect(inserted).toEqual({
    text: "😀 ask @Ada",
    mentions: [{ agentId: "a", labelSnapshot: "Ada", startUtf16: 7, endUtf16: 11 }],
  });
});
```

Test insertion, edits before/inside/overlapping a token, paste, deletion, duplicate mentions retained in draft, and matching by name/description while excluding primary/non-executable agents.

- [ ] **Step 2: Implement exact pure interface**

```ts
export type MentionDraft = { text: string; mentions: AgentMention[] };
export type TextEdit = { startUtf16: number; endUtf16: number; text: string };
export type MentionQuery = { startUtf16: number; endUtf16: number; query: string };

export function activeMentionQuery(text: string, caretUtf16: number): MentionQuery | null;
export function insertMention(draft: MentionDraft, query: MentionQuery, agent: AgentSummaryInfo): MentionDraft;
export function applyTextEdit(draft: MentionDraft, edit: TextEdit): MentionDraft;
export function matchMentionAgents(
  query: string, agents: AgentSummaryInfo[], primaryAgentId: string,
): AgentSummaryInfo[];
export function serializeMentionDraft(
  draft: MentionDraft,
): Pick<TurnInput, "text" | "mentions">;
```

DOM selection/string indices already use UTF-16. Never convert to code points in TypeScript. Editing any part of a mention removes its structured span while leaving edited text.

- [ ] **Step 3: Implement autocomplete with shared primitives**

Use `MenuPanel` for composer-anchored autocomplete and `Button`/existing textarea primitives. Context autocomplete wins when the caret is inside its token; mention menu owns ArrowUp/ArrowDown/Enter/Escape only for an active `@` query. Clear text and mentions only after successful send; queued composer entries retain both.

- [ ] **Step 4: Verify and commit**

```sh
bun test apps/cockpit/src/lib/mentions.test.ts apps/cockpit/src/components/composer/AgentMentionMenu.test.tsx apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx
bun run typecheck
git add apps/cockpit/src/lib/mentions.ts apps/cockpit/src/lib/mentions.test.ts apps/cockpit/src/components/composer/AgentMentionMenu.tsx apps/cockpit/src/components/composer/AgentMentionMenu.test.tsx apps/cockpit/src/views/HomeView.tsx apps/cockpit/src/views/SessionView.tsx
git commit -m "feat(cockpit): add structured agent mentions"
```

---

### Task 9: Right-Panel Child Roster and Full Transcript

**Files:**
- Create: `apps/cockpit/src/store-delegation.ts`
- Create: `apps/cockpit/src/store-delegation.test.ts`
- Create: `apps/cockpit/src/components/session/AgentRunRoster.tsx`
- Create: `apps/cockpit/src/components/session/AgentRunRoster.test.tsx`
- Create: `apps/cockpit/src/components/session/AgentRunDetail.tsx`
- Create: `apps/cockpit/src/components/session/AgentRunDetail.test.tsx`
- Modify: `apps/cockpit/src/components/session/RightPanel.tsx`
- Modify: `apps/cockpit/src/components/session/RightPanel.test.tsx`
- Delete: `apps/cockpit/src/components/session/SubagentList.tsx`
- Delete: `apps/cockpit/src/components/session/SubagentList.test.tsx`

**Interfaces:**
- Consumes: Task 6 generated exact commands/event/run/message types.
- Produces: runner/session-scoped live Active/Done roster and selected run transcript.

- [ ] **Step 1: Write failing scoping/action/navigation tests**

Cover two runners sharing a `sessionPk`, event refetch, Active/Done status split, retry appending a new attempt, failed cancellation rollback, full transcript, Back, Copy result, related changes, and selection reset on runner/session change.

- [ ] **Step 2: Implement scoped store using exact commands**

```ts
type DelegationState = {
  bySession: Record<string, AgentRun[]>;
  transcriptByRun: Record<string, Message[]>;
  selectedBySession: Record<string, string | null>;
  load(runnerId: string, sessionPk: string): Promise<void>;
  select(runnerId: string, sessionPk: string, runId: string | null): void;
  loadTranscript(runnerId: string, sessionPk: string, runId: string): Promise<void>;
  stop(runnerId: string, sessionPk: string, runId: string): Promise<void>;
  retry(runnerId: string, sessionPk: string, runId: string): Promise<void>;
};
```

Use `commands.getChildRuns`, `getChildTranscript`, `cancelChildRun`, and `retryChildRun`. Keys are `${runnerId}:${sessionPk}` and `${runnerId}:${sessionPk}:${runId}`. Subscribe through the existing core-event bridge and refetch metadata/transcript for the affected scoped run.

- [ ] **Step 3: Implement roster/detail**

Active is exactly queued/running; Done is completed/failed/cancelled/interrupted. Cards show identity, `Main agent`/`Subagent`, task, status, elapsed/final duration, tool count, and error. Detail replaces (not nests under) roster and shows **Back to Agents**, task, metadata, existing `Transcript`, approvals/errors, usage/cost already present in message payload, final result, Stop for active, Retry for failed/cancelled/interrupted, Copy when result exists, and related changes navigation.

- [ ] **Step 4: Verify and commit**

```sh
bun test apps/cockpit/src/store-delegation.test.ts apps/cockpit/src/components/session/AgentRunRoster.test.tsx apps/cockpit/src/components/session/AgentRunDetail.test.tsx apps/cockpit/src/components/session/RightPanel.test.tsx
bun run typecheck
bun run --cwd apps/cockpit build
git add -A apps/cockpit/src/store-delegation.ts apps/cockpit/src/store-delegation.test.ts apps/cockpit/src/components/session
git commit -m "feat(cockpit): navigate live child transcripts"
```

---

### Task 10: Complete Legacy Orchestration Removal, Preserve Generic Delegation

**Files:**
- Delete: `crates/core/src/api/orch_api.rs`
- Delete: `crates/core/src/orch.rs`
- Delete: `crates/core/src/harness/native/tools/orch_block.rs`
- Delete: `crates/core/src/harness/native/tools/app_orchestrate.rs`
- Delete if present: `apps/cockpit/src/components/session/TaskStrip.tsx`
- Delete if present: `apps/cockpit/src/store-orch.test.ts`
- Modify all registrations/consumers found by the searches below in `crates/core/src`, `apps/cockpit/src`, and `apps/cockpit/src-tauri/src`
- Regenerate: `apps/cockpit/src/bindings.ts`

**Interfaces:**
- Consumes: Tasks 1–9 unified delegation.
- Produces: no legacy orchestration module, command, event, task graph, setting, built-in orchestrator, app tool, block tool, composer path, or UI; preserves generic delegation/background/subagent primitives.

- [ ] **Step 1: Add negative-space tests**

Core dispatch must return 404 for exactly:

```text
orch_submit
orch_list_roots
orch_tasks
orch_cancel
orch_retry
orch_answer_block
orch_steer
```

Frontend must send `/orchestrate audit` as ordinary `TurnInput.text`. Generated commands must contain no orchestration command/type.

- [ ] **Step 2: Remove every product/runtime producer and consumer**

Remove `orch_api`, `orch`, all seven handlers/proxies/registrations, daemon orchestration scheduler, router branches, `CoreEvent::OrchTaskChanged`, `BackgroundKind::Orch`, orchestration-only settings, built-in `orchestrator`, group-chat judge/decomposer/speaker branches, task descriptions recommending orchestrator, `app_orchestrate`, `orch_block`, Cockpit orchestration state/events/actions, TaskStrip/task graph, block-for-human orchestration cards, orchestrator color/speaker special cases, toggle, and slash-command branch.

Do **not** remove:

- `BackgroundKind::Delegation` or generic background rail enqueue/claim/delivery;
- `SubagentSpawner`, `TaskTool`, built-in subagent prompts/types, batch parallelism, or unattended subagent policy;
- `DelegationRuntime`, child cancellation/retry, result formatting, run events, or transcripts;
- generic `CancellationToken`, semaphore, progress, or provider-turn utilities used outside orchestration.

Leave historical `orch_tasks`/`orch_task_deps` migration SQL and inert tables for Plan 6's destructive migration. No live method may read/write them.

- [ ] **Step 3: Regenerate final post-removal bindings**

```sh
cargo gen-bindings
```

Update frontend calls only to generated signatures; add no casts or hand-authored DTO mirrors.

- [ ] **Step 4: Run complete absence searches**

```sh
rg -n -i "orch_submit|orch_list_roots|orch_tasks|orch_cancel|orch_retry|orch_answer_block|orch_steer|OrchTask|TaskStrip|app_orchestrate|orch_block|/orchestrate|Orchestrate" crates/core/src apps/cockpit/src apps/cockpit/src-tauri/src
rg -n "BackgroundKind::Delegation|SubagentSpawner|delegate_agent|get_child_runs" crates/core/src
```

Expected: first search finds only historical migration SQL/tests and the negative dispatcher test. Second proves generic delegation remains.

- [ ] **Step 5: Verify independently and commit**

```sh
cargo fmt --check
cargo test -p ryuzi-core rpc_surface_has_no_legacy_orchestration_methods -- --nocapture
cargo test -p ryuzi-core
cargo test -p ryuzi-cockpit export_bindings_test -- --nocapture
bun test apps/cockpit/src/store.test.ts apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx
bun run typecheck
bun run --cwd apps/cockpit build
git add -A crates/core/src apps/cockpit/src apps/cockpit/src-tauri/src
git commit -m "refactor!: remove legacy orchestration flow"
```

---

### Task 11: Full Plan 4 Verification and Plan 6 Contract Gate

**Files:**
- Modify only for failures attributable to Tasks 1–10.
- Verify: `docs/superpowers/plans/2026-07-12-agentic-06-integration-cleanup.md` consumed names.

**Interfaces:**
- Consumes: all Plan 4 tasks and Plans 1–3.
- Produces: a buildable Plan 4 increment with exact ownership and child-run interfaces for Plan 6.

- [ ] **Step 1: Add a compile-time Plan 6 contract test**

Create a `#[cfg(test)]` test module in `sessions/ownership.rs` that type-checks:

```rust
fn plan6_contract(
    session: Session,
    snapshot: AgentIdentitySnapshot,
    access: SessionAgentAccess,
) {
    let _: Option<String> = session.primary_agent_id;
    let _: Option<AgentIdentitySnapshot> = session.primary_agent_snapshot;
    let _ = snapshot;
    match access {
        SessionAgentAccess::Executable { agent_id } => drop(agent_id),
        SessionAgentAccess::LegacyReadOnly => {}
        SessionAgentAccess::DeletedReadOnly { snapshot } => drop(snapshot),
    }
}
```

Add a Tauri export assertion for `getChildRuns`, `getChildTranscript`, `cancelChildRun`, and `retryChildRun`, and absence of the old Plan 4 draft names `listAgentRuns`, `getAgentRunTranscript`, `stopAgentRun`, and `retryAgentRun`.

- [ ] **Step 2: Run Rust gates**

```sh
cargo fmt --check
cargo test -p ryuzi-core -p ryuzi-runner -p ryuzi-cockpit
cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings
cargo run -p ryuzi-runner -- --help
```

Expected: all pass with no warnings.

- [ ] **Step 3: Run frontend gates**

```sh
bun test apps/cockpit/src/lib/mentions.test.ts apps/cockpit/src/store.test.ts apps/cockpit/src/store-nav.test.ts apps/cockpit/src/store-delegation.test.ts apps/cockpit/src/components/composer/AgentMentionMenu.test.tsx apps/cockpit/src/components/session/AgentRunRoster.test.tsx apps/cockpit/src/components/session/AgentRunDetail.test.tsx apps/cockpit/src/components/session/RightPanel.test.tsx apps/cockpit/src/views/HomeView.test.tsx apps/cockpit/src/views/SessionView.test.tsx
bun run typecheck
bun run --cwd apps/cockpit build
```

- [ ] **Step 4: Run final contract/source checks**

```sh
rg -n "pub struct AgentIdentitySnapshot|pub enum SessionAgentAccess|resolve_session_agent_access|primary_agent_snapshot" crates/core/src/domain.rs crates/core/src/sessions
rg -n "get_child_runs|get_child_transcript|cancel_child_run|retry_child_run" crates/core/src apps/cockpit/src-tauri/src apps/cockpit/src/bindings.ts
rg -n "start_utf16|end_utf16|startUtf16|endUtf16" crates/core/src apps/cockpit/src
rg -n -i "orch_submit|orch_list_roots|orch_tasks|orch_cancel|orch_retry|orch_answer_block|orch_steer|OrchTask|TaskStrip|app_orchestrate|orch_block|/orchestrate" crates/core/src apps/cockpit/src apps/cockpit/src-tauri/src
```

Expected: ownership and exact Plan 6 commands are present; mention field names match across generated boundary; the last search has only migration/negative-test references.

- [ ] **Step 5: Inspect the final diff without committing this checkpoint**

```sh
git status --short
git diff --check
git diff --stat
```

Expected: no whitespace errors or unplanned files. Task 11 is verification-only and creates no commit unless fixing an attributable defect in a scoped file; do not commit the plan document.

## Acceptance Checklist

1. Every new session persists exactly one immutable `AgentIdentitySnapshot`; every turn has one parentless `primary` run.
2. `SessionAgentAccess` and child-run command names exactly match Plan 6.
3. Legacy/deleted history is readable and cannot continue; renames preserve identity snapshots.
4. Structured mentions use `startUtf16`/`endUtf16` everywhere, trust stable IDs, and dispatch deduplicated targets concurrently.
5. An explicit mention turn is coordinator-only and synthesizes once after all terminal outcomes, including partial failures.
6. Autonomous foreground/background main delegation uses complete immutable target profiles; existing subagents remain ephemeral and memoryless.
7. Depth 4 and active-descendant limit 8 are fixed and tested; statuses/kinds use only the specified values.
8. Cancellation, retry, restart interruption, run-scoped messages, and run-scoped approvals preserve provenance.
9. Cockpit shows immutable ownership and Active/Done full child transcripts through generated commands.
10. Legacy orchestration is absent from runtime/API/UI while generic delegation, `TaskTool`, and `BackgroundKind::Delegation` remain.
11. Every task regenerates boundary code when needed and ends with valid commands and an independently compiling checkpoint.
