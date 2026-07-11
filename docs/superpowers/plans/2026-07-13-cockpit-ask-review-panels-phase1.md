# Cockpit Ask User, Review, and Panel Layout — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Ask User approvals step through one question at a time, repair Cockpit's existing inline Review routes, and make the session bottom/right panels behave as stable workspace-level UI.

**Architecture:** Keep existing Zustand ownership (`useNav`, `useDiff`, `useUi`) and generated bindings unchanged. Add local step navigation to `ApprovalCard`, make pending Review intent consumption explicit in `RightPanel`, and reshape `SessionView` into a vertical workspace containing a horizontal main row plus a full-width terminal row.

**Tech Stack:** React 19, TypeScript, Zustand 4, `@ryuzi/ui`, Lucide React, Tailwind CSS v4, Bun test, Testing Library, Vite.

## Global Constraints

- Phase 1 must not add a Rust API, database migration, generated-binding edit, or frontend-created `SessionKind::Review`.
- `/review` remains an inline native command in the active session.
- Use `@ryuzi/ui` primitives rather than raw interactive elements.
- Preserve existing `useNav` persistence, panel resize bounds, right-panel maximize behavior, and terminal lifetime.
- The bottom terminal spans the app workspace, not the global sidebar.
- Panel controls remain visible in the workspace top-right whether panels are open or closed.
- Questions remain optional; empty answers may advance and submit.
- Preserve unrelated worktree changes and commit each independently testable task.

## File Structure

- Modify `apps/cockpit/src/components/approval/ApprovalCard.tsx`: own step index, render one active question, reset request-local state, and implement step-aware keyboard/action behavior.
- Modify `apps/cockpit/src/components/approval/ApprovalCard.test.tsx`: cover step navigation, persistence, payloads, reset, keyboard behavior, and empty input.
- Modify `apps/cockpit/src/components/session/RightPanel.tsx`: stabilize its header, clamp selected review file, and consume pending review targets after a completed diff attempt.
- Modify `apps/cockpit/src/components/session/RightPanel.test.tsx`: cover Review loading/error/refresh/target behavior and stable header actions.
- Create `apps/cockpit/src/components/transcript/FileChangeCards.test.tsx`: protect transcript Review navigation into the right panel.
- Modify `apps/cockpit/src/views/SessionView.tsx`: move panel controls to workspace scope and move the terminal outside the horizontal chat/right-panel row.
- Create `apps/cockpit/src/views/SessionView.test.tsx`: render the view with bounded mocks and protect control accessibility and workspace row placement.
- Existing `apps/cockpit/src/store.test.ts`, `crates/core/src/harness/native/commands.rs`, and `crates/core/src/harness/native/runner.rs` already protect forwarding and execution of `/review`; no production changes are planned there.

---

### Task 1: Ask User Question Stepper

**Files:**
- Modify: `apps/cockpit/src/components/approval/ApprovalCard.tsx:107-291`
- Test: `apps/cockpit/src/components/approval/ApprovalCard.test.tsx:89-140,170-end`

**Interfaces:**
- Consumes: existing `Question`, `PendingApproval`, `ApprovalResponse`, `resolveApproval(requestId, response)`, and `@ryuzi/ui` `Button`, `Input`, and `Badge`.
- Produces: question approvals that render one active question and resolve with the unchanged payload shape `{ answers: Record<string, string[]> }`.

- [ ] **Step 1: Add failing tests for one-question-at-a-time navigation and answer persistence**

Append tests using a reusable two-question approval:

```tsx
function questionApproval(requestId = "r1"): PendingApproval {
  return approval({
    requestId,
    kind: "question",
    tool: "askuserquestion",
    input: {
      questions: [
        {
          question: "Which DB?",
          header: "Database",
          multiSelect: false,
          options: [{ label: "SQLite" }, { label: "Postgres" }],
        },
        {
          question: "Which checks?",
          header: "Checks",
          multiSelect: true,
          options: [{ label: "Tests" }, { label: "Lint" }],
        },
      ],
    },
  });
}

test("question card shows one question per step and preserves answers across Back", () => {
  render(<ApprovalCard approval={questionApproval()} />);

  expect(screen.getByText("Question 1 of 2")).toBeTruthy();
  expect(screen.getByText("Which DB?")).toBeTruthy();
  expect(screen.queryByText("Which checks?")).toBeNull();
  expect(screen.queryByRole("button", { name: "Back" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.click(screen.getByRole("button", { name: "Next" }));

  expect(screen.getByText("Question 2 of 2")).toBeTruthy();
  expect(screen.queryByText("Which DB?")).toBeNull();
  expect(screen.getByText("Which checks?")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Tests/ }));
  fireEvent.click(screen.getByRole("button", { name: "Back" }));

  expect(screen.getByRole("button", { name: /SQLite/ }).getAttribute("aria-pressed")).toBe("true");
});

test("question card combines every step in the final payload", () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={questionApproval()} />);

  fireEvent.click(screen.getByRole("button", { name: /Postgres/ }));
  fireEvent.change(screen.getByLabelText("Other answer for Database"), { target: { value: "CockroachDB" } });
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  fireEvent.click(screen.getByRole("button", { name: /Tests/ }));
  fireEvent.click(screen.getByRole("button", { name: /Lint/ }));
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));

  expect(calls[0]).toEqual([
    "r1",
    {
      decision: "allowOnce",
      scope: null,
      payload: {
        answers: {
          "Which DB?": ["Postgres", "CockroachDB"],
          "Which checks?": ["Tests", "Lint"],
        },
      },
    },
  ]);
});
```

- [ ] **Step 2: Run the navigation tests and confirm the current all-questions form fails**

Run:

```sh
bun test apps/cockpit/src/components/approval/ApprovalCard.test.tsx
```

Expected: the new tests fail because both questions render together and `Next`, `Back`, and progress text do not exist.

- [ ] **Step 3: Implement active-question state and step-aware rendering**

In `ApprovalCard`, add state and derived values:

```tsx
const [currentQuestion, setCurrentQuestion] = useState(0);
const activeQuestion = questions[currentQuestion];
const isFirstQuestion = currentQuestion === 0;
const isLastQuestion = currentQuestion === questions.length - 1;
const nextQuestion = () => setCurrentQuestion((index) => Math.min(index + 1, Math.max(questions.length - 1, 0)));
const previousQuestion = () => setCurrentQuestion((index) => Math.max(index - 1, 0));
```

Replace the question body map with one active-question block. Keep the existing option styling and `toggle` callback, and add accessible progress:

```tsx
{activeQuestion ? (
  <div className="space-y-3">
    <div className="flex items-center justify-between gap-3 text-[11.5px] text-muted-foreground">
      <span aria-live="polite">
        Question {currentQuestion + 1} of {questions.length}
      </span>
      <div aria-hidden className="flex items-center gap-1">
        {questions.map((q, index) => (
          <span
            key={q.question}
            className={`h-1.5 rounded-full ${index === currentQuestion ? "w-5 bg-primary" : "w-1.5 bg-border"}`}
          />
        ))}
      </div>
    </div>
    <div className="space-y-1.5">
      <div className="flex items-center gap-2">
        <Badge variant="outline">{activeQuestion.header}</Badge>
        <h3 className="text-[13px] font-normal">{activeQuestion.question}</h3>
      </div>
      <div className="space-y-1">
        {/* Render activeQuestion.options with the existing Button implementation and add `aria-pressed={selected}` to each option Button. */}
        <Input
          aria-label={`Other answer for ${activeQuestion.header}`}
          placeholder="Other…"
          value={others[activeQuestion.question] ?? ""}
          onChange={(event) =>
            setOthers((previous) => ({ ...previous, [activeQuestion.question]: event.target.value }))
          }
        />
      </div>
    </div>
  </div>
) : (
  <div className="py-2 text-[12.5px] text-muted-foreground">No questions were provided.</div>
)}
```

Replace question footer actions with:

```tsx
<>
  <Button size="sm" variant="outline" onClick={() => resolve(once(false))}>
    Dismiss
  </Button>
  {!isFirstQuestion && activeQuestion && (
    <Button size="sm" variant="outline" onClick={previousQuestion}>
      Back
    </Button>
  )}
  {activeQuestion && !isLastQuestion && (
    <Button size="sm" onClick={nextQuestion}>
      Next
    </Button>
  )}
  {activeQuestion && isLastQuestion && (
    <Button size="sm" onClick={submitQuestions}>
      Submit
    </Button>
  )}
</>
```

- [ ] **Step 4: Make the primary keyboard action advance before submitting**

Change the question branch of the primary action:

```tsx
if (approval.kind === "question") {
  if (activeQuestion && !isLastQuestion) nextQuestion();
  else if (activeQuestion) submitQuestions();
}
```

Keep plan and tool approval branches unchanged. Update `primaryRef.current = primary` through the existing ref pattern so the window listener sees current step state without being reinstalled on every render.

- [ ] **Step 5: Add failing tests for keyboard navigation, request reset, Dismiss, and empty input**

Add:

```tsx
test("question hotkey advances before submitting the final step", () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={questionApproval()} hotkey />);

  fireEvent.keyDown(window, { key: "Enter", ctrlKey: true });
  expect(screen.getByText("Question 2 of 2")).toBeTruthy();
  expect(calls).toHaveLength(0);

  fireEvent.keyDown(window, { key: "Enter", ctrlKey: true });
  expect(calls).toHaveLength(1);
});

test("a changed request resets question step and answers", () => {
  const view = render(<ApprovalCard approval={questionApproval("r1")} />);
  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.click(screen.getByRole("button", { name: "Next" }));

  view.rerender(<ApprovalCard approval={questionApproval("r2")} />);
  expect(screen.getByText("Question 1 of 2")).toBeTruthy();
  expect(screen.getByRole("button", { name: /SQLite/ }).getAttribute("aria-pressed")).toBe("false");
});

test("Dismiss rejects the whole question form from an intermediate step", () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={questionApproval()} />);
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(calls[0]).toEqual(["r1", { decision: "rejectOnce", scope: null, payload: null }]);
});

test("empty question input shows an empty state without Submit", () => {
  render(
    <ApprovalCard
      approval={approval({ kind: "question", tool: "askuserquestion", input: { questions: [] } })}
    />,
  );
  expect(screen.getByText("No questions were provided.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Submit" })).toBeNull();
  expect(screen.getByRole("button", { name: "Dismiss" })).toBeTruthy();
});
```

- [ ] **Step 6: Reset all request-local state when `requestId` changes**

Add one reset effect:

```tsx
useEffect(() => {
  setCurrentQuestion(0);
  setAnswers({});
  setOthers({});
  setFeedback("");
  setRejecting(false);
}, [approval.requestId]);
```

Do not add a second source of truth for answers or progress.

- [ ] **Step 7: Run the focused approval tests**

Run:

```sh
bun test apps/cockpit/src/components/approval/ApprovalCard.test.tsx
```

Expected: all `ApprovalCard` tests pass.

- [ ] **Step 8: Commit the stepper**

```sh
git add apps/cockpit/src/components/approval/ApprovalCard.tsx apps/cockpit/src/components/approval/ApprovalCard.test.tsx
git commit -m "feat(cockpit): step through ask user questions"
```

---

### Task 2: Reliable Review Target Consumption and Stable Right Header

**Files:**
- Modify: `apps/cockpit/src/components/session/RightPanel.tsx:30-139,149-218,280-312`
- Modify: `apps/cockpit/src/components/session/RightPanel.test.tsx`

**Interfaces:**
- Consumes: `useDiff` state `{ files, loading, error }`, `pendingReview`, `reviewFileIndex`, `useUi.tabs`, and existing `useNav` right-panel actions.
- Produces: a Review panel that clamps file selection, clears completed pending intents, keeps Refresh usable on errors, and exposes a stable `data-testid="right-panel-header"` header with a `shrink-0` expand action.

- [ ] **Step 1: Expand the test fixture so each test can seed diff and file-tab state**

Import the stores after the component import:

```tsx
const { useDiff } = await import("@/store-diff");
const { useUi } = await import("@/store-ui");
```

Reset them in `beforeEach`:

```tsx
useDiff.setState({ bySession: {}, pendingReview: null });
useUi.setState({ tabs: [], activeTabId: null });
gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: "" }));
```

Use this representative unified diff in tests:

```tsx
const APP_DIFF = [
  "diff --git a/src/app.ts b/src/app.ts",
  "--- a/src/app.ts",
  "+++ b/src/app.ts",
  "@@ -1 +1 @@",
  "-old",
  "+new",
].join("\n");
```

- [ ] **Step 2: Add failing tests for target selection, unmatched cleanup, and index clamping**

Add:

```tsx
test("completed diff selects and clears a pending transcript review target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(screen.getByText("src/app.ts")).toBeTruthy());
  expect(useDiff.getState().pendingReview).toBeNull();
});

test("completed diff clears an unmatched pending target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "src/missing.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().bySession.s1?.loading).toBe(false));
  expect(useDiff.getState().pendingReview).toBeNull();
  expect(screen.getByText("app.ts")).toBeTruthy();
});

test("refreshing to fewer files clamps the selected review file", async () => {
  useDiff.setState({
    bySession: {
      s1: {
        loading: false,
        error: null,
        files: [
          { dir: "src/", name: "first.ts", add: 1, del: 0, lines: [] },
          { dir: "src/", name: "second.ts", add: 1, del: 0, lines: [] },
        ],
      },
    },
  });
  const view = render(<RightPanel sessionPk="s1" branch="main" running isGit />);
  fireEvent.click(screen.getByRole("button", { name: "second.ts" }));

  useDiff.setState({
    bySession: { s1: { loading: false, error: null, files: [{ dir: "src/", name: "first.ts", add: 1, del: 0, lines: [] }] } },
  });
  view.rerender(<RightPanel sessionPk="s1" branch="main" running isGit />);

  expect(screen.getByRole("button", { name: "first.ts" })).toBeTruthy();
});
```

Add `fireEvent` to the Testing Library imports.

- [ ] **Step 3: Implement pending-target completion and selected-index clamping**

Derive a safe selected index and review:

```tsx
const selectedReviewFile = Math.min(reviewFile, Math.max(diff.files.length - 1, 0));
const review = diff.files[selectedReviewFile] ?? null;
```

Use `selectedReviewFile` for selected-row styling. Add an effect that clamps state after the list changes:

```tsx
useEffect(() => {
  setReviewFile((index) => Math.min(index, Math.max(diff.files.length - 1, 0)));
}, [diff.files.length]);
```

Replace pending-intent consumption with loading-aware completion:

```tsx
useEffect(() => {
  if (pendingReview === null || pendingReview.sessionPk !== sessionPk || diff.loading) return;
  const index = reviewFileIndex(diff.files, pendingReview.path);
  if (index >= 0) setReviewFile(index);
  setPendingReview(null);
}, [pendingReview, diff.files, diff.loading, setPendingReview, sessionPk]);
```

This keeps the target while fetch is active and clears it after either successful parsing or a completed error state.

- [ ] **Step 4: Add failing tests for refresh-after-error and fixed header action structure**

Add:

```tsx
test("Review error keeps Refresh available and retries", async () => {
  gitDiff.mockImplementationOnce(() => Promise.resolve({ status: "error", error: { message: "diff failed" } }));
  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(screen.getByText("diff failed")).toBeTruthy());
  gitDiff.mockImplementationOnce(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  fireEvent.click(screen.getByTitle("Refresh diff"));

  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(2));
  await waitFor(() => expect(screen.queryByText("diff failed")).toBeNull());
});

test("many file tabs do not move the expand action out of the fixed header", () => {
  useNav.setState({ rightTab: "file" });
  useUi.setState({
    tabs: Array.from({ length: 12 }, (_, index) => ({
      id: `/work/file-${index}.ts`,
      kind: "file" as const,
      path: `/work/file-${index}.ts`,
      title: `file-${index}.ts`,
    })),
    activeTabId: "/work/file-0.ts",
  });

  render(<RightPanel sessionPk="s1" branch="main" running isGit />);

  const header = screen.getByTestId("right-panel-header");
  const expand = screen.getByTitle("Expand panel");
  expect(header.contains(expand)).toBe(true);
  expect(expand.parentElement?.className).toContain("shrink-0");
});
```

- [ ] **Step 5: Split the right-panel header into overflow and fixed-action regions**

Replace the main tab bar with this structure while preserving the existing tab buttons:

```tsx
<div
  data-testid="right-panel-header"
  className="box-border flex h-[55px] shrink-0 items-center border-b border-border px-2.5"
>
  <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
    {rightTabs.map((tab) => {
      const selected = nav.rightTab === tab.id;
      const Icon = tab.icon;
      return (
        <Button
          key={tab.id}
          variant="ghost"
          onClick={() => nav.setRightTab(tab.id)}
          className={`shrink-0 ${selected ? "border-border bg-background text-foreground" : "text-muted-foreground"}`}
        >
          <Icon aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          {tab.label}
        </Button>
      );
    })}
  </div>
  <div className="ml-1 flex shrink-0 items-center">
    <Button
      variant="ghost"
      size="icon-sm"
      title={nav.rightMaximized ? "Restore panel" : "Expand panel"}
      onClick={() => nav.setRightMaximized(!nav.rightMaximized)}
      className="text-muted-foreground"
    >
      {nav.rightMaximized ? <Minimize2 aria-hidden size={13} /> : <Maximize2 aria-hidden size={13} />}
    </Button>
  </div>
</div>
```

Keep the separate open-file tab strip below this header with `overflow-x-auto`.

- [ ] **Step 6: Run the right-panel tests**

Run:

```sh
bun test apps/cockpit/src/components/session/RightPanel.test.tsx
```

Expected: all right-panel tests pass with no unhandled React `act` warnings from store changes.

- [ ] **Step 7: Commit Review-panel reliability**

```sh
git add apps/cockpit/src/components/session/RightPanel.tsx apps/cockpit/src/components/session/RightPanel.test.tsx
git commit -m "fix(cockpit): stabilize review panel navigation"
```

---

### Task 3: Transcript Review Action Contract

**Files:**
- Create: `apps/cockpit/src/components/transcript/FileChangeCards.test.tsx`
- Test existing production callback: `apps/cockpit/src/components/transcript/FileChangeCards.tsx:32-36`

**Interfaces:**
- Consumes: `FileChangeCards({ sessionPk, cards })`, `useDiff.setPendingReview`, `useNav.setRightOpen`, and `useNav.setRightTab`.
- Produces: a tested contract that clicking a transcript Review action records the target, opens the right panel, and activates the Review tab.

- [ ] **Step 1: Create a focused test with bounded IPC mocks**

Create the test file:

```tsx
import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const gitDiff = mock(async () => ({ status: "ok" as const, data: "" }));
const sessionWorkdir = mock(async () => ({ status: "ok" as const, data: "/work/demo" }));
const revertFile = mock(async () => ({ status: "ok" as const, data: null }));

mock.module("@/bindings", () => ({
  commands: { gitDiff, sessionWorkdir, revertFile },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { FileChangeCards } = await import("./FileChangeCards");
const { useDiff } = await import("@/store-diff");
const { useNav } = await import("@/store-nav");

beforeEach(() => {
  useDiff.setState({
    bySession: { s1: { files: [], loading: false, error: null } },
    pendingReview: null,
  });
  useNav.setState({ rightOpen: false, rightTab: "file" });
});

afterEach(cleanup);

test("Review opens the right panel on the selected changed file", () => {
  render(
    <FileChangeCards
      sessionPk="s1"
      cards={[
        {
          path: "src/app.ts",
          kind: "edit",
        },
      ]}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Review" }));

  expect(useDiff.getState().pendingReview).toEqual({ sessionPk: "s1", path: "src/app.ts" });
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useNav.getState().rightTab).toBe("review");
});
```

- [ ] **Step 2: Run the new test**

Run:

```sh
bun test apps/cockpit/src/components/transcript/FileChangeCards.test.tsx
```

Expected: PASS against the existing `review` callback; the assertions independently protect the pending target, panel opening, and Review-tab selection.

- [ ] **Step 3: Confirm the production callback remains unchanged**

The existing callback already satisfies the tested contract and must remain exactly equivalent to:

```tsx
const review = (card: EditCard) => {
  setPendingReview({ sessionPk, path: card.path });
  nav.setRightOpen(true);
  nav.setRightTab("review");
};
```

Do not trigger a second diff fetch here; `RightPanel` owns fetching when opened on Review, and `FileChangeCards` already fetches once for stats.

- [ ] **Step 4: Re-run the focused test and commit the contract test**

Run:

```sh
bun test apps/cockpit/src/components/transcript/FileChangeCards.test.tsx
```

Expected: PASS.

Then commit:

```sh
git add apps/cockpit/src/components/transcript/FileChangeCards.test.tsx apps/cockpit/src/components/transcript/FileChangeCards.tsx
git commit -m "test(cockpit): protect transcript review navigation"
```

---

### Task 4: Workspace-Level Panel Controls and Full-Width Bottom Drawer

**Files:**
- Modify: `apps/cockpit/src/views/SessionView.tsx:1-4,284-323,487-503`
- Create: `apps/cockpit/src/views/SessionView.test.tsx`

**Interfaces:**
- Consumes: existing `useNav.bottomOpen`, `rightOpen`, `rightMaximized`, `toggleBottom`, and `toggleRight`; existing `RightPanel` and `BottomTerminalDrawer` props.
- Produces: `SessionView` DOM with `data-testid="session-main-row"`, `data-testid="session-panel-controls"`, and `data-testid="session-bottom-row"` boundaries for focused layout tests.

- [ ] **Step 1: Create a focused SessionView test harness with child components mocked**

Create `SessionView.test.tsx`. Mock expensive transcript, editor, terminal, and right-panel children, then seed the real Zustand stores. The core assertions are:

```tsx
import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

mock.module("@/components/transcript/Transcript", () => ({
  Transcript: ({ children }: { children?: React.ReactNode }) => <div data-testid="transcript">{children}</div>,
}));
mock.module("@/components/session/RightPanel", () => ({
  RightPanel: () => <div data-testid="right-panel">right</div>,
}));
mock.module("@/components/session/BottomTerminalDrawer", () => ({
  BottomTerminalDrawer: () => <div data-testid="bottom-terminal">terminal</div>,
}));
mock.module("@/components/session/TodoPanel", () => ({ TodoPanel: () => null }));
mock.module("@/components/session/QueuedMessages", () => ({ QueuedMessages: () => null }));
mock.module("@/components/session/SessionCostPanel", () => ({ SessionCostPanel: () => null }));
mock.module("@/components/session/OpenInMenu", () => ({ OpenInMenu: () => null }));
mock.module("@/components/ComposerModelEffortMenu", () => ({ ComposerModelEffortMenu: () => null }));

const { SessionView } = await import("./SessionView");
const { useNav } = await import("@/store-nav");
const { useStore } = await import("@/store");

beforeEach(() => {
  useNav.setState({ bottomOpen: true, rightOpen: true, rightMaximized: false, rightTab: "review", drafts: {} });
  useStore.setState({
    focusedSessionPk: "s1",
    sessions: [
      {
        sessionPk: "s1",
        projectId: null,
        agentSessionId: null,
        worktreePath: null,
        branch: null,
        title: "Review layout",
        status: "idle",
        permMode: "default",
        startedBy: "cockpit",
        createdAt: 1,
        lastActive: 1,
        resumeAttempts: 0,
        branchOwned: false,
        kind: "chat",
        speaker: null,
        agent: null,
        parentSessionPk: null,
      },
    ],
    projects: [],
    transcripts: { s1: [] },
    pendingApprovals: [],
  });
});

afterEach(cleanup);

test("panel controls live at workspace scope and expose pressed state", () => {
  render(<SessionView />);
  const controls = screen.getByTestId("session-panel-controls");
  const chatHeader = screen.getByTestId("session-chat-header");
  const bottomToggle = screen.getByTitle("Toggle bottom panel");
  const rightToggle = screen.getByTitle("Toggle right panel");

  expect(controls.contains(bottomToggle)).toBe(true);
  expect(controls.contains(rightToggle)).toBe(true);
  expect(chatHeader.contains(bottomToggle)).toBe(false);
  expect(bottomToggle.getAttribute("aria-pressed")).toBe("true");
  expect(rightToggle.getAttribute("aria-pressed")).toBe("true");
});

test("bottom terminal is outside the horizontal main row", () => {
  render(<SessionView />);
  const mainRow = screen.getByTestId("session-main-row");
  const bottomRow = screen.getByTestId("session-bottom-row");
  const terminal = screen.getByTestId("bottom-terminal");

  expect(mainRow.contains(screen.getByTestId("right-panel"))).toBe(true);
  expect(mainRow.contains(terminal)).toBe(false);
  expect(bottomRow.contains(terminal)).toBe(true);
  expect(mainRow.parentElement).toBe(bottomRow.parentElement);
});

test("workspace toggles remain rendered and update panel state when panels close", () => {
  render(<SessionView />);
  fireEvent.click(screen.getByTitle("Toggle right panel"));
  fireEvent.click(screen.getByTitle("Toggle bottom panel"));

  expect(useNav.getState().rightOpen).toBe(false);
  expect(useNav.getState().bottomOpen).toBe(false);
  expect(screen.getByTitle("Toggle right panel")).toBeTruthy();
  expect(screen.getByTitle("Toggle bottom panel")).toBeTruthy();
});
```

The test file must also provide these exact inert mocks before importing `SessionView`, preventing render-time IPC and browser-only behavior while retaining real `useNav` and `useStore` state:

```tsx
mock.module("@/bindings", () => ({
  commands: {
    sessionWorkdir: async () => ({ status: "ok" as const, data: "/work/demo" }),
    searchFiles: async () => ({ status: "ok" as const, data: [] }),
  },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
mock.module("@/components/transcript/TranscriptFileContext", () => ({
  TranscriptFileContext: {
    Provider: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  },
}));
mock.module("@/components/composer/useComposerAttachments", () => ({
  useComposerAttachments: () => ({
    attachments: [],
    dragOver: false,
    onPaste: () => undefined,
    remove: () => undefined,
  }),
}));
mock.module("@/components/composer/AttachmentChips", () => ({ AttachmentChips: () => null }));
mock.module("@/lib/voice", () => ({
  startVoiceDictation: () => ({ ok: false as const, message: "Voice unavailable in test" }),
}));
```

Keep the real stores because this test must exercise actual toggle state.

- [ ] **Step 2: Run the new SessionView test to verify the current nesting fails**

Run:

```sh
bun test apps/cockpit/src/views/SessionView.test.tsx
```

Expected: FAIL because the workspace test IDs do not exist, controls are inside the chat header, and the terminal is inside the chat column.

- [ ] **Step 3: Reshape SessionView into a vertical workspace and horizontal main row**

Replace the outer structure beginning at the return with:

```tsx
<div className="relative flex min-h-0 flex-1 flex-col">
  <div data-testid="session-main-row" className="flex min-h-0 min-w-0 flex-1">
    <div className={`flex min-h-0 min-w-0 flex-1 flex-col ${nav.rightMaximized && nav.rightOpen ? "hidden" : ""}`}>
      {/* Existing chat header, transcript, and composer. */}
    </div>

    {nav.rightOpen && (
      <RightPanel
        key={session.sessionPk}
        sessionPk={session.sessionPk}
        branch={session.branch ?? null}
        running={running}
        isGit={project?.isGit ?? false}
      />
    )}
  </div>

  {nav.bottomOpen && (
    <div data-testid="session-bottom-row" className="min-w-0 shrink-0">
      <BottomTerminalDrawer sessionPk={session.sessionPk} projectName={projectName} />
    </div>
  )}
</div>
```

Move the existing `RightPanel` block into `session-main-row`. Move the existing `BottomTerminalDrawer` block after that row. Do not change either component's props or resize logic.

- [ ] **Step 4: Move panel toggles to an always-rendered workspace control group**

Remove the two toggle buttons from the chat header. Add this sibling after the main row begins, positioned at workspace scope:

```tsx
<div
  data-testid="session-panel-controls"
  className="absolute right-2.5 top-2.5 z-30 flex items-center gap-1 rounded-md border border-border bg-background/80 p-1 shadow-xs backdrop-blur"
>
  <Button
    variant="ghost"
    size="icon-sm"
    title="Toggle bottom panel"
    aria-pressed={nav.bottomOpen}
    onClick={nav.toggleBottom}
    className={nav.bottomOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
  >
    <PanelBottom aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
  </Button>
  <Button
    variant="ghost"
    size="icon-sm"
    title="Toggle right panel"
    aria-pressed={nav.rightOpen}
    onClick={nav.toggleRight}
    className={nav.rightOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
  >
    <PanelRight aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
  </Button>
</div>
```

Mark the chat header for the focused test and reserve its right edge:

```tsx
<div
  data-testid="session-chat-header"
  className="box-border flex h-[55px] shrink-0 items-center gap-3 border-b border-border px-5 pr-[92px]"
>
```

Reserve `pr-[92px]` in both the chat header and the right-panel header. This fixed reservation keeps the workspace control group at the workspace top-right without covering `RightPanel`'s expand action; do not move controls back into either panel header.

- [ ] **Step 5: Run the SessionView test and focused panel tests**

Run:

```sh
bun test apps/cockpit/src/views/SessionView.test.tsx
bun test apps/cockpit/src/components/session/RightPanel.test.tsx
```

Expected: all tests pass; closing either panel leaves both controls mounted.

- [ ] **Step 6: Build Cockpit to catch JSX nesting and Tailwind/type errors**

Run:

```sh
bun run --cwd apps/cockpit build
```

Expected: TypeScript succeeds and Vite produces the Cockpit frontend bundle.

- [ ] **Step 7: Commit the workspace layout**

```sh
git add apps/cockpit/src/views/SessionView.tsx apps/cockpit/src/views/SessionView.test.tsx
git commit -m "feat(cockpit): promote session panel controls to workspace"
```

---

### Task 5: Phase 1 Integration Verification

**Files:**
- Modify only for failures attributable to this phase: files listed in Tasks 1-4.
- Verify existing inline Review coverage: `apps/cockpit/src/store.test.ts:794-839`, `crates/core/src/harness/native/commands.rs:179-190`, `crates/core/src/harness/native/runner.rs:3935-3988`.

**Interfaces:**
- Consumes: all Task 1-4 deliverables.
- Produces: a clean, tested Phase 1 implementation with no generated-binding or Rust behavior changes.

- [ ] **Step 1: Run all directly affected frontend tests together**

Run:

```sh
bun test \
  apps/cockpit/src/components/approval/ApprovalCard.test.tsx \
  apps/cockpit/src/components/session/RightPanel.test.tsx \
  apps/cockpit/src/components/transcript/FileChangeCards.test.tsx \
  apps/cockpit/src/views/SessionView.test.tsx \
  apps/cockpit/src/store-diff.test.ts \
  apps/cockpit/src/store-nav.test.ts
```

On Windows shells that do not accept backslash continuation, run the same paths on one line. Expected: all targeted tests pass.

- [ ] **Step 2: Re-run the existing inline `/review` forwarding test**

Run:

```sh
bun test apps/cockpit/src/store.test.ts --test-name-pattern "start forwards chat options"
```

If Bun's installed version does not accept `--test-name-pattern`, run the whole file:

```sh
bun test apps/cockpit/src/store.test.ts
```

Expected: the test confirms `/review` is forwarded as ordinary session input with model/context/attachments and focuses the returned active session.

- [ ] **Step 3: Run frontend type/build verification**

Run:

```sh
bun run --cwd apps/cockpit build
bun run typecheck
```

Expected: both commands exit successfully. `bun run typecheck` covers the root, Cockpit, and shared UI TypeScript projects.

- [ ] **Step 4: Review the diff for scope and generated files**

Run:

```sh
git status --short
git diff --stat HEAD~4..HEAD
git diff HEAD~4..HEAD -- apps/cockpit/src/bindings.ts crates
```

Expected: no changes to `apps/cockpit/src/bindings.ts` or any Rust crate; only the approved Cockpit component/test files and plan/spec commits are present.

- [ ] **Step 5: Perform a focused manual layout smoke check when a desktop display is available**

Run:

```sh
bun run cockpit:dev
```

Verify these exact behaviors:

1. open enough Files tabs to overflow their strip; Review/Files/Agents and Expand remain visible;
2. toggle the right panel open and closed; both panel buttons remain in the workspace top-right;
3. open the terminal with the right panel visible; the terminal spans below both chat and right panel;
4. resize the right panel and terminal; persisted bounds still apply;
5. maximize the right panel; the terminal remains available below it;
6. answer a multi-question Ask User card, move Back, and confirm prior answers remain;
7. click Review on a file-change card and confirm the matching diff file opens;
8. submit `/review` and confirm output remains in the active session transcript.

If desktop startup is unavailable, report this check as skipped and rely on the component tests plus Vite build; do not claim it was run.

- [ ] **Step 6: Commit any integration-only corrections**

If verification required code corrections, commit only those corrections:

```sh
git add apps/cockpit/src
git commit -m "fix(cockpit): complete review panel integration"
```

If no correction was needed, do not create an empty commit.
