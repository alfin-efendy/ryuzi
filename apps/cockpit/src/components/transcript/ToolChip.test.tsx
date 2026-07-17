import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ActivityItem } from "@/lib/transcript";
import { delegationSessionKey, useDelegation } from "@/store-delegation";
import { TranscriptFileContext } from "./TranscriptFileContext";
import { useUi } from "@/store-ui";

const { ActivityCluster } = await import("./ToolChip");

afterEach(cleanup);
beforeEach(() => {
  useUi.setState({ tabs: [], activeTabId: null });
  useDelegation.setState({
    bySession: {},
    rootRunBySession: {},
    rosterStateBySession: {},
    transcriptByRun: {},
    transcriptStateByRun: {},
    seenRunsByDispatch: {},
    selectedBySession: {},
  });
});

function toolItem(key: string, status = "completed"): ActivityItem {
  return {
    type: "tool",
    key,
    toolCallId: null,
    name: "read",
    kind: "read",
    status,
    subagent: null,
    output: null,
    path: null,
    input: { file_path: `src/${key}.ts` },
    durationMs: null,
    exitCode: null,
    summary: null,
  };
}

test("fold=false renders every item flat (no See N steps)", () => {
  render(<ActivityCluster items={[toolItem("a"), toolItem("b")]} />);
  expect(screen.queryByText(/See \d+ step/)).toBeNull();
});

test("fold with liveTail shows the tail and a See N steps row counting the whole run", () => {
  const items = ["a", "b", "c", "d", "e"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold liveTail />);
  expect(screen.getByText("See 5 steps")).toBeTruthy();
});

test("expanding the fold reveals the hidden chips", () => {
  const items = ["a", "b", "c", "d", "e"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold liveTail />);
  // Folded: the two oldest (a, b) are hidden.
  expect(screen.queryByText((c) => c.includes("src/a.ts"))).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /See 5 steps/ }));
  expect(screen.getByText((c) => c.includes("src/a.ts"))).toBeTruthy();
});

test("fold without liveTail folds the entire cluster", () => {
  const items = ["a", "b"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold />);
  expect(screen.getByText("See 2 steps")).toBeTruthy();
  expect(screen.queryByText("src/a.ts")).toBeNull();
});

test("singular label for a one-step fold", () => {
  render(<ActivityCluster items={[toolItem("a")]} fold />);
  expect(screen.getByText("See 1 step")).toBeTruthy();
});

test("a tool card's path opens the file without toggling the card", async () => {
  const item = {
    ...toolItem("t1"),
    path: "src/a.ts",
    output: "file contents",
  };
  render(
    <TranscriptFileContext.Provider value={{ runnerId: "local", sessionPk: "s1", workdir: "/home/u/proj" }}>
      <ActivityCluster items={[item]} />
    </TranscriptFileContext.Provider>,
  );
  const link = screen.getByRole("link", { name: /src\/a\.ts/ });
  fireEvent.click(link);
  expect(useUi.getState().tabs.some((t) => t.path === "/home/u/proj/src/a.ts")).toBe(true);
  // stopPropagation: the expandable card did not toggle open.
  expect(screen.queryByText("file contents")).toBeNull();
});

test("without a provider the detail stays a plain span", () => {
  const item = { ...toolItem("t1"), path: "src/a.ts" };
  render(<ActivityCluster items={[item]} />);
  expect(screen.queryByRole("link")).toBeNull();
});

test("an expandable no-link card toggles open when clicking the detail text (full row is the toggle)", () => {
  const item = { ...toolItem("t1"), path: null, summary: "3 files touched", output: "diff output here" };
  render(<ActivityCluster items={[item]} />);
  expect(screen.queryByText("diff output here")).toBeNull();
  fireEvent.click(screen.getByText("3 files touched"));
  expect(screen.getByText("diff output here")).toBeTruthy();
});

test("routes linked task rows to agent cards while ordinary tools retain ToolChip", () => {
  const runnerId = "local";
  const sessionPk = "session-1";
  useDelegation.setState({
    bySession: {
      [delegationSessionKey(runnerId, sessionPk)]: [
        {
          runId: "child-1",
          sessionPk,
          parentRunId: "root-1",
          retryOf: null,
          sourceToolCallId: "dispatch-1",
          dispatchIndex: 0,
          primaryAgentId: "primary",
          executingAgentId: "worker",
          executingAgentNameSnapshot: "Researcher",
          agentKind: "subagent",
          task: "Inspect this task",
          status: "running",
          startedAt: 1,
          finishedAt: null,
          toolCount: 0,
          resolvedModel: null,
          resolvedEffort: null,
          result: null,
          error: null,
        },
      ],
    },
    rosterStateBySession: { [delegationSessionKey(runnerId, sessionPk)]: { status: "ready", error: null } },
  });
  const dispatch = { ...toolItem("dispatch"), name: "task", kind: "task", toolCallId: "dispatch-1" };

  render(<ActivityCluster runnerId={runnerId} sessionPk={sessionPk} ownerRunId="root-1" items={[dispatch, toolItem("ordinary")]} />);

  expect(screen.getByRole("button", { name: /Open Researcher agent run/i })).toBeTruthy();
  expect(screen.getByText((content) => content.includes("src/ordinary.ts"))).toBeTruthy();
  expect(screen.queryByText((content) => content.includes("src/dispatch.ts"))).toBeNull();
});

test("retains a terminal ToolChip for an unadmitted batch slot beside admitted dispatch cards", () => {
  const runnerId = "local";
  const sessionPk = "session-1";
  useDelegation.setState({
    bySession: {
      [delegationSessionKey(runnerId, sessionPk)]: [
        {
          runId: "admitted-child",
          sessionPk,
          parentRunId: "root-1",
          retryOf: null,
          sourceToolCallId: "delegate-batch",
          dispatchIndex: 0,
          primaryAgentId: "primary",
          executingAgentId: "worker",
          executingAgentNameSnapshot: "Researcher",
          agentKind: "main-delegate",
          task: "Admitted work",
          status: "running",
          startedAt: 1,
          finishedAt: null,
          toolCount: 0,
          resolvedModel: null,
          resolvedEffort: null,
          result: null,
          error: null,
        },
      ],
    },
    rosterStateBySession: { [delegationSessionKey(runnerId, sessionPk)]: { status: "ready", error: null } },
  });
  const dispatch = {
    ...toolItem("delegate-batch"),
    name: "delegate_agent",
    kind: "other",
    toolCallId: "delegate-batch",
    output: "one delegation was rejected",
    dispatchFailures: [{ dispatchIndex: 1, error: "Async delegation capacity reached (1 running). Run this task synchronously." }],
  };

  render(<ActivityCluster runnerId={runnerId} sessionPk={sessionPk} live ownerRunId="root-1" items={[dispatch]} />);

  expect(screen.getByRole("button", { name: /Open Researcher agent run/i })).toBeTruthy();
  expect(screen.getByText(/Async delegation capacity reached/)).toBeTruthy();
});
