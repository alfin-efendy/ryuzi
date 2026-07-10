# Approvals

The engine gates native tool calls through one pipeline:

1. `PermMode::Plan` hard-denies non-safe tools (nothing overrides this).
2. Session overrides — "allow/deny for this session" replies, in-memory.
3. Project `tool_policies` rows — `allowAlways` and `rejectAlways`, persisted;
   manage them in Cockpit under Settings → Permissions.
4. Mode auto-allow (`acceptEdits`, `bypassPermissions`, safe tools).
5. Otherwise: `CoreEvent::ApprovalRequested` is emitted and the call parks
   until a surface resolves it with an `ApprovalResponse`.

`ApprovalRequested.approval_kind` distinguishes tool permissions from the two
interaction tools:

- `exitplanmode` (Plan kind): plan review. Approving switches the session's
  permission mode and persists it to that SESSION's row (per-session; the
  project's mode remains only the default seed for new sessions); rejecting
  sends feedback back to the model.
- `askuserquestion` (Question kind): a 1-4 question multiple-choice form whose
  answers become the tool result.

Surfaces: Cockpit renders `ApprovalCard`s in the session view and the
cross-session Inbox; the CLI prompts inline. Gateways (Discord, headless
daemon) only fan out Tool-kind prompts as binary approve/deny; Plan/Question
prompts in headless sessions expire after `approval_timeout_ms` and the tool
reports that no interactive surface answered.

MCP tools are gated per-tool: the permission key is the tool's own full name
(`mcp__server__tool`), so "don't ask again" rules never span multiple MCP
tools or servers.
