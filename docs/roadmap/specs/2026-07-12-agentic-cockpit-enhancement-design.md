# Agentic Cockpit Enhancement Design

**Date:** 2026-07-12
**Status:** Approved
**Branch:** `feat/enhance-agentic`

## 1. Purpose

Make persistent main agents first-class entities in Ryuzi while keeping subagents ephemeral. Move agent behavior out of chat-level controls, isolate each main agent's configuration and knowledge, replace the legacy orchestration UX/runtime with natural agent delegation, and enhance Cockpit's Agents UI and live child-run monitoring.

The design uses:

- Cockpit-managed YAML files as the source of truth for agent configuration, following the declarative direction of Omnigent's agent bundles.
- Per-agent Open Knowledge Format (OKF) Markdown bundles for memory and learning.
- SQLite only for operational session, transcript, run, queue, and delegation provenance.
- Provider/model capability metadata as the single source for effort controls on provider, route, agent, and subagent surfaces.

References used to derive this self-contained contract:

- Omnigent's declarative agent YAML (`docs/AGENT_YAML_SPEC.md` in the local reference checkout).
- Open Knowledge Format overview: <https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing>.
- OKF v0.1 draft/reference implementation (`okf/SPEC.md` in the local knowledge-catalog checkout).

The normative requirements needed for implementation are reproduced in this document: declarative YAML without credentials; one Markdown concept per knowledge unit; YAML frontmatter; optional generated indexes/logs; extension-field tolerance; and standard Markdown links. Implementation must not depend on either local reference checkout being present.

## 2. Product Model

### 2.1 Main agents

A main agent is a permanent CRUD entity with a stable `agent_id`. Each main agent owns:

- name, avatar/color, and capability description;
- one concrete model or model route;
- optional effort for a concrete model only;
- permission policy and persisted approval rules;
- skills, native tools, plugin tools, and Apps/MCP access;
- loop settings and limits;
- an isolated memory and Learning bundle.

Main agents can own sessions, be mentioned by users, and be selected autonomously by another main agent for delegation. A delegated main agent executes with its full profile, including its own model, permissions, tools, and knowledge.

Names must be unique case-insensitively so `@Name` is unambiguous. Renaming does not change `agent_id`.

### 2.2 Subagents

Subagents are runtime-only workers created by main agents inside sessions. Users cannot create or manage subagent types. Subagents:

- have no persistent memory or Learning bundle;
- share one global model/model-route configuration;
- terminate with their task/session lifecycle;
- remain distinguishable from delegated main agents in provenance and UI.

### 2.3 Bootstrap and deletion

On a fresh install, on the first ordinary upgrade to this schema, or when agent data is reset, Ryuzi removes legacy agent configuration/knowledge and creates one preconfigured main agent named **Ryuzi**. Projects, providers, and historical sessions remain intact. Pre-feature sessions have no owner and remain readable as **Legacy agent** history; they are read-only and are not assigned Ryuzi retroactively.

Any main agent, including Ryuzi, can be deleted if at least one main agent remains. Historical sessions owned by a deleted agent remain accessible read-only through identity snapshots. Agent configuration and knowledge are removed with the agent.

## 3. File-Based Persistence

### 3.1 Storage boundaries

| Data | Authority |
|---|---|
| Main-agent profiles and behavior | YAML files |
| Shared subagent model | YAML file |
| Agent memory and Learning | Per-agent OKF Markdown bundle |
| Session/transcript/run/delegation provenance | SQLite |
| Durable Learning delivery queue | SQLite operational queue |
| Providers, accounts, model catalog/routes | Existing model storage |
| Scheduler and other operational state | Existing storage |

Cockpit is the supported writer for agent YAML and OKF files. Files remain human-readable and portable, but live external-edit watching is not required. A reload operation may re-read files. Unknown YAML and OKF extension fields must survive UI round-trips.

### 3.2 Directory layout

Use Ryuzi's cross-platform configuration directory:

```text
<ryuzi-config>/
└── agents/
    ├── index.yaml
    ├── subagents.yaml
    └── <agent-id>/
        ├── agent.yaml
        └── knowledge/
            ├── index.md
            ├── log.md
            ├── memory/
            │   ├── index.md
            │   ├── global/
            │   ├── user/
            │   └── projects/<project-id>/
            ├── learning/
            │   ├── index.md
            │   ├── skills/
            │   ├── reviews/
            │   └── journey/
            └── curator/
                ├── index.md
                ├── state.md
                └── history/
```

`index.yaml` stores only schema version, display order, and the default agent ID. It does not duplicate agent profiles.

### 3.3 Agent YAML

Example:

```yaml
schema_version: 1
id: code-reviewer
name: Code Reviewer
description: Reviews implementation quality, safety, and regressions.
avatar:
  color: violet

model:
  name: openai/gpt-sol-5-6
  effort: high

permissions:
  mode: ask
  rules:
    - tool: bash
      decision: allow
      command_prefix: cargo test

skills:
  enabled:
    - systematic-debugging
    - requesting-code-review

tools:
  native:
    - read
    - grep
    - glob
    - bash
  plugins:
    - github.create_pull_request
  apps:
    - github

loop:
  max_turns: 50
  max_tool_rounds: 100
```

`description` is the capability description shown to users and other agents. Concrete model names use the canonical `provider-family/model-id` request value, so provider-family resolution is unambiguous. Permission rules use the engine's typed persisted-rule contract; the example shows an allow decision scoped to a tool and command prefix. Native tools, plugin-provided tools, and Apps/MCP principals have separate stable-ID lists.

Secrets and provider credentials must never be stored in agent YAML. Apps, tools, skills, models, and routes are referenced by stable IDs.

The model field is an exclusive union:

```yaml
model:
  route: smart
```

or:

```yaml
model:
  name: openai/gpt-sol-5-6
  effort: high
```

Rules:

- exactly one of `route` and `name` is required;
- `effort` is invalid with `route`;
- `effort` is optional with `name` and valid only when the selected concrete model advertises it;
- model routes control effort through their concrete targets.

`subagents.yaml` uses the same union and contains only the shared model configuration:

```yaml
schema_version: 1
model:
  route: fast
```

### 3.4 Atomicity

Create/update/delete operations validate before mutation and use a small recovery journal in `agents/.transactions/`. Each transaction stages complete replacement files/directories, records old/new paths and the intended default/order, then commits agent directories before atomically replacing `index.yaml`. Startup replays a committed journal or rolls back an uncommitted one, so a crash between filesystem mutations cannot leave an unregistered directory or dangling registry entry. Cache updates happen only after the transaction is committed. Deletion moves an agent directory to internal trash, updates registry/default state, and cleans trash after success. File-write failures retain the previous active version and roll back optimistic UI state.

## 4. Open Knowledge Format

### 4.1 Bundle profile

Each agent has a fully isolated OKF bundle. A concept is a UTF-8 Markdown file with YAML frontmatter. Ryuzi's profile requires:

- `type`;
- `title`;
- `description`;
- ISO 8601 `timestamp`.

Additional fields such as `agent_id`, `scope`, tags, and provenance are valid extensions. Unknown fields are preserved. Standard Markdown links express relationships. Broken links do not invalidate a bundle.

`index.md` supports progressive disclosure and may be regenerated. `log.md` records creation, update, rollback, and deletion entries. Reserved files are not writable through ordinary concept operations.

### 4.2 One fact per concept

Each memory fact is one file:

```markdown
---
type: Memory
title: Prefer concise implementation summaries
description: The user prefers concise summaries after code changes.
timestamp: 2026-07-12T14:30:00Z
scope: user
agent_id: ryuzi
tags: [preference, communication]
---

The user prefers concise summaries that identify changed files and
verification commands without repeating the implementation plan.

# Citations

[Original session](ryuzi://sessions/c-123#turn-18)
```

Scopes are namespaced inside an agent. Cockpit is a single-local-user product, so `user` intentionally requires no additional user/account ID:

- `global`: global only for that agent;
- `user`: knowledge about the local Cockpit user, owned by that agent;
- `projects/<project-id>`: agent knowledge for a project.

No scope is shared across main agents. Delegated main agents read their own bundle. Subagents receive no persistent-memory snapshot and no persistent-memory write tool.

### 4.3 Learning concepts

Learning also uses OKF:

- skill summaries in `learning/skills/`;
- review findings in `learning/reviews/`;
- milestones in `learning/journey/`;
- curator state in `curator/state.md`;
- rollback records in `curator/history/`.

A per-agent single-writer queue serializes Learning updates. Every event that changes memory, skill usage, review findings, journey milestones, curator state, or rollback history is durable; ephemeral UI progress is not. SQLite queue rows carry a unique event ID, agent ID, monotonic per-agent sequence, and payload. OKF concepts record the event ID so replay is idempotent. Restart resumes in sequence order. Deleting an agent first blocks new events and discards its unconsumed rows as part of the same delete workflow. SQLite is transport, not Learning authority.

### 4.4 Knowledge operations

`KnowledgeStore::for_agent(agent_id)` owns listing, reading, creating, updating, deleting, searching, index generation, and log append. Concept IDs and path segments are validated; traversal and escaping symlinks are rejected.

The memory tool retains `add`, `replace`, `remove`, and `batch` semantics. Add creates a concept, replace preserves its concept ID, remove stages deletion, and batch validates all operations before exposing changes.

## 5. Cockpit Information Architecture

### 5.1 Sidebar

Add **Agents** to the main sidebar. Remove top-level **Learning**. Keep **Models** as provider/account/catalog/route management.

### 5.2 Agents hub

Use a hub-to-dedicated-detail layout. The hub has:

- **Main Agent** tab: roster plus New agent;
- **Sub Agent** tab: shared model/model-route picker only, with explanatory copy that subagents are ephemeral, memoryless, and not user-creatable.

Main-agent rows/cards show avatar, name, description, model/route, permission, and skill/tool counts. Clicking the body opens the dedicated detail page. The action menu contains:

- Start chat;
- Duplicate;
- Delete.

Start chat is not a standalone header button.

### 5.3 Agent detail

The dedicated page has Back, identity, and an action menu. Tabs:

1. **Overview** — identity, count of readable knowledge concepts, enabled skills/tools, and recent sessions ordered by last activity.
2. **Model** — route or concrete model; effort only for supported concrete models.
3. **Permissions** — mode and approval rules. Persisted rules can be created only by explicit edits here; runtime approval cards grant or deny one request and never create a durable rule.
4. **Skills & Tools** — skills, native/plugin tools, Apps/MCP access.
5. **Learning** — memory editor, journey milestones, per-skill usage/success counts, review concepts, curator status, and rollback. Rollback lists curator history snapshots and atomically restores the selected prior OKF state while recording a new rollback event; it does not rewrite agent YAML or transcripts.
6. **Advanced** — loop settings, limits, and danger zone.

Move current Settings agent defaults, loop settings, and permissions into agent detail.

## 6. Sessions and Composer

### 6.1 New session

New session has a primary-agent selectbox. Initial selection is the last valid agent stored in Cockpit's local UI preferences (global to this desktop user), otherwise `index.yaml`'s default, otherwise the first executable agent in registry order. If none is executable, the composer stays disabled and links to the Agents repair view; automatic default creation occurs only during startup/registry recovery. Start chat from an agent action opens New session with that agent preselected.

On first send, persist `primary_agent_id` and identity snapshots. The primary agent is locked for the session. Project, branch, worktree, context, and attachments remain selectable.

Remove from New session and running-session composers:

- model and effort controls;
- permission selector;
- Orchestrate toggle and `/orchestrate` behavior.

A running-session header shows the primary agent read-only. Deleted-agent sessions show a Deleted marker and are read-only.

### 6.2 Mentions

Typing `@` opens a main-agent autocomplete. It excludes the primary agent, deleted agents, and invalid agents; searches names and descriptions; and supports keyboard navigation.

Mentions are structured references carrying `agent_id` and a label snapshot. Backend resolution trusts only `agent_id`. Multiple unique mentions delegate the same task in parallel; duplicate mentions execute once. Mention tokens are removed from the delegated task body while all other text is preserved. On an explicitly mentioned turn, the primary agent acts as coordinator: it does not separately execute the task, waits until every mentioned run reaches a terminal state, receives each result/error as it arrives, and then synthesizes one response that identifies partial failures. Autonomous background delegations on ordinary turns can stream completion events into the primary run without blocking unrelated primary work.

### 6.3 Autonomous collaboration

Main agents also receive a tool for single or batch delegation to other main agents. Available agents and capability descriptions are exposed to the agent. Self-delegation and ancestry cycles are rejected. Initial engine limits are a maximum main-agent delegation depth of 4 edges and 8 concurrently active child runs per root primary run. These are fixed, documented constants in the first release; per-agent loop settings do not override them.

Subagent execution keeps the existing built-in subagent prompts/types and unattended permission policy. All subagents use `subagents.yaml`'s model selection, inherit the dispatching run's project/worktree and explicitly passed task/context, receive the existing bounded native tool set for their type, and receive neither the parent's persistent memory, Apps/MCP grants, attachments, nor full main-agent profile unless the dispatcher serializes relevant content into the task. A delegated main agent may dispatch these same runtime-only subagents.

A delegated main agent uses its complete profile and may use subagents. Results/errors return to the primary run for synthesis. Delegation never changes session ownership.

## 7. Delegation Runtime and Legacy Orchestration Removal

Replace the old root-goal/decomposer/judge orchestration flow entirely. Remove its chat UI, `/orchestrate` path, task-graph-specific UI, and old submission flow. Preserve reusable primitives—parallelism, background execution, progress, cancellation, retry, result delivery, and provenance—inside a unified delegation runtime.

Operational run records include:

```text
run_id
session_pk
parent_run_id
retry_of
primary_agent_id
executing_agent_id
executing_agent_name_snapshot
agent_kind             # primary | main-delegate | subagent
task
status                 # queued | running | completed | failed | cancelled | interrupted
started_at
finished_at
tool_count
resolved_model
resolved_effort
result
error
```

The primary turn is a `primary` run and is the provenance root; child `parent_run_id` values always reference another run. A run snapshots the resolved agent profile in process at start. SQLite persists only its identity and execution audit fields, while the live runtime owns the immutable full snapshot needed by in-flight work. Updating or deleting YAML does not mutate/cancel an active snapshot; restart cannot resume it and therefore marks it interrupted. Profile changes apply to later turns/runs.

Stopping a child run cancels its descendants but not siblings or the primary run. Retry creates a new run using the latest valid profile and preserves prior attempts. After restart, completed runs remain viewable and every previously queued/running run becomes terminal `interrupted` with a standard restart reason; it is never resumed automatically.

## 8. Right-Panel Agent Experience

The existing **Agents** tab becomes an Active/Done roster containing delegated main agents and subagents. Cards show:

- identity/icon;
- Main agent or Subagent type;
- task summary;
- status and duration;
- tool count;
- error state.

There is no inline detail. Clicking a card replaces the roster with a full live child transcript and a Back to Agents control. Detail includes task, assistant/thought blocks under existing visibility policy, tool calls/results, file changes, approvals/errors, cost/usage, and final result. Actions include Stop, Retry, Copy result, and opening related changes.

Approvals surface in the parent session and identify the requesting agent. The decision applies only to that run.

## 9. Capability-Aware Model Effort

### 9.1 Single resolver

Provider detail, route editor, main-agent detail, and Sub Agent configuration consume the same `(provider family, concrete model)` capability resolver. Concrete selections use canonical `provider-family/model-id` values; model IDs do not have to be globally unique. Resolution order:

1. provider-discovered metadata;
2. documented vendored catalog fallback for missing capability data;
3. unknown/unsupported.

Discovery wins when it supplies capability data. An explicit unsupported/empty capability is not overwritten. Unknown models are never guessed to support effort.

The initial Anthropic fallback is pinned to <https://platform.claude.com/docs/en/build-with-claude/effort>, reviewed on 2026-07-12. Canonical family/model patterns are: `anthropic/claude-fable-5`, `anthropic/claude-mythos-5`, `anthropic/claude-mythos-preview`, `anthropic/claude-opus-4-8`, `anthropic/claude-opus-4-7`, `anthropic/claude-opus-4-6`, `anthropic/claude-sonnet-5`, `anthropic/claude-sonnet-4-6`, and `anthropic/claude-opus-4-5`. The resolver also matches documented dated aliases through the existing model-ID normalization that strips only a terminal `-YYYYMMDD` or `-YYYY-MM-DD`; it does not use display-name matching. All patterns support `low`, `medium`, and `high`; default is `high`. `max` is additionally supported by all listed patterns except Opus 4.5. `xhigh` is additionally supported by Fable 5, Mythos 5, Opus 4.8, Opus 4.7, and Sonnet 5. The vendored catalog records source URL and review date and is updated deliberately when official documentation changes. Do not apply provider-wide assumptions. Fallback applies across Anthropic API-key/OAuth surfaces only where the wire protocol supports `output_config.effort`.

Effort controls render only when the resolved `supported` list is non-empty. No disabled placeholder is shown otherwise.

### 9.2 Route target effort

Promote `ModelRouteTarget.effort` from legacy compatibility storage to supported configuration:

```rust
pub struct ModelRouteTarget {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>, // None = Model default
}
```

Each route target displays an effort selector only if that concrete model supports effort. Options are Model default plus only supported values. Changing model preserves a compatible effort and clears an incompatible one. Backend validation uses the same resolver. A concrete main-agent or subagent model with omitted effort likewise uses the global concrete-model preference, then the provider/model default.

Route resolution precedence is:

1. explicit target effort;
2. global concrete-model preference from Provider detail;
3. provider/model default.

A caller-supplied effort does not override route policy. Agent YAML referencing a route contains no effort.

Route cards summarize per-target overrides. Existing legacy Codex virtual suffixes are migrated only for the OpenAI family using the router's existing deterministic suffix parser: strip one terminal `-review`, then one recognized terminal effort suffix (`-minimal`, `-low`, `-medium`, `-high`, `-xhigh`, or `-ultra`, longest first). Migration occurs only when the stripped base model exists and the parsed effort is supported; otherwise the target remains unchanged and is surfaced for manual repair. Duplicate/colliding normalized targets preserve their original order and remain separate.

## 10. Agent Registry and APIs

All engine components access YAML through an `AgentRegistry`; none reads files ad hoc. It lists, gets, creates, updates, duplicates, deletes, selects default, loads shared subagent model, validates references, and returns immutable resolved snapshots.

Invalid agents remain visible when identity can be recovered but cannot start sessions or receive mentions/delegations. Repair is limited to a validation-error view plus the normal typed field editors; Cockpit never guesses replacement values. Invalid knowledge concepts expose their path, parser error, raw Markdown editor, Validate, and Delete actions. If no executable agent remains, the engine creates a new uniquely identified `Ryuzi` agent from the versioned built-in default template; it does not overwrite or rename an invalid user-owned Ryuzi directory. This bootstrap happens only at startup/registry recovery and is reported in Cockpit.

A missing model/route leaves an agent editable but non-executable. A malformed knowledge concept is skipped for injection and shown in a repair view without invalidating the agent or being deleted automatically.

SQLite sessions retain only agent references and identity snapshots, not agent configuration. Resolved model/effort in run provenance is audit history, not configuration authority.

## 11. Validation and Error Semantics

- Agent model union and effort support are validated in UI and backend.
- Main-agent names are unique case-insensitively.
- Minimal one-agent invariant is enforced transactionally.
- Unknown YAML/OKF fields survive round-trip.
- Invalid route targets block route resave until corrected.
- Knowledge paths cannot escape their bundle.
- File failures never activate partial state.
- Session history remains readable after rename/delete.
- Optimistic UI changes roll back on persistence failure.
- Error messages identify the logical file/resource without exposing secrets.

## 12. Testing Strategy

### Agent files

Test parse/serialize, model union, supported effort, unknown-field preservation, atomic CRUD, unique duplicate IDs/names, last-agent protection, invalid agents, and corrupt-index recovery.

### OKF

Test one-fact/one-concept creation, required frontmatter, CRUD/batch, agent and project isolation, unknown metadata/link preservation, invalid-document tolerance, index/log behavior, path safety, and concurrent single-writer correctness.

### Sessions/delegation

Test locked ownership, one/multiple/deduplicated mentions, autonomous delegation, full target profile use, memory isolation, memoryless subagents, self/cycle/depth/concurrency rejection, result/error delivery, approval routing, cancellation, retry, and restart reconstruction.

### Model effort

Test no picker without capability, supported options only, incompatible effort clearing, documented Anthropic fallback and unknown-model exclusion, target-over-request precedence, model-default preference resolution, and backend rejection of unsupported values.

### Cockpit

Test Agents hub/tabs/actions, Start chat in action menu, Learning removal from sidebar, New session selection, composer control removals, mention autocomplete, Active/Done roster, card-to-full-transcript navigation, Back behavior, and deleted-agent read-only history.

## 13. Delivery Sequence

The program is implemented through coordinated subplans, not one monolithic patch:

1. **Capability resolver foundation plan** — extract and test the shared provider/model capability interface used by YAML validation and all effort controls; include the pinned Anthropic fallback without changing route persistence yet.
2. **Persistence foundation plan** — YAML registry, first-upgrade reset/default Ryuzi, validation/recovery journal, OKF store, isolated memory, and Learning queue, consuming the resolver interface from plan 1.
3. **Agent management plan** — CRUD APIs/bindings and Cockpit Agents hub/detail, including Learning relocation and removal of legacy Settings ownership.
4. **Session/delegation plan** — locked session ownership, composer simplification, structured mentions, unified primary/delegate/subagent run provenance, right-panel child transcripts, and legacy orchestration/API removal.
5. **Route-effort plan** — editable route-target effort, precedence, route execution changes, and legacy suffix cleanup on the established resolver.
6. **Integration/cleanup plan** — cross-stack compatibility, old storage/code removal, e2e coverage, docs, and release checks.

The plans execute in that order; each has its own implementation checkpoints and tests, and later plans consume only committed interfaces from earlier plans. Each increment must keep the repository buildable. Agent configuration must never be dual-written to YAML and SQLite as competing sources of truth.

## 14. Acceptance Criteria

The feature is complete when users can create isolated YAML-backed main agents, choose one before chat, delegate to others by `@` or autonomous agent action, inspect every delegated main-agent/subagent transcript from the right panel, and manage each main agent's OKF-backed Learning without chat-level model/effort/permission/orchestration controls. Route targets expose only model-supported effort options, including documented Anthropic capability fallback, and legacy orchestration no longer exists as a product flow.
