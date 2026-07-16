import { afterEach, beforeEach, expect, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentRun, Message } from "@/bindings";
import type { ActivityItem } from "@/lib/transcript";
import { dispatchSlotKey } from "@/lib/agent-runs";
import { delegationRunKey, delegationSessionKey, useDelegation } from "@/store-delegation";
import { useNav } from "@/store-nav";
import { AgentDispatchGroup } from "./AgentDispatchGroup";

const runnerId = "local";
const sessionPk = "session-1";
const ownerRunId = "root-1";
const toolCallId = "dispatch-1";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "child-1",
    sessionPk,
    parentRunId: ownerRunId,
    retryOf: null,
    sourceToolCallId: toolCallId,
    dispatchIndex: 0,
    primaryAgentId: "primary",
    executingAgentId: "worker",
    executingAgentNameSnapshot: "Researcher",
    agentKind: "subagent",
    task: "Inspect the dispatch behavior",
    status: "running",
    startedAt: 1_000,
    finishedAt: null,
    toolCount: 2,
    resolvedModel: "model-a",
    resolvedEffort: "high",
    result: null,
    error: null,
    ...overrides,
  };
}

function message(overrides: Partial<Message> = {}): Message {
  return {
    sessionPk,
    seq: 1,
    role: "assistant",
    blockType: "tool_call",
    payload: { name: "read", input: { path: "README.md" } },
    toolCallId: "child-tool-1",
    status: "completed",
    toolKind: "read",
    createdAt: 1_000,
    speaker: null,
    ...overrides,
  };
}

function item(overrides: Partial<Extract<ActivityItem, { type: "tool" }>> = {}): Extract<ActivityItem, { type: "tool" }> {
  return {
    type: "tool",
    key: "dispatch-row",
    toolCallId,
    name: "task",
    kind: "task",
    status: "in_progress",
    output: "ordinary tool output",
    path: null,
    input: { prompt: "Inspect" },
    durationMs: null,
    exitCode: null,
    summary: null,
    subagent: null,
    ...overrides,
  };
}

function renderGroup(overrides: Partial<Parameters<typeof AgentDispatchGroup>[0]> = {}) {
  return render(
    <AgentDispatchGroup
      runnerId={runnerId}
      sessionPk={sessionPk}
      ownerRunId={ownerRunId}
      item={item()}
      fallback={<div>ordinary task chip</div>}
      {...overrides}
    />,
  );
}

beforeEach(() => {
  useDelegation.setState({
    bySession: {},
    rootRunBySession: {},
    rosterStateBySession: {},
    transcriptByRun: {},
    transcriptStateByRun: {},
    seenRunsByDispatch: {},
    selectedBySession: {},
  });
  useNav.setState({ rightOpen: false, rightTab: "review" });
});

afterEach(cleanup);

test("linked dispatch replaces its ordinary task chip with a semantic child card", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  useDelegation.setState({ bySession: { [key]: [run()] }, rosterStateBySession: { [key]: { status: "ready", error: null } } });

  renderGroup();

  expect(screen.queryByText("ordinary task chip")).toBeNull();
  expect(screen.getByRole("button", { name: /Open Researcher agent run/i })).toBeTruthy();
  expect(screen.getByText("Inspect the dispatch behavior")).toBeTruthy();
  expect(screen.getByText("Running")).toBeTruthy();
});

test("orders batch cards by persistent dispatch index and shows retry and kind labels", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  const first = run({ runId: "one", dispatchIndex: 0, task: "First task", status: "queued" });
  const second = run({ runId: "two", dispatchIndex: 1, task: "Second task", agentKind: "main-delegate", status: "completed", result: "Done" });
  const retried = run({ runId: "three", dispatchIndex: 2, task: "Third task", status: "failed", error: "Timed out" });
  const retryTip = run({ runId: "four", retryOf: "three", dispatchIndex: 2, task: "Third task retry", status: "interrupted", error: "Interrupted" });
  useDelegation.setState({ bySession: { [key]: [retryTip, second, retried, first] }, rosterStateBySession: { [key]: { status: "ready", error: null } } });

  renderGroup();

  expect(screen.getAllByRole("button", { name: /Open .* agent run/i }).map((card) => card.textContent)).toEqual([
    expect.stringContaining("First task"),
    expect.stringContaining("Second task"),
    expect.stringContaining("Third task retry"),
  ]);
  expect(screen.getByText("Retry 2")).toBeTruthy();
  expect(screen.getAllByText("Subagent")).toHaveLength(2);
  expect(screen.getByText("Main agent")).toBeTruthy();
});

test("renders every terminal and active state with real running activity only", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  const runs = [
    run({ runId: "queued", dispatchIndex: 0, task: "Queued task", status: "queued" }),
    run({ runId: "running", dispatchIndex: 1, task: "Running task", status: "running" }),
    run({ runId: "completed", dispatchIndex: 2, task: "Completed task", status: "completed", result: "Completed report" }),
    run({ runId: "failed", dispatchIndex: 3, task: "Failed task", status: "failed", error: "Failure report" }),
    run({ runId: "cancelled", dispatchIndex: 4, task: "Cancelled task", status: "cancelled" }),
    run({ runId: "interrupted", dispatchIndex: 5, task: "Interrupted task", status: "interrupted" }),
  ];
  useDelegation.setState({
    bySession: { [key]: runs },
    rosterStateBySession: { [key]: { status: "ready", error: null } },
    transcriptByRun: {
      [delegationRunKey(runnerId, sessionPk, "running")]: [
        message({ blockType: "text", toolCallId: null, status: null, toolKind: null, payload: { text: "Persisted child activity" } }),
      ],
    },
  });

  renderGroup();

  for (const status of ["Queued", "Running", "Completed", "Failed", "Cancelled", "Interrupted"]) {
    expect(screen.getByText(status)).toBeTruthy();
  }
  expect(screen.getAllByText("Persisted child activity")).toHaveLength(1);
  expect(screen.queryByText(/next step/i)).toBeNull();
  expect(screen.getByText("Completed report")).toBeTruthy();
  expect(screen.getByText("Failure report")).toBeTruthy();
  expect(screen.getByText("Cancelled before completion.")).toBeTruthy();
  expect(screen.getByText("Interrupted before completion.")).toBeTruthy();
});

test("keeps a card when its child transcript is unavailable", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  const child = run({ status: "completed", result: "Durable report" });
  useDelegation.setState({
    bySession: { [key]: [child] },
    rosterStateBySession: { [key]: { status: "ready", error: null } },
    transcriptStateByRun: { [delegationRunKey(runnerId, sessionPk, child.runId)]: { status: "error", error: "Transcript unavailable" } },
  });

  renderGroup();

  expect(screen.getByText("Researcher")).toBeTruthy();
  expect(screen.getByText("Durable report")).toBeTruthy();
});

test("uses stable loading and roster-error card states before metadata exists", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  useDelegation.setState({ rosterStateBySession: { [key]: { status: "loading", error: null } } });
  const { rerender } = renderGroup();
  expect(screen.getByRole("status", { name: "Loading agent run" })).toBeTruthy();
  expect(screen.queryByText("ordinary task chip")).toBeNull();

  act(() => useDelegation.setState({ rosterStateBySession: { [key]: { status: "error", error: "offline" } } }));
  rerender(
    <AgentDispatchGroup runnerId={runnerId} sessionPk={sessionPk} ownerRunId={ownerRunId} item={item()} fallback={<div>ordinary task chip</div>} />,
  );
  expect(screen.getByText("Agent runs could not be loaded.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry loading agent runs" })).toBeTruthy();
});

test("retains stale cards during a roster refresh error and marks vanished slots unavailable after a successful roster", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  const stale = run({ runId: "stale", status: "completed", result: "Still visible" });
  useDelegation.setState({
    bySession: { [key]: [stale] },
    rosterStateBySession: { [key]: { status: "error", error: "refresh failed" } },
    seenRunsByDispatch: { [key]: { [dispatchSlotKey(ownerRunId, toolCallId, 2)]: ["gone"] } },
  });

  const { rerender } = renderGroup();

  expect(screen.getByText("Still visible")).toBeTruthy();
  expect(screen.getByText("Could not refresh agent runs.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry loading agent runs" })).toBeTruthy();

  act(() => useDelegation.setState({ rosterStateBySession: { [key]: { status: "ready", error: null } } }));
  rerender(
    <AgentDispatchGroup runnerId={runnerId} sessionPk={sessionPk} ownerRunId={ownerRunId} item={item()} fallback={<div>ordinary task chip</div>} />,
  );
  expect(screen.getByText("Agent run unavailable")).toBeTruthy();
});

test("keeps ordinary chips for successful terminal admission failures and legacy rows", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  useDelegation.setState({ bySession: { [key]: [] }, rosterStateBySession: { [key]: { status: "ready", error: null } } });

  const { rerender } = renderGroup({ item: item({ status: "failed" }) });
  expect(screen.getByText("ordinary task chip")).toBeTruthy();

  rerender(
    <AgentDispatchGroup
      runnerId={runnerId}
      sessionPk={sessionPk}
      ownerRunId={null}
      item={item({ status: "completed" })}
      fallback={<div>ordinary task chip</div>}
    />,
  );
  expect(screen.getByText("ordinary task chip")).toBeTruthy();
});

test("card click and shared-button keyboard activation open the exact Agents run", () => {
  const key = delegationSessionKey(runnerId, sessionPk);
  useDelegation.setState({ bySession: { [key]: [run({ runId: "exact-run" })] }, rosterStateBySession: { [key]: { status: "ready", error: null } } });
  renderGroup();

  const card = screen.getByRole("button", { name: /Open Researcher agent run/i });
  fireEvent.click(card);
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useNav.getState().rightTab).toBe("agents");
  expect(useDelegation.getState().selectedBySession[key]).toBe("exact-run");

  act(() => {
    useNav.setState({ rightOpen: false, rightTab: "review" });
    useDelegation.setState({ selectedBySession: {} });
  });
  card.focus();
  fireEvent.keyDown(card, { key: "Enter" });
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useDelegation.getState().selectedBySession[key]).toBe("exact-run");

  act(() => {
    useNav.setState({ rightOpen: false, rightTab: "review" });
    useDelegation.setState({ selectedBySession: {} });
  });
  fireEvent.keyDown(card, { key: " " });
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useDelegation.getState().selectedBySession[key]).toBe("exact-run");
});
