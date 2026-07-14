import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentRun } from "@/bindings";

const getChildTranscript = mock(() => Promise.resolve({ status: "ok", data: [] }));
mock.module("@/bindings", () => ({ commands: { getChildTranscript } }));

import { useDelegation, delegationSessionKey } from "@/store-delegation";

const { AgentRunRoster } = await import("./AgentRunRoster");

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
    task: "Inspect the service logs",
    status: "running",
    startedAt: Date.now() - 2_000,
    finishedAt: null,
    toolCount: 3,
    resolvedModel: "model-a",
    resolvedEffort: "high",
    result: null,
    error: null,
    ...overrides,
  };
}

beforeEach(() => useDelegation.setState({ bySession: {}, transcriptByRun: {}, selectedBySession: {} }));
afterEach(cleanup);

test("splits exactly queued/running into Active and terminal runs into Done", () => {
  useDelegation.setState({
    bySession: {
      [delegationSessionKey("local", "s1")]: [
        run({ runId: "queued", status: "queued" }),
        run({ runId: "active", status: "running" }),
        run({ runId: "done", status: "completed" }),
        run({ runId: "failed", status: "failed", error: "tool failed" }),
        run({ runId: "cancelled", status: "cancelled" }),
        run({ runId: "interrupted", status: "interrupted" }),
      ],
    },
  });

  render(<AgentRunRoster runnerId="local" sessionPk="s1" />);

  expect(screen.getByText("Active (2)")).toBeTruthy();
  expect(screen.getByText("Done (4)")).toBeTruthy();
  expect(screen.getAllByText("Subagent").length).toBeGreaterThan(0);
  expect(screen.getAllByText("3 tools").length).toBeGreaterThan(0);
  expect(screen.getByText("tool failed")).toBeTruthy();
});

test("selecting a roster card navigates to its full detail", () => {
  useDelegation.setState({ bySession: { [delegationSessionKey("local", "s1")]: [run()] } });

  render(<AgentRunRoster runnerId="local" sessionPk="s1" />);
  fireEvent.click(screen.getByRole("button", { name: /Researcher/i }));

  expect(useDelegation.getState().selectedBySession[delegationSessionKey("local", "s1")]).toBe("run-1");
});
