import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentRun, Message } from "@/bindings";

const cancelChildRun = mock(() => Promise.resolve({ status: "ok", data: null }));
const retryChildRun = mock(() => Promise.resolve({ status: "ok", data: run({ runId: "retry", status: "queued" }) }));
const getChildTranscript = mock(() => Promise.resolve({ status: "ok", data: [] }));
mock.module("@/bindings", () => ({ commands: { cancelChildRun, retryChildRun, getChildTranscript } }));

import { useDelegation, delegationRunKey, delegationSessionKey } from "@/store-delegation";

const { AgentRunDetail } = await import("./AgentRunDetail");

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "run-1",
    sessionPk: "s1",
    parentRunId: null,
    retryOf: null,
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
    ...overrides,
  };
}

beforeEach(() => {
  useDelegation.setState({
    bySession: { [delegationSessionKey("local", "s1")]: [run()] },
    transcriptByRun: {
      [delegationRunKey("local", "s1", "run-1")]: [
        { seq: 1, sessionPk: "s1", role: "assistant", blockType: "text", payload: { text: "The complete child transcript" }, toolCallId: null, status: null, toolKind: null, createdAt: 1, speaker: null },
      ] as Message[],
    },
    selectedBySession: { [delegationSessionKey("local", "s1")]: "run-1" },
  });
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

test("back clears selection and copy writes the final result", () => {
  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "Copy result" }));
  fireEvent.click(screen.getByRole("button", { name: "Back to Agents" }));

  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Final findings");
  expect(useDelegation.getState().selectedBySession[delegationSessionKey("local", "s1")]).toBeNull();
});

test("active runs expose Stop while failed runs expose Retry", () => {
  const active = render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run({ status: "running", result: null, error: null })} onRelatedChanges={() => {}} />);
  expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
  active.unmount();

  render(<AgentRunDetail runnerId="local" sessionPk="s1" run={run()} onRelatedChanges={() => {}} />);
  expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
});
