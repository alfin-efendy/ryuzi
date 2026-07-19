import { expect, test } from "bun:test";
import type { AgentRun, Message } from "../bindings";
import { agentRunStatusPresentation, linkedDispatchSlots, projectAgentRunPreview } from "./agent-runs";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "child-1",
    sessionPk: "session-1",
    parentRunId: "owner-1",
    retryOf: null,
    sourceToolCallId: "dispatch-1",
    dispatchIndex: 0,
    primaryAgentId: "primary",
    executingAgentId: "worker",
    executingAgentNameSnapshot: "Researcher",
    agentKind: "subagent",
    task: "Investigate the failure",
    status: "running",
    startedAt: 1_000,
    finishedAt: null,
    toolCount: 2,
    resolvedModel: "model-a",
    resolvedEffort: "high",
    result: null,
    error: null,
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

function message(overrides: Partial<Message> = {}): Message {
  return {
    seq: 1,
    sessionPk: "session-1",
    runId: null,
    role: "assistant",
    blockType: "tool_call",
    payload: { name: "read", input: { path: "README.md" } },
    toolCallId: "tool-1",
    status: "completed",
    toolKind: "read",
    createdAt: 1_000,
    speaker: null,
    ...overrides,
  };
}

test("presents every agent-run status with its shared label and tone", () => {
  expect(agentRunStatusPresentation("queued")).toEqual({ label: "Queued", tone: "text-muted-foreground" });
  expect(agentRunStatusPresentation("running")).toEqual({ label: "Running", tone: "text-primary" });
  expect(agentRunStatusPresentation("completed")).toEqual({ label: "Completed", tone: "text-emerald-600 dark:text-emerald-400" });
  expect(agentRunStatusPresentation("failed")).toEqual({ label: "Failed", tone: "text-destructive" });
  expect(agentRunStatusPresentation("cancelled")).toEqual({ label: "Cancelled", tone: "text-muted-foreground" });
  expect(agentRunStatusPresentation("interrupted")).toEqual({ label: "Interrupted", tone: "text-amber-700 dark:text-amber-400" });
});

test("links dispatch slots only to the matching owner and tool call", () => {
  const runs = [
    run({ runId: "matched", parentRunId: "owner-1", sourceToolCallId: "dispatch-1", dispatchIndex: 0 }),
    run({ runId: "other-owner", parentRunId: "owner-2", sourceToolCallId: "dispatch-1", dispatchIndex: 0 }),
    run({ runId: "other-call", parentRunId: "owner-1", sourceToolCallId: "dispatch-2", dispatchIndex: 0 }),
  ];

  expect(linkedDispatchSlots("owner-1", "dispatch-1", runs).map((slot) => slot.current.runId)).toEqual(["matched"]);
});

test("sorts batch dispatch slots by dispatch index", () => {
  const runs = [
    run({ runId: "third", dispatchIndex: 2, startedAt: 300 }),
    run({ runId: "first", dispatchIndex: 0, startedAt: 900 }),
    run({ runId: "second", dispatchIndex: 1, startedAt: 100 }),
  ];

  expect(linkedDispatchSlots("owner-1", "dispatch-1", runs).map((slot) => slot.dispatchIndex)).toEqual([0, 1, 2]);
});

test("selects the retry tip while preserving the original dispatch slot", () => {
  const first = run({ runId: "attempt-1", status: "failed", dispatchIndex: 4, startedAt: 10 });
  const retried = run({ runId: "attempt-2", retryOf: "attempt-1", dispatchIndex: 4, startedAt: 20 });

  const [slot] = linkedDispatchSlots("owner-1", "dispatch-1", [retried, first]);
  expect(slot).toMatchObject({ dispatchIndex: 4, current: { runId: "attempt-2" }, attemptNumber: 2 });
  expect(slot.attempts.map((attempt) => attempt.runId)).toEqual(["attempt-1", "attempt-2"]);
});

test("never links legacy runs without a complete dispatch identity", () => {
  const legacy = run({ sourceToolCallId: null, dispatchIndex: null });
  expect(linkedDispatchSlots("owner-1", "dispatch-1", [legacy])).toEqual([]);
  expect(linkedDispatchSlots(null, "dispatch-1", [run()])).toEqual([]);
  expect(linkedDispatchSlots("owner-1", null, [run()])).toEqual([]);
});

test("projects only the latest three live activity items plus the latest persisted excerpt", () => {
  const preview = projectAgentRunPreview(run(), [
    message({ seq: 1, toolCallId: "a" }),
    message({ seq: 2, toolCallId: "b" }),
    message({ seq: 3, toolCallId: "c" }),
    message({ seq: 4, toolCallId: "d" }),
    message({ seq: 5, blockType: "status", role: "system", payload: { summary: "Checking the final result" }, toolCallId: null }),
  ]);

  expect(preview.activities.map((item) => item.key)).toEqual(["s3", "s4", "s5"]);
  expect(preview.excerpt).toBe("Checking the final result");
});

test("projects a bounded direct terminal excerpt and a reportless completion", () => {
  const result = `first line\n${"x".repeat(400)}`;
  const completed = projectAgentRunPreview(run({ status: "completed", result }), []);
  expect(completed.excerpt).toStartWith("first line ");
  expect(completed.excerpt).not.toContain("\n");
  expect(completed.excerpt!.length).toBeLessThanOrEqual(280);
  expect(projectAgentRunPreview(run({ status: "completed", result: null }), []).excerpt).toBe("Completed with no report.");
  expect(projectAgentRunPreview(run({ status: "failed", error: "request timed out" }), []).excerpt).toBe("request timed out");
});
