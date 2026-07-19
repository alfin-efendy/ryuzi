import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { AgentRun, Message } from "@/bindings";

const cancelChildRun = mock(() => Promise.resolve({ status: "ok", data: null }));
const retryChildRun = mock(() => Promise.resolve({ status: "ok", data: run({ runId: "retry", status: "queued" }) }));
const getChildTranscript = mock(() => Promise.resolve({ status: "ok", data: [] }));
mock.module("@/bindings", () => ({ commands: { cancelChildRun, retryChildRun, getChildTranscript } }));

import { useDelegation, delegationRunKey, delegationSessionKey } from "@/store-delegation";
import { useStore } from "@/store";
import { sessKey } from "@/lib/session-key";

const { AgentRunDetail } = await import("./AgentRunDetail");

function run({ sourceToolCallId = null, dispatchIndex = null, ...overrides }: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "run-1",
    sessionPk: "s1",
    parentRunId: null,
    retryOf: null,
    sourceToolCallId,
    dispatchIndex,
    primaryAgentId: "lead",
    executingAgentId: "worker",
    executingAgentNameSnapshot: "Researcher",
    agentKind: "subagent",
    task: "Inspect logs",
    status: "failed",
    startedAt: 1_000,
    finishedAt: 3_000,
    toolCount: 2,
    resolvedModel: "model-a",
    resolvedEffort: "high",
    result: "Final findings",
    error: "tool failed",
    contextActiveTokens: null,
    contextUsableWindow: null,
    contextPercentLeft: null,
    contextWindow: null,
    cacheReadTokens: null,
    cacheCreationTokens: null,
    outputTokens: null,
    cost: null,
    ...overrides,
  };
}

beforeEach(() => {
  useDelegation.setState({
    bySession: { [delegationSessionKey("local", "s1")]: [run()] },
    transcriptByRun: {
      [delegationRunKey("local", "s1", "run-1")]: [
        {
          seq: 1,
          sessionPk: "s1",
          runId: "run-1",
          role: "assistant",
          blockType: "text",
          payload: { text: "The complete child transcript" },
          toolCallId: null,
          status: null,
          toolKind: null,
          createdAt: 1,
          speaker: null,
        },
      ] as Message[],
    },
    selectedBySession: { [delegationSessionKey("local", "s1")]: "run-1" },
  });
  useStore.setState({ pendingApprovals: [] });
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: mock(() => Promise.resolve()) } });
});
afterEach(cleanup);

test("shows the full transcript, metadata, result, and related changes", () => {
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  expect(screen.getByRole("button", { name: "Back to Agents" })).toBeTruthy();
  expect(screen.getByText("Inspect logs")).toBeTruthy();
  expect(screen.getByText("The complete child transcript")).toBeTruthy();
  expect(screen.getByText("tool failed")).toBeTruthy();
  expect(screen.getByText("Final findings")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Related changes" })).toBeTruthy();
});

test("labels a main delegate as Main agent", () => {
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run({ agentKind: "main-delegate" })} onRelatedChanges={() => {}} />);

  expect(screen.getByText("Main agent")).toBeTruthy();
  expect(screen.queryByText("Subagent")).toBeNull();
});

test("keeps detail metadata and result available when retrying a failed transcript load", async () => {
  const key = delegationRunKey("local", "s1", "run-1");
  useDelegation.setState({
    transcriptStateByRun: { [key]: { status: "error", error: "Transcript service unavailable" } },
  });

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  expect(screen.getByText("Inspect logs")).toBeTruthy();
  expect(screen.getByText("Final findings")).toBeTruthy();
  expect(screen.getByText("Transcript service unavailable")).toBeTruthy();
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Retry transcript" }));
    await Promise.resolve();
  });
  expect(getChildTranscript).toHaveBeenCalledWith("local", "s1", "run-1");
});

test("places the run identity and valid action in the detail header", () => {
  const active = render(
    <AgentRunDetail
      runnerId="local"
      sessionPk="s1"
      run={run({ status: "running", result: null, error: null })}
      onRelatedChanges={() => {}}
    />,
  );
  const header = active.container.querySelector("header");
  expect(header).toBeTruthy();
  const headerContent = within(header!);

  expect(headerContent.getByRole("button", { name: "Back to Agents" })).toBeTruthy();
  expect(headerContent.getByRole("img", { name: "Agent avatar for Researcher" })).toBeTruthy();
  for (const label of ["Researcher", "Subagent", "Running", "2 tools", "2s", "model-a", "high"]) {
    expect(headerContent.getByText(label)).toBeTruthy();
  }
  expect(headerContent.getByRole("button", { name: "Stop" })).toBeTruthy();
  expect(headerContent.queryByRole("button", { name: "Retry" })).toBeNull();
  active.unmount();

  const failed = render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);
  expect(
    failed.container.querySelector("header") && within(failed.container.querySelector("header")!).getByRole("button", { name: "Retry" }),
  ).toBeTruthy();
  expect(within(failed.container.querySelector("header")!).queryByRole("button", { name: "Stop" })).toBeNull();
});

test("renders a grandchild dispatch card inside the child transcript", () => {
  const parent = run();
  const grandchild = run({
    runId: "grandchild-1",
    parentRunId: parent.runId,
    sourceToolCallId: "nested-task",
    dispatchIndex: 0,
    executingAgentNameSnapshot: "Grandchild",
    task: "Inspect nested work",
    status: "completed",
    result: "Nested findings",
    error: null,
  });
  const sessionKey = delegationSessionKey("local", "s1");
  useDelegation.setState({
    bySession: { [sessionKey]: [parent, grandchild] },
    rosterStateBySession: { [sessionKey]: { status: "ready", error: null } },
    transcriptByRun: {
      [delegationRunKey("local", "s1", parent.runId)]: [
        {
          seq: 1,
          sessionPk: "s1",
          runId: parent.runId,
          role: "assistant",
          blockType: "tool_call",
          payload: { name: "task", input: { prompt: "Inspect nested work" } },
          toolCallId: "nested-task",
          status: "completed",
          toolKind: "task",
          createdAt: 1,
          speaker: null,
        },
      ],
    },
  });

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={parent} onRelatedChanges={() => {}} />);

  expect(screen.getByRole("button", { name: "Open Grandchild agent run" })).toBeTruthy();
  expect(screen.getByText("Inspect nested work")).toBeTruthy();
  expect(screen.queryByText("ordinary task chip")).toBeNull();
});

test("renders and resolves only approvals for the exact runner, session, and run", () => {
  const resolveApproval = mock(() => Promise.resolve());
  useStore.setState({
    resolveApproval,
    pendingApprovals: [
      {
        runnerId: "local",
        sessionPk: "s1",
        runId: "run-1",
        requestId: "exact",
        tool: "bash",
        summary: "exact approval",
        kind: "tool",
        input: { command: "pwd" },
        principal: null,
      },
      {
        runnerId: "local",
        sessionPk: "s1",
        runId: "other-run",
        requestId: "other-run",
        tool: "bash",
        summary: "other run approval",
        kind: "tool",
        input: { command: "whoami" },
        principal: null,
      },
      {
        runnerId: "remote",
        sessionPk: "s1",
        runId: "run-1",
        requestId: "other-runner",
        tool: "bash",
        summary: "other runner approval",
        kind: "tool",
        input: { command: "hostname" },
        principal: null,
      },
      {
        runnerId: "local",
        sessionPk: "other-session",
        runId: "run-1",
        requestId: "other-session",
        tool: "bash",
        summary: "other session approval",
        kind: "tool",
        input: { command: "date" },
        principal: null,
      },
    ],
  });

  expect(useStore.getState().pendingApprovals).toHaveLength(4);
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  expect(screen.getByText("pwd")).toBeTruthy();
  expect(screen.queryByText("whoami")).toBeNull();
  expect(screen.queryByText("hostname")).toBeNull();
  expect(screen.queryByText("date")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Allow" }));

  expect(resolveApproval).toHaveBeenCalledWith("local", "run-1", "exact", { decision: "allowOnce", scope: null, payload: null });
});

test("back clears selection and copy writes the final result", () => {
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "Copy result" }));
  fireEvent.click(screen.getByRole("button", { name: "Back to Agents" }));

  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Final findings");
  expect(useDelegation.getState().selectedBySession[delegationSessionKey("local", "s1")]).toBeNull();
});

test("active runs expose Stop while failed runs expose Retry", () => {
  const active = render(
    <AgentRunDetail
      runnerId="local"
      sessionPk="s1"
      run={run({ status: "running", result: null, error: null })}
      onRelatedChanges={() => {}}
    />,
  );
  expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
  active.unmount();

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);
  expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
});

test("ring reflects the run's own context usage, not the session's", () => {
  useStore.setState({
    contextUsage: {
      [sessKey("local", "s1")]: {
        activeTokens: 9,
        usableWindow: 10,
        percentLeft: 5,
        contextWindow: 10,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        outputTokens: 0,
      },
    },
    runContextUsage: {
      [delegationRunKey("local", "s1", "run-1")]: {
        activeTokens: 4000,
        usableWindow: 120000,
        percentLeft: 60,
        contextWindow: 200000,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        outputTokens: 0,
      },
    },
  });
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);
  // Ring shows USED = 100 - percentLeft. Run usage (60 left → 40% used), never the session's (5 left → 95%).
  expect(screen.getByText("40%")).toBeTruthy();
  expect(screen.queryByText("95%")).toBeNull();
  expect(screen.getByTitle(/Sub-agent context/)).toBeTruthy();
});

test("ring falls back to the persisted row usage when there is no live event", () => {
  useStore.setState({ contextUsage: {}, runContextUsage: {} });
  render(
    <AgentRunDetail
      runnerId="local"
      sessionPk="s1"
      run={run({ contextActiveTokens: 7000, contextUsableWindow: 100000, contextPercentLeft: 70 })}
      onRelatedChanges={() => {}}
    />,
  );
  expect(screen.getByText("30%")).toBeTruthy();
});

test("ring is hidden when the run has no usage yet", () => {
  useStore.setState({ contextUsage: {}, runContextUsage: {} });
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run({ contextPercentLeft: null })} onRelatedChanges={() => {}} />);
  expect(screen.queryByTitle(/Sub-agent context/)).toBeNull();
});

test("context ring opens a Context+Cost popover with the run's own values", () => {
  useStore.setState({
    runContextUsage: {
      [delegationRunKey("local", "s1", "run-1")]: {
        activeTokens: 100,
        usableWindow: 900,
        percentLeft: 40,
        contextWindow: 1000,
        cacheReadTokens: 30,
        cacheCreationTokens: 4,
        outputTokens: 12,
      },
    },
    runCost: {
      [delegationRunKey("local", "s1", "run-1")]: {
        totalUsd: 0.5,
        models: [{ model: "gpt-5.6-terra", input: 10, output: 4, cacheRead: 0, cacheCreation: 0, usd: 0.5 }],
      },
    },
  });

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: /context and cost/i }));

  expect(screen.getByText("gpt-5.6-terra")).toBeTruthy();
  expect(screen.getByText("Full window")).toBeTruthy();
});

test("context ring toggles the popover closed on a second trigger click", () => {
  useStore.setState({
    runContextUsage: {
      [delegationRunKey("local", "s1", "run-1")]: {
        activeTokens: 100,
        usableWindow: 900,
        percentLeft: 40,
        contextWindow: 1000,
        cacheReadTokens: 30,
        cacheCreationTokens: 4,
        outputTokens: 12,
      },
    },
    runCost: {},
  });

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  const trigger = screen.getByRole("button", { name: /context and cost/i });
  fireEvent.click(trigger);
  expect(screen.getByText("Full window")).toBeTruthy();

  // jsdom never fires the outside-mousedown that MenuPanel listens for, so
  // this second click only exercises the trigger's own open/close toggle.
  fireEvent.click(trigger);
  expect(screen.queryByText("Full window")).toBeNull();
});
