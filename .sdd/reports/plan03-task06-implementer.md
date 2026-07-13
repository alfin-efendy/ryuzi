# Plan 3 Task 6 — Agents Sidebar Hub and Actions Menu

## Outcome

Implemented the Cockpit Agents hub and retained the existing Learning navigation until Task 9, per controller correction.

- Added Main Agent and Sub Agent tabs with roster metadata and shared subagent model controls.
- Added the reusable agent actions menu for Start chat, Duplicate, and Delete.
- Added the create-agent modal with the complete initial mutation and accessible names for all fields.
- Added Agents as a separate sidebar destination while preserving Learning.
- Routed `{ kind: "agents" }` to `AgentsView`; `{ kind: "agentDetail" }` remains the Task 7 placeholder.
- Extended `ConfirmActionModal` with an optional `confirmDisabled` prop that defaults to `false`, with backward-compatibility coverage.

## Audit corrections

- Corrected the prior sidebar implementation so Agents is added without replacing Learning.
- Added `aria-label="Description"` to the create modal textarea, resolving the reported accessibility failure.
- Added a focused `AgentEditorModal` accessibility test.
- Confirmed delete stays on the Agents hub and is disabled when only one agent remains.
- Confirmed roster controls are siblings rather than nested interactive controls and use shared `@ryuzi/ui` primitives.

## Verification

| Check | Result |
| --- | --- |
| `bun test apps/cockpit/src/views/AgentsView.test.tsx` | 4 pass / 0 fail |
| `bun test apps/cockpit/src/components/agents/AgentActionsMenu.test.tsx` | 5 pass / 0 fail |
| `bun test apps/cockpit/src/components/agents/AgentEditorModal.test.tsx` | 1 pass / 0 fail |
| `bun test apps/cockpit/src/components/shell/Sidebar.test.tsx` | 5 pass / 0 fail |
| `bun test apps/cockpit/src/components/modals/ConfirmActionModal.test.tsx` | 2 pass / 0 fail |
| `bun run typecheck` | pass |
| `bun run --cwd apps/cockpit build` | pass (existing large-chunk advisory only) |
| `bunx biome check` on all touched product/test files | clean |
| `git diff --check` | clean |

No generated build output or other artifacts are included in the commit.
