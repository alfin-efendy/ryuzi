import { beforeEach, expect, spyOn, test } from "bun:test";
import { commands, type AgentRun, type Message } from "./bindings";
import { useStore } from "./store";
import { useDelegation, delegationRunKey, delegationSessionKey } from "./store-delegation";

const local = "local";
const remote = "remote-1";
const sessionPk = "shared";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "run-1",
    sessionPk,
    parentRunId: null,
    retryOf: null,
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
    ...overrides,
  };
}

beforeEach(() => {
  useDelegation.setState({ bySession: {}, transcriptByRun: {}, selectedBySession: {} });
});

test("keeps child runs isolated when two runners share a session pk", async () => {
  const getChildRuns = spyOn(commands, "getChildRuns").mockImplementation(async (runnerId) => ({
    status: "ok",
    data: [run({ runId: runnerId === local ? "local-run" : "remote-run" })],
  }));

  await useDelegation.getState().load(local, sessionPk);
  await useDelegation.getState().load(remote, sessionPk);

  expect(useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)]?.[0]?.runId).toBe("local-run");
  expect(useDelegation.getState().bySession[delegationSessionKey(remote, sessionPk)]?.[0]?.runId).toBe("remote-run");
  getChildRuns.mockRestore();
});

test("store core-event bridge refetches child metadata for its scoped run", async () => {
  const getChildRuns = spyOn(commands, "getChildRuns").mockResolvedValue({ status: "ok", data: [run()] });

  useStore.getState().applyCoreEvent({ kind: "agentRunChanged", session_pk: sessionPk, run_id: "run-1", parent_run_id: null, status: "running" }, local);
  await Promise.resolve();

  expect(getChildRuns).toHaveBeenCalledWith(local, sessionPk);
  getChildRuns.mockRestore();
});

test("agent-run event reloads only its runner/session metadata and selected transcript", async () => {
  const getChildRuns = spyOn(commands, "getChildRuns").mockResolvedValue({ status: "ok", data: [run()] });
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });
  useDelegation.getState().select(local, sessionPk, "run-1");

  useDelegation.getState().applyCoreEvent({ kind: "agentRunChanged", session_pk: sessionPk, run_id: "run-1", parent_run_id: null, status: "completed" }, local);
  await Promise.resolve();
  await Promise.resolve();

  expect(getChildRuns).toHaveBeenCalledWith(local, sessionPk);
  expect(getChildTranscript).toHaveBeenCalledWith(local, sessionPk, "run-1");
  expect(getChildRuns).not.toHaveBeenCalledWith(remote, sessionPk);
  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("retry appends the returned attempt and selects it", async () => {
  const retried = run({ runId: "run-2", retryOf: "run-1", status: "queued" });
  const retryChildRun = spyOn(commands, "retryChildRun").mockResolvedValue({ status: "ok", data: retried });
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });
  useDelegation.setState({ bySession: { [delegationSessionKey(local, sessionPk)]: [run({ status: "failed" })] } });

  await useDelegation.getState().retry(local, sessionPk, "run-1");

  expect(useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)]?.map((entry) => entry.runId)).toEqual(["run-1", "run-2"]);
  expect(useDelegation.getState().selectedBySession[delegationSessionKey(local, sessionPk)]).toBe("run-2");
  retryChildRun.mockRestore();
  getChildTranscript.mockRestore();
});

test("failed cancellation restores the previous status", async () => {
  const cancelChildRun = spyOn(commands, "cancelChildRun").mockResolvedValue({ status: "error", error: { message: "offline" } });
  useDelegation.setState({ bySession: { [delegationSessionKey(local, sessionPk)]: [run()] } });

  await useDelegation.getState().stop(local, sessionPk, "run-1");

  expect(useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)]?.[0]?.status).toBe("running");
  cancelChildRun.mockRestore();
});

test("stores a full child transcript under its runner/session/run key", async () => {
  const messages: Message[] = [
    {
      seq: 1,
      sessionPk,
      role: "assistant",
      blockType: "text",
      payload: { text: "The full child transcript" },
      toolCallId: null,
      status: null,
      toolKind: null,
      createdAt: 1,
      speaker: null,
    },
  ];
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: messages } as never);

  await useDelegation.getState().loadTranscript(local, sessionPk, "run-1");

  expect(useDelegation.getState().transcriptByRun[delegationRunKey(local, sessionPk, "run-1")]).toEqual(messages);
  getChildTranscript.mockRestore();
});
