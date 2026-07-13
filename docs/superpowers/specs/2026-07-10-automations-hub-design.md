# Automations Hub: Scheduler, Hooks, and Commands

**Date:** 2026-07-10
**Status:** Approved for planning

## 1. Purpose and scope

Replace Cockpit's standalone **Scheduler** navigation item with an **Automations** hub. The hub combines three related but distinct automation mechanisms:

1. **Scheduler** — existing cron/natural-language recurring agent jobs.
2. **Hooks** — event-triggered agent runs or outbound webhook deliveries, including inbound webhook endpoints.
3. **Commands** — project-defined slash-command prompt templates, modeled after [OpenCode Commands](https://opencode.ai/docs/commands) as reviewed on 2026-07-10.

The first delivery provides complete Cockpit configuration for all three categories. It does not create a general multi-step workflow builder, execute arbitrary user shell commands, expose a public server bind, or implement a managed tunnel.

## 2. Navigation and view model

- Replace the Sidebar item label **Scheduler** with **Automations** and route it to a new `automations` hub route. Use a Lucide automation/workflow icon consistent with the existing sidebar.
- The hub has a `Segmented` control with exactly three tabs: **Scheduler**, **Hooks**, and **Commands**. The selected tab is held by the Automations view for the current visit; the Sidebar always opens Scheduler by default.
- Existing Scheduler job routes remain valid for history/backward compatibility:
  - `scheduler`
  - `jobDetail`
  - `jobNew`
- Scheduler-related Back, Save, Cancel, and delete-success navigation returns to the `automations` hub with the Scheduler tab selected. The Sidebar marks Automations active for all legacy Scheduler and all new Automation routes.
- Scheduler list semantics and existing persisted `jobs`/`job_runs` records are unchanged.

## 3. Commands

### 3.1 Source of truth and scope

Commands are named prompt templates, not shell commands. They retain the existing Ryuzi runtime convention:

- project commands live in `<project>/.ryuzi/commands/<name>.md`;
- global commands live in `~/.config/ryuzi/commands/`;
- built-ins are `init`, `review`, and `compact`.

The project command Markdown file is the source of truth. Cockpit supports full CRUD only for project-defined commands. Built-in and global commands are shown in the Commands tab as read-only rows with their origin. A command changed externally is reflected at the next list/read operation. An unreadable or invalid external file is shown as a read-only error row and is never deleted automatically.

Project command discovery is recursive under `.ryuzi/commands`; a file `commands/review/security.md` is command name `review/security`. Cockpit creates needed directories, deletes empty parent directories after deletion up to (but never including) `commands`, rejects symlinks, and canonicalizes every path to ensure it remains below the canonical commands root. Built-ins always win a name collision; for all other names, global loads before project and project replaces global at execution. The UI shows every colliding source row but marks the effective one; creating a project command that shadows a global command is allowed, while built-ins cannot be shadowed. Global files with built-in names are listed read-only as shadowed and never execute. Cockpit records an ETag-like revision from file contents at editor open and rejects a save if the current file content differs; the user must reload rather than silently overwrite an external edit.

The Commands tab has an explicit project `Combobox`, populated from local-engine projects and defaulting to the project selected in the Sidebar/session store when one exists. With no selected project it shows “Select a project to manage project commands” and disables `New command`; global and built-in commands remain visible in a separate read-only section. Every project command RPC receives this selected `projectId`, so list/read/create/update/delete are unambiguous.

A project command includes:

- `name` — normalized command name, called as `/name`;
- `description`;
- prompt `template`;
- optional `agent`;
- optional `model` (`provider/model`);
- optional `subtask`, defaulting to `false`.

Templates support Ryuzi's existing `$ARGUMENTS` placeholder and `$1` through `$9` positional placeholders. Placeholder expansion is unchanged: `$ARGUMENTS` receives all trimmed arguments, positionals are whitespace split, and an unfilled positional expands to an empty string.

Cockpit validates command names before save:

- maximum 80 characters;
- lowercase letters, digits, `-`, `_`, and `/` only;
- no leading `/`;
- no empty path segment or `.` / `..` segment;
- cannot replace the built-in names `init`, `review`, or `compact`.

Commands are persisted with an atomic temp-file-and-rename write. The exact interoperable format is:

```md
---
description: Review security changes
agent: plan
model: provider/model
subtask: false
---
Review $ARGUMENTS with emphasis on authentication and authorization.
```

Old command files without `model` or `subtask` remain valid. The runtime parser and `Command` model gain those two optional fields. When a slash command is submitted in a session composer, the registry resolves and expands it exactly as today, then passes the optional agent, model, and subtask overrides into the normal session lifecycle. Plain composer text is unchanged.

### 3.3 Command UI

The Commands tab provides:

- a `New command` toolbar action;
- searchable rows showing `/name`, description, project/global/built-in origin, and agent/model summary;
- an editor modal for name, description, prompt template, agent selector, model selector, and Subtask switch;
- a preview of `/name <arguments>` plus `$ARGUMENTS` and `$1`–`$9` help;
- edit/delete only for project commands.

## 4. Hooks

### 4.1 Persistent model and lifecycle

Hooks are engine-owned SQLite entities because their inbound paths, outbound credentials, runtime state, and execution history cannot safely be file-only state.

A new migration adds:

- `automation_hooks` — ID, name, trigger kind/configuration, action kind/configuration, enabled state, timestamps, and an inbound path where applicable;
- `automation_hook_runs` — immutable hook dispatch history: hook ID, source event envelope, status, queued/started/finished timestamps, linked session ID or outbound delivery result, and safe error text.

The full immutable trigger/action snapshot is written to each run at queue time. Editing or deleting a Hook never changes an in-flight run. Disabling/deleting prevents future dispatches but does not cancel a session or delivery already queued/running. Historical runs are retained. Every list/detail API returns at most the 20 most recent runs for a hook.

Run statuses are `queued`, `running`, `success`, `failed`, and `skipped`. Disabled Hooks are silently ignored and do not create a run; `skipped` is reserved for a rate-limited accepted event. For an Agent-run action, a row moves `queued` → `running` immediately after a fresh session is created, then uses the linked session's terminal result: completed is `success`, errored/cancelled/interrupted is `failed` with the terminal reason. For an Outbound action, one Hook run represents the complete logical delivery. It contains `attempt_count`, `last_http_status`, and an append-only encrypted-free attempt log (attempt ordinal, start/finish time, HTTP status or sanitized error); the list's 20-row cap applies to logical runs, and Hook detail includes at most its three attempt entries. Engine restart leaves queued/running agent sessions to the normal session lifecycle; active or retry-waiting outbound runs are marked `failed` with a restart-interrupted error rather than replayed automatically.

### 4.2 Trigger kinds

A Hook has exactly one trigger and exactly one action. Canonical action IDs are `agent.run` and `webhook.outbound`. `webhook.inbound` is compatible only with `agent.run`; the lifecycle, scheduler, and gateway triggers are compatible with both action IDs. The `agent.run` configuration object is `{ projectId, branch, gatewayId, prompt, agentId?: string, modelOverride?: string, subtask: boolean }`. The `webhook.outbound` configuration object is `{ url, method: "POST", headers: [{ name, value?: string, configured: boolean }], payloadTemplate?: string }`; create requires each header value, while update uses an omitted `value` only to retain an already configured secret, a supplied string to replace it, and `configured: false` to remove it. A header cannot have both a supplied value and `configured: false`. Read DTOs always omit `value` and include `configured` only.

Hook validation is deterministic: `name` is 1–120 trimmed Unicode scalar values, normalized by collapsing surrounding whitespace, and must be unique case-insensitively among all Hooks; prompt is 1–32 KiB UTF-8; an Agent run requires existing local-engine project and gateway IDs, branch ≤256 bytes, optional known agent/model IDs, and `subtask` boolean; an Outbound action requires a valid URL ≤2 KiB, `POST`, ≤20 headers with names ≤128 bytes and values ≤4 KiB, and a payload template ≤64 KiB. Only `webhook.inbound` has an inbound path, generated by the engine and never client supplied. Trigger IDs, action IDs, and action-specific fields are closed enums; unknown fields and mismatched trigger/action fields are rejected with `422` rather than retained.

Initial trigger IDs are:

- `session.start`
- `tool.before`
- `tool.after`
- `session.end`
- `scheduler.run.success`
- `scheduler.run.failed`
- `gateway.status.changed`
- `webhook.inbound`

Existing native lifecycle hook scripts and plugin extension dispatch remain unchanged. UI-created hooks are observational only: a UI `tool.before` Hook never blocks the tool. Existing on-disk scripts/extensions remain the sole source of the native `tool.before` gating result.

`tool.before` and `tool.after` are limited per Hook to 60 accepted executions per rolling minute. Inbound webhook Hooks are likewise limited per Hook to 60 accepted requests per rolling minute and return `429` rather than creating a run when exhausted. Events over the lifecycle rate limit create a `skipped` history record so tool events do not silently disappear. Other trigger kinds have no new per-Hook rate limit in this delivery.

Runtime trigger sources are connected as follows:

- lifecycle trigger data is emitted by the native harness at its existing session/tool fire points;
- scheduler triggers are emitted only after a job run reaches final `success` or `failed` status;
- gateway triggers are emitted only on a connected/offline state transition, never on a polling refresh;
- inbound webhook triggers are emitted by the local endpoint route after authentication and validation.

To prevent recursive cascades, Hook-created sessions carry immutable origin metadata `{ kind: "hook", hookId, runId, depth }`. Lifecycle Hooks are never dispatched for a session with Hook origin. Scheduler-created sessions do dispatch lifecycle Hooks as ordinary sessions. A Hook dispatch caused by any event carrying origin depth `>= 3` is recorded as `skipped`; non-session sources begin at depth 0, and a Hook-created session begins at depth 1. The dispatcher additionally enforces a fixed maximum of 1,000 Hook runs per local engine per rolling minute; excess accepted events are recorded `skipped` with an engine-limit reason.

All trigger sources construct this stable envelope:

```json
{
  "event": "scheduler.run.failed",
  "occurredAt": "2026-07-10T00:00:00Z",
  "source": { "kind": "scheduler", "id": "job-id" },
  "data": {}
}
```

`event` is one of the canonical IDs above; `occurredAt` is RFC 3339 UTC; `source.kind` identifies the originating subsystem and `source.id` is its stable primary ID where one exists. Source-specific data lives exclusively in `data`. The concrete, version-1 `data` schemas are:

- session events: `{ "sessionPk": string, "projectId": string|null, "gatewayId": string, "agentId": string, "status": string }`; `tool.before` and `tool.after` also include `{ "tool": string, "input": object, "result": object|null }`, with values truncated recursively to a serialized 64 KiB;
- scheduler final events: `{ "jobId": string, "runId": string, "sessionPk": string|null, "status": "success"|"failed", "error": string|null }`;
- gateway transition: `{ "gatewayId": string, "previousStatus": "connected"|"offline", "status": "connected"|"offline" }`;
- inbound webhook: the `request` and `body` contract in §5.2.

No trigger has user-configurable filtering in this delivery. Before persistence, prompt injection, or outbound rendering, every envelope is JSON serialized with a 256 KiB maximum: object/array fields are truncated deterministically in lexical-key order and strings add a `…[truncated]` suffix; excluded sensitive headers are removed before this cap. Oversized inbound bodies are rejected before envelope construction.

### 4.3 Agent-run action

The **Agent run** action uses Scheduler's established **Prompt & target** model:

- project ID;
- branch (empty is valid for non-Git projects);
- gateway ID;
- prompt;
- optional agent, model, and subtask overrides.

Target project and gateway must exist when the hook is saved. In this delivery a Hook Agent run may target only gateway ID `local`; the UI does not offer remote gateways and save rejects another ID. At dispatch, a missing/offline local target fails the Hook run without creating an agent session. Otherwise the engine starts a fresh agent session using a snapshot of this configuration, precisely as Scheduler jobs start fresh sessions.

The hook envelope is supplied to the agent as an explicitly labeled, JSON-formatted untrusted context block appended to the configured prompt. Event/webhook payload data never becomes system/developer instructions or trusted tool input.

### 4.4 Outbound webhook action

The **Outbound webhook** action stores:

- destination URL;
- HTTP method, fixed as `POST` in this delivery;
- enabled state inherited from the Hook;
- optional additional headers;
- optional JSON payload template.

The default payload is the stable trigger envelope. An optional payload template is a JSON document up to 64 KiB containing only string values with whole-string placeholders `${event}` and `${run}`. `${event}` replaces the full envelope as a JSON value; `${run}` replaces `{ "id": string, "hookId": string, "attempt": number, "test": boolean }` as a JSON value. Placeholders embedded within other strings, JSON Pointer paths, expressions, and escaping directives are unsupported. Missing values are impossible because both objects are supplied; any non-JSON template, unsupported placeholder, or rendered payload over 256 KiB rejects save/test with `422`. `test` is `true` only for the explicit Cockpit test-delivery action. No arbitrary script execution or chained actions are included.

Additional header values are encrypted at rest with Ryuzi's existing secret cipher and are never returned in full through Cockpit DTOs after save. The UI may return header names and a `configured` indicator only.

Outbound security and delivery contract:

- `https` URLs are accepted; `http` is accepted only for the literal hosts `localhost`, `127.0.0.1`, or `::1`;
- URLs include no userinfo, use only default HTTPS/HTTP ports (443/80), and are normalized with a standards-compliant URL parser; non-loopback hostnames resolve immediately before every attempt through the system resolver, **all** returned A/AAAA addresses are classified, and any private, loopback, link-local, carrier-grade NAT, LAN, multicast, unspecified, or reserved address rejects the delivery;
- IP literals are rejected except the three allowed HTTP loopback hosts; system HTTP proxy environment settings are disabled; after validating DNS answers, the client connects only to the selected validated address while preserving the original hostname for SNI and Host, preventing DNS rebinding between validation and connect;
- redirects are never followed;
- each attempt has a 10-second timeout;
- 2xx responses are success; every other transport or HTTP result is failed;
- retry after 1 second, 5 seconds, then 30 seconds (three total attempts);
- each delivery attempt and final result is represented in the Hook run history without leaking secret header values.

The UI offers a test-delivery action which posts a fixed sample envelope. It records history exactly like a live delivery. An inbound Hook always uses Agent run, so its accepted response always has a created session ID.

## 5. Inbound webhook

### 5.1 Network and authentication

Inbound webhooks share the existing Models Endpoint server and its API-key validator. They are available only while the Endpoint is running and it continues to bind exclusively to `127.0.0.1`; no LAN/public bind control, Ryuzi-managed tunnel, or per-webhook secret is added.

Each saved inbound Hook owns a generated stable path identifier (`wh_<random-id>`), allowing multiple endpoints. Its URL is:

```text
POST http://127.0.0.1:<endpoint-port>/v1/automations/hooks/wh_<random-id>
```

Requests authenticate using an existing endpoint key with either:

- `Authorization: Bearer <API_KEY>`; or
- `x-api-key: <API_KEY>`.

The endpoint-key validation path is reused, including encrypted storage and `last_used_at` updates. There is no anonymous mode and no separate Hook secret.

Only `POST` with `Content-Type: application/json` is accepted. The body maximum is 1 MiB. Before the route responds, it synchronously validates that the Hook's local target exists and is online and that the engine-wide rate budget is available. It returns `422` for an invalid/missing target and `429` with `Retry-After: 60` for an exhausted rate budget; these cases do not return `202` and do not create a session. The route returns:

- `401` for missing/invalid key;
- `404` for an unknown path;
- `409` for a known but disabled hook;
- `415` for non-JSON content type;
- `422` for invalid JSON, invalid target, or invalid Hook configuration;
- `429` for an exhausted per-Hook or engine rate budget;
- `503` if the local endpoint server cannot enqueue the hook run.

A valid request is queued without waiting for the agent. It returns `202 Accepted` with the hook ID, Hook run ID, and created agent session ID.

### 5.2 Inbound payload

The inbound envelope adds request metadata and the complete JSON body:

```json
{
  "event": "webhook.inbound",
  "occurredAt": "2026-07-10T00:00:00Z",
  "source": { "kind": "webhook", "id": "wh_..." },
  "data": {
    "request": {
      "method": "POST",
      "path": "/v1/automations/hooks/wh_...",
      "headers": {}
    },
    "body": {}
  }
}
```

Headers are filtered before persistence or prompt injection. At minimum `authorization`, `x-api-key`, `cookie`, `set-cookie`, and headers matching `*-token`, `*-secret`, or `*-key` (case-insensitive) are removed. The original unfiltered headers are never persisted or delivered to the agent.

## 6. Cockpit Hooks UI

The Hooks tab provides:

- `New hook` toolbar action;
- searchable rows with trigger badge, action type, target/destination, enable switch, last status, and last-run time;
- a sectioned editor: **Trigger**, **Action**, **Prompt & target** for Agent run or **Webhook delivery** for Outbound webhook, and **Status**;
- detail/history with up to 20 records, showing queue/delivery state, session link for agent runs, HTTP result for outbound delivery, and sanitized errors;
- a disabled Endpoint status callout and CTA to Models → Endpoint for inbound hooks when the endpoint server is stopped;
- the saved inbound URL, copy controls, API-key usage example, and full-payload contract only after the Hook has an ID/path;
- a test-delivery action and sanitized result for outbound hooks.

All UI controls use existing `@ryuzi/ui` primitives (`Segmented`, `FormField`, `Input`, `Textarea`, `Combobox`, `Switch`, `Button`, `SettingsCard`, and `Modal`) rather than raw form elements.

## 7. API and boundaries

- `ryuzi-core` owns migrations, validation, Hook persistence/history, dispatch, retries, outbound policy, command file CRUD, and all route behavior.
- Core RPC exposes Hook CRUD/list/detail/test operations and command project-file list/read/create/update/delete operations.
- The Cockpit Tauri commands are thin RPC proxies only. Generated Cockpit bindings are regenerated from those commands; `apps/cockpit/src/bindings.ts` is not hand edited.
- The shared local endpoint server is extended with the inbound route and delegates to core Hook dispatch after using the existing endpoint-key validator.
- Remote runner behavior is explicit: the automation hub manages the selected local engine in this delivery. The local Models Endpoint and localhost inbound routes cannot trigger a remote runner. Scheduler's existing multi-gateway target model remains unchanged for local-engine jobs.

## 8. Upgrade and failure behavior

- Fresh installs receive the two new Hook tables and empty Commands/Hooks tabs.
- Existing installs retain all `jobs`, `job_runs`, endpoint keys, project command files, global command files, built-ins, and existing on-disk/plugin hooks unchanged.
- No existing file or table is transformed or deleted by the migration.
- A command file written by Cockpit is atomically replaced; a failed write leaves the previous file intact and returns an error without changing Cockpit state.
- Hook configuration is committed transactionally before it becomes dispatchable. An inbound URL is displayed only after the database commit succeeds.
- Outbound header secrets are encrypted before their Hook row is committed. A decrypt or delivery failure records only a sanitized operational error.

## 9. Tests and verification

Rust tests cover:

- SQLite migration and CRUD/history semantics;
- trigger/action/target validation and legacy data behavior;
- immutable run snapshot and delete/disable behavior;
- lifecycle, scheduler-final-status, and gateway-transition dispatch;
- rate limits;
- inbound endpoint authentication reuse, status codes, body bound, header filtering, envelope/prompt construction, multiple paths, and enqueue behavior;
- outbound encryption, URL/SSRF policy, redirect policy, valid JSON templates, timeouts, retries, and sanitized history;
- command name validation, atomic persistence, parser compatibility, model/subtask propagation, and Composer resolution.

Cockpit tests cover:

- renamed active Sidebar navigation and tab selection;
- Scheduler preservation and its return navigation;
- Commands list/origin, validation, editor CRUD, and placeholder preview;
- Hooks list/editor variants, endpoint-stopped callout, URL/example copy, outbound test delivery, and history rendering.

Minimum verification after implementation:

```sh
cargo fmt
cargo test -p ryuzi-core -p ryuzi-runner
bun test apps/cockpit/src/...
bun run --cwd apps/cockpit build
```

## 10. Dependency-ordered delivery

1. Define core command and Hook DTOs, validators, migrations, storage, and pure tests.
2. Implement Hook dispatch and hook-run lifecycle; wire native lifecycle, Scheduler final status, and gateway transition sources.
3. Extend the Models Endpoint server with authenticated inbound webhooks and implement secure outbound delivery/retries.
4. Add core RPC and thin Tauri proxies; regenerate bindings.
5. Build Cockpit stores and the Automations hub while preserving Scheduler routes.
6. Build Commands and Hooks editors/history, then integrate expanded command options into the composer.
7. Add cross-stack tests, update user/developer docs, run required verification, and review error/security paths.
