# Cockpit Ask User, Review, and Panel Layout — Phase 1 Design

**Date:** 2026-07-13
**Status:** Approved

## Summary

This phase improves Cockpit's existing session UI in three related areas:

1. present multi-question `askuserquestion` approvals as a one-question-at-a-time stepper;
2. repair the existing inline Review flows (Review tab, transcript Review action, and `/review` in the active session);
3. reorganize the session workspace so panel toggles stay in the workspace's top-right corner and the bottom terminal spans the full workspace width.

A separate Phase 2 will design and implement full child review sessions. Phase 1 does not create `SessionKind::Review` sessions because the engine does not currently provide their creation/orchestration lifecycle.

## Goals

- Make multiple Ask User questions easier to answer without rendering a long form.
- Make every Review capability currently backed by the engine usable and predictable.
- Keep the right-panel header and expand action available when many files are open.
- Keep right- and bottom-panel toggles visible in the workspace top-right rather than treating them as chat-header actions.
- Make the bottom terminal span the chat and right-panel columns.
- Preserve existing panel persistence, resize limits, terminal lifetime, and generated Rust/TypeScript contracts.

## Non-goals

- Creating or orchestrating `SessionKind::Review` sessions.
- Adding a new Rust API, database migration, or generated binding in Phase 1.
- Replacing `/review` with a child-session workflow.
- Introducing a generic workspace-layout framework.
- Reworking file previews, terminal semantics, or the diff format.

## Architecture and Workspace Layout

`SessionView` becomes a two-row workspace:

```text
Session workspace
├─ Main row
│  ├─ Chat column
│  │  ├─ Session header
│  │  ├─ Transcript
│  │  └─ Composer
│  └─ RightPanel
└─ BottomTerminalDrawer
```

The main row owns horizontal composition between chat and `RightPanel`. `BottomTerminalDrawer` moves outside the chat column and follows the main row, so it spans the complete workspace from its left edge to its right edge, including the area below an open right panel. It remains inside the app's main workspace and does not extend below the global sidebar.

The workspace remains `min-h-0`, flex-based, and overflow-contained. The main row uses the remaining height; the terminal remains a fixed, resizable row using `useNav.bottomHeight`. Its existing resize handle and `BOTTOM_HEIGHT` clamp remain unchanged.

When the right panel is maximized, the chat column remains hidden as it is today. The right panel fills the main row, while the bottom terminal remains independently available across the workspace width.

### Workspace panel controls

The bottom- and right-panel toggles move out of the chat header into a workspace-level control group anchored at the workspace's top-right corner. The group is always rendered while a session is open, regardless of either panel's state, and does not participate in transcript, file-tab, or chat-header scrolling.

The buttons continue using `@ryuzi/ui` `Button` primitives and Lucide `PanelBottom`/`PanelRight` icons. Each button:

- keeps its existing title;
- gains `aria-pressed` reflecting whether its panel is open;
- uses the existing accent treatment when active;
- calls the existing `useNav` toggle action.

The session header retains session status, title, agent/branch metadata, and `OpenInMenu`, but no longer owns panel controls. Header padding reserves enough space where necessary so the workspace controls do not obscure content.

### Stable right-panel header

`RightPanel` keeps a fixed-height, non-scrolling main header. Within it:

- the Review/Files/Agents tab group is `min-w-0` and may scroll or compact within its allotted area;
- the expand/restore action lives in a separate `shrink-0` action region;
- overflow in the separate open-file tab strip cannot move the main header or expand action;
- the panel resize handle remains independent of header overflow.

The main header, not merely the expand icon, is the stable element. File tabs below it retain their own horizontal overflow behavior.

## Ask User Stepper

`ApprovalCard` renders one question at a time for `approval.kind === "question"`. The same presentation applies to one or many questions, giving a consistent interaction model.

### State

Local state includes:

- `currentQuestion`: zero-based active step;
- `answers`: selected option labels keyed by question text;
- `others`: free-form Other input keyed by question text.

Answers and Other values remain intact while moving backward and forward. Single-select questions replace their selected label; multi-select questions toggle labels independently. Other text is appended to that question's answer list only during submission, matching the existing approval payload contract.

When `approval.requestId` changes while the card remains mounted, all interaction state is reset: current step, answers, Other values, plan feedback, and rejection mode. The active index is clamped if malformed or changing input produces fewer questions.

### Presentation and navigation

The card shows:

- a textual progress label such as `Question 2 of 4`;
- a compact visual step indicator;
- the active question's header, prompt, options, and Other input;
- `Dismiss` on every step;
- `Back` when not on the first step;
- `Next` before the last step;
- `Submit` only on the last step.

Questions are not required. Users may advance or submit unanswered questions to preserve compatibility with the current tool contract.

`Cmd/Ctrl+Enter` advances to the next question on non-final steps and submits on the final step. `Dismiss` rejects the entire approval once. Back navigation never clears stored answers.

If the input contains no valid questions, the body displays an explicit empty state and offers `Dismiss`; it does not send an empty answer payload through a misleading Submit action.

An internal `QuestionStepper` component may be extracted from `ApprovalCard` to keep rendering and navigation bounded. `ApprovalCard` remains responsible for resolving the approval.

## Existing Review Flows

### Review tab

For a git-backed session, opening the Review tab fetches the session diff. Refresh fetches it again, and completion of a running turn triggers the existing automatic refresh. The UI explicitly distinguishes:

- loading;
- no changes;
- backend error;
- non-git repository.

An error remains visible while the header and Refresh action stay usable. The selected review-file index is clamped against the latest file list after refresh so a stale index cannot leave the content pane unusable.

### Transcript file-change Review action

Selecting Review on a transcript file-change card performs one coordinated navigation:

1. record `{ sessionPk, path }` as the pending review target;
2. open the right panel;
3. activate its Review tab;
4. fetch or consume the diff;
5. select the matching file using the existing absolute/relative and slash-tolerant path matching.

Once a completed diff attempt has been processed, the pending target is cleared whether or not a matching file exists. A missing file therefore leaves the Review tab open without retaining an intent forever. While the diff is still loading, the target remains pending.

Opening a selected diff file in Files continues to resolve the session workdir, open the file tab, and activate the right-panel Files tab.

### Inline `/review`

`/review` remains a native command executed inside the active session. Cockpit must continue to expose it through command autocomplete, send it as normal session input, and render its result in the active transcript. Tests should protect this user-visible route where practical.

Phase 1 does not relabel this inline command as a review session and does not manufacture `kind: "review"` state in the frontend.

## State and Data Flow

No new global store is introduced:

- `useNav` continues to own panel visibility, selected right tab, dimensions, and maximize state;
- `useDiff` continues to own per-session diff state and the pending transcript-to-review target;
- `useUi` continues to own open file tabs and the active file;
- `ApprovalCard` owns ephemeral stepper state.

No generated binding is edited. No Rust contract, database schema, or migration changes in Phase 1.

## Error Handling and Edge Cases

- Empty Ask User input gets a clear empty state and Dismiss action.
- A new approval request resets all local approval state.
- Question and review-file indices are clamped to current data.
- Diff errors do not hide navigation or Refresh.
- A transcript review target is consumed after a completed fetch even when unmatched.
- Persisted panel sizes and open states retain existing behavior.
- Right-panel horizontal overflow cannot displace workspace controls or the expand action.
- Closing the final terminal retains the existing empty-terminal behavior; moving the drawer does not alter terminal lifecycle.

## Accessibility

- Step progress is conveyed as text, not color or dots alone.
- The active question has a programmatically recognizable heading.
- Back, Next, Submit, Dismiss, panel toggles, and expand/restore remain semantic `Button` controls.
- Panel toggles expose `aria-pressed`.
- Expand and restore retain distinct accessible titles.
- Horizontal file-tab overflow is isolated from the stable main header.

## Testing and Verification

### Targeted component tests

Extend `ApprovalCard.test.tsx` to cover:

- one question per step;
- progress text and navigation visibility;
- forward/back navigation preserving answers;
- single-select and multi-select behavior;
- per-question Other values;
- final combined payload;
- Dismiss from any step;
- `Cmd/Ctrl+Enter` advancing and submitting;
- state reset for a changed request;
- empty question input.

Extend `RightPanel.test.tsx` and related tests to cover:

- git and non-git states;
- fetch and manual refresh;
- visible errors with usable refresh;
- stale selected index after a changed diff;
- pending target selection after loading;
- pending target cleanup when unmatched;
- stable header/action structure with many file tabs.

Add or extend `FileChangeCards` tests to verify that Review opens the right panel, selects Review, and records the target file.

Add a focused `SessionView` test or a small pure layout helper test to protect:

- bottom drawer placement outside the horizontal main row;
- workspace-level toggle presence;
- toggle state/accessibility;
- full-workspace terminal behavior with an open right panel.

### Commands

Run the smallest meaningful checks:

```sh
bun test apps/cockpit/src/components/approval/ApprovalCard.test.tsx
bun test apps/cockpit/src/components/session/RightPanel.test.tsx
bun test <additional targeted session/file-change tests>
bun run --cwd apps/cockpit build
```

Run `bun run typecheck` if extraction or shared component signatures affect broader workspace typing. Any skipped or environment-blocked check must be reported.

## Phase 2 Boundary: Full Review Sessions

A separate design will define full review-session lifecycle and semantics, including:

- an engine API to create a review child session;
- `SessionKind::Review` and `parentSessionPk` creation and persistence;
- reviewed worktree and branch context;
- read-only reviewer model/tool policy;
- events, hydration, status, and teardown;
- child presentation beneath the parent session in the sidebar;
- navigation, transcript, and follow-up behavior;
- the explicit relationship between inline `/review` and spawned review sessions.

This boundary prevents Phase 1 from shipping frontend-only review-session state unsupported by the engine.
