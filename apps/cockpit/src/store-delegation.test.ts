import { beforeEach, expect, spyOn, test } from "bun:test";
import { commands, type AgentRun, type AgentRunRosterInfo, type Message } from "./bindings";
import { useDelegation, delegationRunKey, delegationSessionKey } from "./store-delegation";

const local = "local";
const remote = "remote-1";
const sessionPk = "shared";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: "run-1",
    sessionPk,
    parentRunId: "root-1",
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
    ...overrides,
  };
}

function roster(runs: AgentRun[] = [run()]): AgentRunRosterInfo {
  return { rootRunId: "root-1", runs };
}

function message(overrides: Partial<Message> = {}): Message {
  return {
    seq: 1,
    sessionPk,
    role: "assistant",
    blockType: "tool_call",
    payload: { name: "read", input: { path: "README.md" } },
    toolCallId: "tool-1",
    status: "pending",
    toolKind: "read",
    createdAt: 1,
    speaker: null,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
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
});

test("keeps rooted child rosters isolated when two runners share a session pk", async () => {
  const getChildRuns = spyOn(commands, "getChildRuns").mockImplementation(async (runnerId) => ({
    status: "ok",
    data: roster([run({ runId: runnerId === local ? "local-run" : "remote-run" })]),
  }));
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });

  await useDelegation.getState().load(local, sessionPk);
  await useDelegation.getState().load(remote, sessionPk);

  expect(useDelegation.getState().rootRunBySession[delegationSessionKey(local, sessionPk)]).toBe("root-1");
  expect(useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)]?.[0]?.runId).toBe("local-run");
  expect(useDelegation.getState().bySession[delegationSessionKey(remote, sessionPk)]?.[0]?.runId).toBe("remote-run");
  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("deduplicates concurrent roster loads for the exact runner/session scope", async () => {
  const pending = deferred<{ status: "ok"; data: AgentRunRosterInfo }>();
  const getChildRuns = spyOn(commands, "getChildRuns").mockImplementation(() => pending.promise as never);
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });

  const first = useDelegation.getState().load(local, sessionPk);
  const second = useDelegation.getState().load(local, sessionPk);
  expect(first).toBe(second);
  expect(getChildRuns).toHaveBeenCalledTimes(1);
  expect(useDelegation.getState().rosterStateBySession[delegationSessionKey(local, sessionPk)]).toEqual({ status: "loading", error: null });
  pending.resolve({ status: "ok", data: roster() });
  await first;

  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("records linked runs by dispatch identity and hydrates only active child transcripts", async () => {
  const active = run({ runId: "active", status: "running", dispatchIndex: 2 });
  const completed = run({ runId: "done", status: "completed", dispatchIndex: 3 });
  const getChildRuns = spyOn(commands, "getChildRuns").mockResolvedValue({ status: "ok", data: roster([active, completed]) });
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });

  await useDelegation.getState().load(local, sessionPk);
  await Promise.resolve();

  const key = delegationSessionKey(local, sessionPk);
  expect(useDelegation.getState().rosterStateBySession[key]).toEqual({ status: "ready", error: null });
  expect(useDelegation.getState().seenRunsByDispatch[key]["root-1\u0000dispatch-1\u00002"]).toEqual(["active"]);
  expect(getChildTranscript).toHaveBeenCalledWith(local, sessionPk, "active");
  expect(getChildTranscript).not.toHaveBeenCalledWith(local, sessionPk, "done");

  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("buffers child message events by runner/session/run and merges terminal tool updates", () => {
  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-a",
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "read", input: { path: "README.md" } },
      tool_call_id: "tool-1",
      status: "pending",
      tool_kind: "read",
      speaker: null,
    },
    local,
  );
  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-b",
      seq: 1,
      role: "assistant",
      block_type: "text",
      payload: { text: "other child" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    local,
  );
  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-a",
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "read", input: { path: "README.md" }, output: "file contents" },
      tool_call_id: "tool-1",
      status: "completed",
      tool_kind: "read",
      speaker: null,
    },
    local,
  );

  const a = useDelegation.getState().transcriptByRun[delegationRunKey(local, sessionPk, "run-a")];
  const b = useDelegation.getState().transcriptByRun[delegationRunKey(local, sessionPk, "run-b")];
  expect(a).toHaveLength(1);
  expect(a?.[0]).toMatchObject({ status: "completed", payload: { output: "file contents" } });
  expect(b?.[0]?.payload).toEqual({ text: "other child" });
});

test("keeps newer live transcript rows and equal-identity live tool updates when hydration resolves", async () => {
  const pending = deferred<{ status: "ok"; data: Message[] }>();
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockImplementation(() => pending.promise as never);
  const loading = useDelegation.getState().loadTranscript(local, sessionPk, "run-1");

  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-1",
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "read", output: "fresh" },
      tool_call_id: "tool-1",
      status: "completed",
      tool_kind: "read",
      speaker: null,
    },
    local,
  );
  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-1",
      seq: 2,
      role: "assistant",
      block_type: "text",
      payload: { text: "newer live text" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    local,
  );
  pending.resolve({ status: "ok", data: [message({ payload: { name: "read" }, status: "pending" })] });
  await loading;

  const rows = useDelegation.getState().transcriptByRun[delegationRunKey(local, sessionPk, "run-1")];
  expect(rows).toHaveLength(2);
  expect(rows[0]).toMatchObject({ toolCallId: "tool-1", status: "completed", payload: { output: "fresh" } });
  expect(rows[1]?.payload).toEqual({ text: "newer live text" });
  getChildTranscript.mockRestore();
});

test("keeps a live equal-sequence child row when hydration resolves", async () => {
  const pending = deferred<{ status: "ok"; data: Message[] }>();
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockImplementation(() => pending.promise as never);
  const loading = useDelegation.getState().loadTranscript(local, sessionPk, "run-1");

  useDelegation.getState().applyCoreEvent(
    {
      kind: "agentRunMessage",
      session_pk: sessionPk,
      run_id: "run-1",
      seq: 1,
      role: "assistant",
      block_type: "text",
      payload: { text: "live wins" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    local,
  );
  pending.resolve({ status: "ok", data: [message({ blockType: "text", payload: { text: "stale snapshot" }, toolCallId: null, status: null, toolKind: null })] });
  await loading;

  expect(useDelegation.getState().transcriptByRun[delegationRunKey(local, sessionPk, "run-1")]?.[0]?.payload).toEqual({ text: "live wins" });
  getChildTranscript.mockRestore();
});

test("agent-run metadata changes refresh only the scoped roster, not a selected transcript", async () => {
  useDelegation.setState({
    selectedBySession: { [delegationSessionKey(local, sessionPk)]: "run-1" },
    transcriptByRun: { [delegationRunKey(local, sessionPk, "run-1")]: [message()] },
    transcriptStateByRun: { [delegationRunKey(local, sessionPk, "run-1")]: { status: "ready", error: null } },
  });
  const getChildRuns = spyOn(commands, "getChildRuns").mockResolvedValue({ status: "ok", data: roster([run({ status: "completed" })]) });
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });

  useDelegation
    .getState()
    .applyCoreEvent({ kind: "agentRunChanged", session_pk: sessionPk, run_id: "run-1", parent_run_id: "root-1", status: "completed" }, local);
  await Promise.resolve();

  expect(getChildRuns).toHaveBeenCalledWith(local, sessionPk);
  expect(getChildTranscript).not.toHaveBeenCalled();
  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("reconciles a completion event that arrives during roster hydration", async () => {
  const staleRoster = deferred<{ status: "ok"; data: AgentRunRosterInfo }>();
  const currentRoster = deferred<{ status: "ok"; data: AgentRunRosterInfo }>();
  const getChildRuns = spyOn(commands, "getChildRuns")
    .mockImplementationOnce(() => staleRoster.promise as never)
    .mockImplementationOnce(() => currentRoster.promise as never);
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });

  const initialLoad = useDelegation.getState().load(local, sessionPk);
  useDelegation
    .getState()
    .applyCoreEvent(
      { kind: "agentRunChanged", session_pk: sessionPk, run_id: "run-1", parent_run_id: "root-1", status: "completed" },
      local,
    );
  staleRoster.resolve({ status: "ok", data: roster([run({ status: "running" })]) });
  await initialLoad;

  expect(getChildRuns).toHaveBeenCalledTimes(2);
  currentRoster.resolve({ status: "ok", data: roster([run({ status: "completed", finishedAt: 2_000, result: "Done" })]) });
  await Promise.resolve();

  const runs = useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)];
  expect(runs).toEqual([run({ status: "completed", finishedAt: 2_000, result: "Done" })]);
  expect(getChildRuns).toHaveBeenCalledTimes(2);
  getChildRuns.mockRestore();
  getChildTranscript.mockRestore();
});

test("retry appends the returned attempt and selects it", async () => {
  const retried = run({ runId: "run-2", retryOf: "run-1", status: "queued" });
  const retryChildRun = spyOn(commands, "retryChildRun").mockResolvedValue({ status: "ok", data: retried });
  const getChildTranscript = spyOn(commands, "getChildTranscript").mockResolvedValue({ status: "ok", data: [] });
  useDelegation.setState({ bySession: { [delegationSessionKey(local, sessionPk)]: [run({ status: "failed" })] } });

  await useDelegation.getState().retry(local, sessionPk, "run-1");

  expect(useDelegation.getState().bySession[delegationSessionKey(local, sessionPk)]?.map((entry) => entry.runId)).toEqual([
    "run-1",
    "run-2",
  ]);
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
