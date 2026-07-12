import { afterAll, test, expect, mock, spyOn } from "bun:test";
import { useStore, markFocusedSessionReadOnEvent, drainQueueOnEvent } from "./store";
import { commands } from "./bindings";
import { useNative } from "./store-native";
import { useAgent } from "./store-agent";
import { useUi } from "./store-ui";
import type { QueuedMessage } from "./lib/queue";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const k1 = sessKey(LOCAL_RUNNER, "s1");
const k2 = sessKey(LOCAL_RUNNER, "s2");

// The sendNextQueued suite below overwrites the store's real `send` action with
// stubs via `setState({ send })`. `useStore` is a process-global singleton shared
// with every other test file in the same `bun test` run, so leaving a stub in
// place leaks into later files (e.g. store-orch.test.ts's send-routing tests,
// which then invoke the stub instead of the real orch-steer path). Snapshot the
// genuine implementation at module load and reinstate it once this file is done.
const realSendImpl = useStore.getState().send;
afterAll(() => {
  useStore.setState({ send: realSendImpl });
});

// refresh() (called fire-and-forget by start/startChat/send/stop/end/cloneProject, and by the
// result/error event handlers, and awaited directly by send()) always fans out to
// listGateways too. Mock it wherever listProjects/listSessions are given RESOLVING mocks
// (never-resolving stubs never reach it, since refresh() awaits listProjects first) so the
// unmocked Tauri IPC call doesn't reject the chain.
function mockGateways() {
  return spyOn(commands, "listGateways").mockResolvedValue({ status: "ok", data: [] });
}

function reset() {
  useStore.setState({
    projects: [],
    sessions: [],
    transcripts: {},
    pendingApprovals: [],
    focusedSession: null,
    selectedProjectId: null,
    lastSeq: {},
    loaded: {},
    contextUsage: {},
    projectRuntimeById: {},
    sessionRuntimeById: {},
    sessionCost: {},
    queued: {},
  });
}

test("composer_model_has_one_project_runtime_source", () => {
  const state = useStore.getState() as unknown as Record<string, unknown>;
  expect(state.setProjectModel).toBeUndefined();
  expect(state.setProjectRuntime).toBeFunction();
});

const runtimeSnapshot = {
  projectId: "p1",
  model: "openai/gpt-5",
  storedEffort: "high",
  effectiveEffort: "high",
  effectiveEffortLabel: "High",
  effectiveSource: "project" as const,
  storedEffortStatus: "valid" as const,
  modelInfo: null,
};

function projectSnapshot(projectId = "p1") {
  return {
    projectId,
    name: projectId,
    workdir: `C:/${projectId}`,
    source: null,
    harness: "native",
    model: "old",
    effort: "low",
    permMode: "default" as const,
    createdAt: null,
    isGit: true,
  };
}

function deferredResult() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("runtime_persistence_is_strictly_queued_while_latest_intent_paints", async () => {
  reset();
  useStore.setState({ projects: [projectSnapshot()], projectRuntimeById: { p1: runtimeSnapshot } });
  const a = deferredResult();
  const b = deferredResult();
  let persisted = { model: "old", effort: "low" };
  const update = spyOn(commands, "updateProjectRuntime")
    .mockImplementationOnce(async () => {
      const result = (await a.promise) as { status: "ok"; data: typeof runtimeSnapshot };
      persisted = { model: result.data.model ?? "", effort: result.data.storedEffort ?? "" };
      return result;
    })
    .mockImplementationOnce(async () => {
      const result = (await b.promise) as { status: "ok"; data: typeof runtimeSnapshot };
      persisted = { model: result.data.model ?? "", effort: result.data.storedEffort ?? "" };
      return result;
    });
  const first = useStore.getState().setProjectRuntime("p1", "model-a", "medium");
  const second = useStore.getState().setProjectRuntime("p1", "model-b", "high");
  expect(update).toHaveBeenCalledTimes(1);
  expect(useStore.getState().projects[0].model).toBe("model-b");
  a.resolve({ status: "ok", data: { ...runtimeSnapshot, model: "model-a", storedEffort: "medium" } });
  await first;
  expect(update).toHaveBeenCalledTimes(2);
  expect(useStore.getState().projects[0].model).toBe("model-b");
  b.resolve({ status: "ok", data: { ...runtimeSnapshot, model: "model-b", storedEffort: "high" } });
  await second;
  expect(persisted).toEqual({ model: "model-b", effort: "high" });
  update.mockRestore();
});

test("two_failed_runtime_intents_restore_original_confirmed_baseline", async () => {
  reset();
  const project = projectSnapshot();
  useStore.setState({ projects: [project], projectRuntimeById: { p1: runtimeSnapshot } });
  const a = deferredResult();
  const b = deferredResult();
  const persisted = { model: "old", effort: "low" };
  const update = spyOn(commands, "updateProjectRuntime")
    .mockImplementationOnce(() => a.promise as never)
    .mockImplementationOnce(() => b.promise as never);
  const first = useStore.getState().setProjectRuntime("p1", "model-a", "medium");
  const second = useStore.getState().setProjectRuntime("p1", "model-b", "high");
  expect(update).toHaveBeenCalledTimes(1);
  a.resolve({ status: "error", error: { message: "A failed" } });
  await first;
  expect(update).toHaveBeenCalledTimes(2);
  b.resolve({ status: "error", error: { message: "B failed" } });
  await second;
  expect(useStore.getState().projects[0]).toEqual(project);
  expect(useStore.getState().projectRuntimeById.p1).toEqual(runtimeSnapshot);
  expect(persisted).toEqual({ model: "old", effort: "low" });
  update.mockRestore();
});

test("failed_latest_runtime_intent_rolls_back_to_confirmed_prior_success", async () => {
  reset();
  useStore.setState({ projects: [projectSnapshot()], projectRuntimeById: { p1: runtimeSnapshot } });
  const a = deferredResult();
  const b = deferredResult();
  const update = spyOn(commands, "updateProjectRuntime")
    .mockImplementationOnce(() => a.promise as never)
    .mockImplementationOnce(() => b.promise as never);
  const first = useStore.getState().setProjectRuntime("p1", "model-a", "medium");
  const second = useStore.getState().setProjectRuntime("p1", "model-b", "high");
  a.resolve({ status: "ok", data: { ...runtimeSnapshot, model: "model-a", storedEffort: "medium" } });
  await first;
  b.resolve({ status: "error", error: { message: "B failed" } });
  await second;
  expect(useStore.getState().projects[0].model).toBe("model-a");
  expect(useStore.getState().projects[0].effort).toBe("medium");
  expect(useStore.getState().projectRuntimeById.p1.model).toBe("model-a");
  update.mockRestore();
});

test("runtime_queues_for_different_projects_proceed_independently", async () => {
  reset();
  const p1 = projectSnapshot("p1");
  const p2 = projectSnapshot("p2");
  useStore.setState({ projects: [p1, p2], projectRuntimeById: { p1: runtimeSnapshot, p2: { ...runtimeSnapshot, projectId: "p2" } } });
  const firstDeferred = deferredResult();
  const secondDeferred = deferredResult();
  const update = spyOn(commands, "updateProjectRuntime")
    .mockImplementationOnce(() => firstDeferred.promise as never)
    .mockImplementationOnce(() => secondDeferred.promise as never);
  const first = useStore.getState().setProjectRuntime("p1", "one", "low");
  const second = useStore.getState().setProjectRuntime("p2", "two", "high");
  expect(update).toHaveBeenCalledTimes(2);
  firstDeferred.resolve({ status: "ok", data: { ...runtimeSnapshot, model: "one" } });
  secondDeferred.resolve({ status: "ok", data: { ...runtimeSnapshot, projectId: "p2", model: "two" } });
  await Promise.all([first, second]);
  update.mockRestore();
});

test("failed_runtime_save_rolls_back_both_snapshots", async () => {
  reset();
  const project = {
    projectId: "p1",
    name: "demo",
    workdir: "C:/demo",
    source: null,
    harness: "native",
    model: "openai/gpt-5",
    effort: "high",
    permMode: "default" as const,
    createdAt: null,
    isGit: true,
  };
  useStore.setState({ projects: [project], projectRuntimeById: { p1: runtimeSnapshot } });
  const update = spyOn(commands, "updateProjectRuntime").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const ok = await useStore.getState().setProjectRuntime("p1", "openai/gpt-5-mini", "low");
  expect(ok).toBe(false);
  expect(useStore.getState().projects[0]).toEqual(project);
  expect(useStore.getState().projectRuntimeById.p1).toEqual(runtimeSnapshot);
  update.mockRestore();
});

test("global_preference_and_metadata_refresh_refetch_loaded_project_runtime_status", async () => {
  reset();
  useStore.setState({ projectRuntimeById: { p1: runtimeSnapshot } });
  const reload = spyOn(useAgent.getState(), "load").mockResolvedValue(undefined);
  const fresh = { ...runtimeSnapshot, storedEffortStatus: "unsupported" as const, effectiveEffort: "medium" };
  const info = spyOn(commands, "projectRuntimeInfo").mockResolvedValue({ status: "ok", data: fresh });
  await useStore.getState().refreshModelConfiguration();
  expect(reload).toHaveBeenCalledTimes(1);
  expect(info).toHaveBeenCalledWith(LOCAL_RUNNER, "p1");
  expect(useStore.getState().projectRuntimeById.p1).toEqual(fresh);
  reload.mockRestore();
  info.mockRestore();
});

test("older_model_configuration_refresh_cannot_overwrite_newer_state", async () => {
  reset();
  useStore.setState({ projectRuntimeById: { p1: runtimeSnapshot } });
  let releaseOlder!: () => void;
  const olderGate = new Promise<void>((resolve) => {
    releaseOlder = resolve;
  });
  const reload = spyOn(useAgent.getState(), "load")
    .mockImplementationOnce(async () => {
      await olderGate;
      return undefined;
    })
    .mockResolvedValueOnce(undefined);
  const newer = { ...runtimeSnapshot, effectiveEffort: "medium" };
  const older = { ...runtimeSnapshot, effectiveEffort: "low" };
  const info = spyOn(commands, "projectRuntimeInfo")
    .mockResolvedValueOnce({ status: "ok", data: newer })
    .mockResolvedValueOnce({ status: "ok", data: older });
  const first = useStore.getState().refreshModelConfiguration();
  const second = useStore.getState().refreshModelConfiguration();
  await second;
  releaseOlder();
  await first;
  expect(useStore.getState().projectRuntimeById.p1.effectiveEffort).toBe("medium");
  reload.mockRestore();
  info.mockRestore();
});

test("configuration_refresh_started_before_mutation_cannot_overwrite_mutation", async () => {
  reset();
  const project = {
    projectId: "p1",
    name: "demo",
    workdir: "C:/demo",
    source: null,
    harness: "native",
    model: "old",
    effort: "low",
    permMode: "default" as const,
    createdAt: null,
    isGit: true,
  };
  useStore.setState({ projects: [project], projectRuntimeById: { p1: runtimeSnapshot } });
  const fetchList = spyOn(useAgent.getState(), "load").mockResolvedValue(undefined);
  let resolveRefresh!: (value: unknown) => void;
  const info = spyOn(commands, "projectRuntimeInfo").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveRefresh = resolve;
      }) as never,
  );
  const refresh = useStore.getState().refreshModelConfiguration();
  await Promise.resolve();
  const update = spyOn(commands, "updateProjectRuntime").mockResolvedValue({
    status: "ok",
    data: { ...runtimeSnapshot, model: "newest", storedEffort: "high" },
  });
  await useStore.getState().setProjectRuntime("p1", "newest", "high");
  resolveRefresh({ status: "ok", data: { ...runtimeSnapshot, model: "stale-refresh" } });
  await refresh;
  expect(useStore.getState().projectRuntimeById.p1.model).toBe("newest");
  fetchList.mockRestore();
  info.mockRestore();
  update.mockRestore();
});

test("load_resolving_during_or_after_mutation_cannot_overwrite_it", async () => {
  reset();
  const project = {
    projectId: "p1",
    name: "demo",
    workdir: "C:/demo",
    source: null,
    harness: "native",
    model: "old",
    effort: "low",
    permMode: "default" as const,
    createdAt: null,
    isGit: true,
  };
  useStore.setState({ projects: [project], projectRuntimeById: { p1: runtimeSnapshot } });
  let resolveLoad!: (value: unknown) => void;
  let resolveMutation!: (value: unknown) => void;
  const load = spyOn(commands, "projectRuntimeInfo").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveLoad = resolve;
      }) as never,
  );
  const update = spyOn(commands, "updateProjectRuntime").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveMutation = resolve;
      }) as never,
  );
  const loading = useStore.getState().loadProjectRuntime("p1");
  const mutation = useStore.getState().setProjectRuntime("p1", "newest", "high");
  resolveMutation({ status: "ok", data: { ...runtimeSnapshot, model: "newest", storedEffort: "high" } });
  await mutation;
  resolveLoad({ status: "ok", data: { ...runtimeSnapshot, model: "stale-load", storedEffort: "low" } });
  await loading;
  expect(useStore.getState().projectRuntimeById.p1.model).toBe("newest");
  load.mockRestore();
  update.mockRestore();
});

test("load_started_and_resolved_during_mutation_cannot_replace_optimistic_state", async () => {
  reset();
  const project = {
    projectId: "p1",
    name: "demo",
    workdir: "C:/demo",
    source: null,
    harness: "native",
    model: "old",
    effort: "low",
    permMode: "default" as const,
    createdAt: null,
    isGit: true,
  };
  useStore.setState({ projects: [project], projectRuntimeById: { p1: runtimeSnapshot } });
  let resolveLoad!: (value: unknown) => void;
  let resolveMutation!: (value: unknown) => void;
  const update = spyOn(commands, "updateProjectRuntime").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveMutation = resolve;
      }) as never,
  );
  const load = spyOn(commands, "projectRuntimeInfo").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveLoad = resolve;
      }) as never,
  );
  const mutation = useStore.getState().setProjectRuntime("p1", "newest", "high");
  const loading = useStore.getState().loadProjectRuntime("p1");
  resolveLoad({ status: "ok", data: { ...runtimeSnapshot, model: "stale-load" } });
  await loading;
  expect(useStore.getState().projectRuntimeById.p1.model).toBe("newest");
  resolveMutation({ status: "ok", data: { ...runtimeSnapshot, model: "newest", storedEffort: "high" } });
  await mutation;
  expect(useStore.getState().projectRuntimeById.p1.model).toBe("newest");
  load.mockRestore();
  update.mockRestore();
});

test("selectProject sets the selected project and clears the focused session", () => {
  reset();
  useStore.setState({ focusedSession: { runnerId: LOCAL_RUNNER, pk: "s1" } });
  useStore.getState().selectProject("p1");
  expect(useStore.getState().selectedProjectId).toBe("p1");
  expect(useStore.getState().focusedSession).toBeNull();
});

test("message events project to rows by role/blockType and dedupe by seq", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" }, LOCAL_RUNNER);
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "user",
      block_type: "text",
      payload: { text: "hi" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 2,
      role: "assistant",
      block_type: "thought",
      payload: { text: "pondering" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 3,
      role: "assistant",
      block_type: "text",
      payload: { text: "hello" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  // A duplicate/stale seq is ignored.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 2,
      role: "assistant",
      block_type: "text",
      payload: { text: "dup" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );

  const rows = useStore.getState().transcripts[k1];
  expect(rows.map((r) => [r.seq, r.role, r.blockType, r.text])).toEqual([
    [1, "user", "text", "hi"],
    [2, "assistant", "thought", "pondering"],
    [3, "assistant", "text", "hello"],
  ]);
});

test("durable system notice messages render live once and hydrate identically", async () => {
  reset();
  const event = {
    kind: "message" as const,
    session_pk: "s1",
    seq: 1,
    role: "system",
    block_type: "notice",
    payload: { text: "Account switched to Work Codex · round robin" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
    speaker: null,
  };

  useStore.getState().applyCoreEvent(event, LOCAL_RUNNER);
  useStore.getState().applyCoreEvent(event, LOCAL_RUNNER);

  const live = useStore.getState().transcripts[sessKey(LOCAL_RUNNER, "s1")];
  expect(live).toHaveLength(1);
  expect(live[0]).toMatchObject({
    seq: 1,
    role: "system",
    blockType: "notice",
    text: "Account switched to Work Codex · round robin",
  });
  const createdAt = live[0].createdAt;
  if (createdAt === null) throw new Error("live message events must receive a timestamp");

  reset();
  await useStore.getState().hydrateTranscript(LOCAL_RUNNER, "s1", async () => [
    {
      sessionPk: "s1",
      seq: 1,
      role: "system",
      blockType: "notice",
      payload: event.payload,
      toolCallId: null,
      status: null,
      toolKind: null,
      speaker: null,
      createdAt,
    },
  ]);
  expect(useStore.getState().transcripts[sessKey(LOCAL_RUNNER, "s1")]).toEqual(live);
});

test("tool_call events append once, then merge in place by toolCallId (same-seq update)", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "Bash", input: { command: "ls" } },
      tool_call_id: "tc-1",
      status: "pending",
      tool_kind: "execute",
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  // Completion re-emit re-uses seq 1 — must merge, not append, not be dropped.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "Bash", input: { command: "ls" }, output: "file.txt" },
      tool_call_id: "tc-1",
      status: "completed",
      tool_kind: "execute",
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  // lastSeq high-water mark is untouched by the merge: a later fresh row still lands.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 2,
      role: "assistant",
      block_type: "text",
      payload: { text: "done" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );

  const rows = useStore.getState().transcripts[k1];
  expect(rows).toHaveLength(2);
  expect(rows[0].toolCallId).toBe("tc-1");
  expect(rows[0].toolStatus).toBe("completed");
  expect(rows[0].toolName).toBe("Bash");
  expect(rows[0].toolOutput).toBe("file.txt");
  expect(rows[1].text).toBe("done");
});

test("hydrateTranscript replaces the transcript from persisted messages and sets lastSeq", async () => {
  reset();
  const rows = [
    {
      sessionPk: "s1",
      seq: 1,
      role: "user",
      blockType: "text",
      payload: { text: "hi" },
      toolCallId: null,
      status: null,
      toolKind: null,
      speaker: null,
      createdAt: 1,
    },
    {
      sessionPk: "s1",
      seq: 2,
      role: "assistant",
      blockType: "tool_call",
      payload: { name: "Read", input: {}, output: { ok: true } },
      toolCallId: "tc-9",
      status: "completed",
      toolKind: "read",
      speaker: null,
      createdAt: 2,
    },
  ];
  await useStore.getState().hydrateTranscript(LOCAL_RUNNER, "s1", async () => rows);
  const st = useStore.getState();
  expect(st.transcripts[k1][0].text).toBe("hi");
  expect(st.transcripts[k1][1].toolName).toBe("Read");
  expect(st.transcripts[k1][1].toolOutput).toBe(JSON.stringify({ ok: true }, null, 2));
  expect(st.lastSeq[k1]).toBe(2);
  expect(st.loaded[k1]).toBe(true);

  // A live non-tool event with seq <= lastSeq is ignored; a newer one appends.
  st.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 2,
      role: "assistant",
      block_type: "text",
      payload: { text: "again" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  st.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 3,
      role: "assistant",
      block_type: "text",
      payload: { text: "next" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  expect(useStore.getState().transcripts[k1].map((r) => r.seq)).toEqual([1, 2, 3]);
});

test("hydrateTranscript keeps live rows that arrived during the fetch (and never regresses lastSeq)", async () => {
  reset();
  const dbRows = [
    {
      sessionPk: "s1",
      seq: 1,
      role: "user",
      blockType: "text",
      payload: { text: "hi" },
      toolCallId: null,
      status: null,
      toolKind: null,
      speaker: null,
      createdAt: 1,
    },
    {
      sessionPk: "s1",
      seq: 2,
      role: "assistant",
      blockType: "text",
      payload: { text: "yo" },
      toolCallId: null,
      status: null,
      toolKind: null,
      speaker: null,
      createdAt: 2,
    },
  ];
  await useStore.getState().hydrateTranscript(LOCAL_RUNNER, "s1", async () => {
    // Simulates an event landing while listMessages is in flight.
    useStore.getState().applyCoreEvent(
      {
        kind: "message",
        session_pk: "s1",
        seq: 3,
        role: "assistant",
        block_type: "text",
        payload: { text: "live" },
        tool_call_id: null,
        status: null,
        tool_kind: null,
        speaker: null,
      },
      LOCAL_RUNNER,
    );
    return dbRows;
  });
  const st = useStore.getState();
  expect(st.transcripts[k1].map((r) => r.seq)).toEqual([1, 2, 3]);
  expect(st.transcripts[k1][2].text).toBe("live");
  expect(st.lastSeq[k1]).toBe(3);
});

test("approval.requested adds a pending approval; resolving removes it", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent(
    {
      kind: "approvalRequested",
      session_pk: "s1",
      request_id: "r1",
      tool: "Bash",
      summary: "Bash: rm",
      approval_kind: "tool",
      input: {},
    },
    LOCAL_RUNNER,
  );
  expect(useStore.getState().pendingApprovals).toHaveLength(1);
  useStore.getState().clearApproval("r1");
  expect(useStore.getState().pendingApprovals).toHaveLength(0);
});

test("pending approvals from different sessions both count", () => {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSession: null });
  const s = useStore.getState();
  s.applyCoreEvent(
    {
      kind: "approvalRequested",
      session_pk: "s1",
      request_id: "r1",
      tool: "Bash",
      summary: "x",
      approval_kind: "tool",
      input: {},
    },
    LOCAL_RUNNER,
  );
  s.applyCoreEvent(
    {
      kind: "approvalRequested",
      session_pk: "s2",
      request_id: "r2",
      tool: "Write",
      summary: "y",
      approval_kind: "tool",
      input: {},
    },
    LOCAL_RUNNER,
  );
  expect(useStore.getState().pendingApprovals).toHaveLength(2);
});

const runningSession = (pk: string) => ({
  runnerId: LOCAL_RUNNER,
  sessionPk: pk,
  projectId: "p1",
  agentSessionId: null,
  worktreePath: null,
  branch: null,
  title: null,
  status: "running" as const,
  permMode: "default" as const,
  createdAt: null,
  lastActive: null,
  startedBy: null,
  resumeAttempts: 0,
  branchOwned: true,
  kind: "project" as const,
  speaker: null,
  agent: null,
  parentSessionPk: null,
});

test("result event flips the session status back to idle (so the composer leaves Stop mode)", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  // result also fires a fire-and-forget refresh(); stub its IPC calls (never resolving,
  // like the "start" tests do) so nothing hits the real Tauri binding after this test ends.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));
  useStore.getState().applyCoreEvent({ kind: "result", session_pk: "s1" }, LOCAL_RUNNER);
  expect(useStore.getState().sessions[0].status).toBe("idle");
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("result event triggers a refresh so the git/harness backfill (branch, worktreePath) lands in the UI", async () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  const backfilled = { ...runningSession("s1"), status: "idle" as const, branch: "harness/s1", worktreePath: "C:\\wt\\s1" };
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [backfilled] });
  const listGateways = mockGateways();

  useStore.getState().applyCoreEvent({ kind: "result", session_pk: "s1" }, LOCAL_RUNNER);
  // refresh() is fire-and-forget; a setTimeout(0) tick drains the whole microtask
  // queue (unlike a fixed count of Promise.resolve() flushes, which can under-count
  // the listProjects → listGateways → Promise.all(listSessions) → set() chain and
  // leave refresh() dangling into the next test).
  await new Promise((resolve) => setTimeout(resolve, 0));

  expect(listProjects).toHaveBeenCalled();
  expect(listSessions).toHaveBeenCalled();
  expect(useStore.getState().sessions[0].branch).toBe("harness/s1");
  expect(useStore.getState().sessions[0].worktreePath).toBe("C:\\wt\\s1");

  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("sessionEnded event marks the session ended", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  useStore.getState().applyCoreEvent({ kind: "sessionEnded", session_pk: "s1" }, LOCAL_RUNNER);
  expect(useStore.getState().sessions[0].status).toBe("ended");
});

test("result event leaves other sessions' status untouched", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1"), runningSession("s2")] });
  // result also fires a fire-and-forget refresh(); stub its IPC calls (never resolving,
  // like the "start" tests do) so nothing hits the real Tauri binding after this test ends.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));
  useStore.getState().applyCoreEvent({ kind: "result", session_pk: "s1" }, LOCAL_RUNNER);
  const byPk = Object.fromEntries(useStore.getState().sessions.map((s) => [s.sessionPk, s.status]));
  expect(byPk).toEqual({ s1: "idle", s2: "running" });
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("error event flips the failed session back to idle and leaves others untouched", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1"), runningSession("s2")] });
  // error also fires a fire-and-forget refresh(); stub its IPC calls (never
  // resolving, like the "result" tests do) so nothing hits the real Tauri
  // binding after this test ends.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));
  useStore.getState().applyCoreEvent({ kind: "error", session_pk: "s1", message: "upstream quota exhausted" }, LOCAL_RUNNER);
  const byPk = Object.fromEntries(useStore.getState().sessions.map((s) => [s.sessionPk, s.status]));
  expect(byPk).toEqual({ s1: "idle", s2: "running" });
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("error event triggers a refresh so the DB-side demotion lands in the UI", async () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  const demoted = { ...runningSession("s1"), status: "idle" as const };
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [demoted] });
  const listGateways = mockGateways();

  useStore.getState().applyCoreEvent({ kind: "error", session_pk: "s1", message: "boom" }, LOCAL_RUNNER);
  // refresh() is fire-and-forget; a setTimeout(0) tick drains the whole microtask queue
  // (see the "result event triggers a refresh" test above for why this beats a fixed
  // count of Promise.resolve() flushes).
  await new Promise((resolve) => setTimeout(resolve, 0));

  expect(listProjects).toHaveBeenCalled();
  expect(listSessions).toHaveBeenCalled();
  expect(useStore.getState().sessions[0].status).toBe("idle");

  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("error event appends no transient row — the durable error row arrives via the message event", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));

  useStore.getState().applyCoreEvent({ kind: "error", session_pk: "s1", message: "upstream quota exhausted" }, LOCAL_RUNNER);
  expect(useStore.getState().transcripts[k1] ?? []).toHaveLength(0);

  // The backend persists the same text via emit_error and broadcasts it as a
  // normal message row (role=system, block_type=error) — THAT renders it.
  useStore.getState().applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 7,
      role: "system",
      block_type: "error",
      payload: { message: "upstream quota exhausted" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  const rows = useStore.getState().transcripts[k1];
  expect(rows).toHaveLength(1);
  expect(rows[0].blockType).toBe("error");
  expect(rows[0].text).toBe("upstream quota exhausted");
  expect(rows[0].seq).toBe(7);

  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("start forwards chat options so composer model, context, and attachments reach IPC", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "s1",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: "harness/s1",
      title: "/review",
      status: "running",
      permMode: "default",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: true,
      kind: "project",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  });
  // refresh() must not gate start(), and this test only checks the IPC call shape and
  // the optimistic focus/seed — never-resolving avoids racing mockRestore() below
  // against a fire-and-forget refresh() still in flight (see "start resolves and
  // focuses the session without waiting for refresh").
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));

  await useStore.getState().start(LOCAL_RUNNER, "p1", "/review", {
    model: "fable",
    context: { branch: "feature/auth", voiceTranscript: null, references: ["src/main.rs"] },
    attachments: ["C:\\tmp\\notes.txt"],
  });

  expect(start).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "/review", {
    model: "fable",
    effort: null,
    context: { branch: "feature/auth", voiceTranscript: null, references: ["src/main.rs"] },
    attachments: ["C:\\tmp\\notes.txt"],
    git: null,
    permMode: null,
  });
  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "s1" });

  start.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("start forwards composer git options to IPC", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "s2",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: "feat/login",
      title: "go",
      status: "running",
      permMode: "default",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: false,
      kind: "project",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  });
  // See "start forwards chat options" above: never-resolving avoids racing
  // mockRestore() against a fire-and-forget refresh() still in flight.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));

  await useStore.getState().start(LOCAL_RUNNER, "p1", "go", {
    git: { useWorktree: false, createBranch: true, branchName: "feat/login", baseBranch: null },
  });

  expect(start).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "go", {
    model: null,
    effort: null,
    context: null,
    attachments: [],
    git: { useWorktree: false, createBranch: true, branchName: "feat/login", baseBranch: null },
    permMode: null,
  });

  start.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("start resolves and focuses the session without waiting for refresh", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "s3",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: null,
      title: "go",
      status: "running",
      permMode: "default",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: true,
      kind: "project",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  });
  // refresh() must not gate start(): these never resolve during the test.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));

  const ok = await useStore.getState().start(LOCAL_RUNNER, "p1", "go", null);

  expect(ok).toBe(true);
  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "s3" });
  // The returned row is seeded so the session view renders immediately.
  expect(useStore.getState().sessions.map((s) => s.sessionPk)).toContain("s3");

  start.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("start returns false and does not focus on backend error", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  const ok = await useStore.getState().start(LOCAL_RUNNER, "p1", "go", null);
  expect(ok).toBe(false);
  expect(useStore.getState().focusedSession).toBeNull();
  start.mockRestore();
});

test("startChat calls start_chat_session (no projectId) and seeds/focuses the returned session", async () => {
  reset();
  const startChat = spyOn(commands, "startChatSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "c1",
      projectId: null,
      agentSessionId: null,
      worktreePath: null,
      branch: null,
      title: "hey",
      status: "running",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: false,
      permMode: "default",
      kind: "chat",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  });
  // refresh() must not gate startChat(): these never resolve during the test.
  const listProjects = spyOn(commands, "listProjects").mockReturnValue(new Promise(() => {}));
  const listSessions = spyOn(commands, "listSessions").mockReturnValue(new Promise(() => {}));

  const ok = await useStore.getState().startChat(LOCAL_RUNNER, "hey", { model: "fable", effort: "high" });

  expect(startChat).toHaveBeenCalledWith(LOCAL_RUNNER, "hey", {
    model: "fable",
    effort: "high",
    permMode: null,
    context: null,
    attachments: [],
    git: null,
  });
  expect(ok).toBe(true);
  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "c1" });
  expect(useStore.getState().sessions.map((s) => s.sessionPk)).toContain("c1");

  startChat.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("projectless session runtime loads and updates independently", async () => {
  reset();
  const initial = {
    sessionPk: "c1",
    model: "fixture/model-alpha",
    storedEffort: "medium",
    effectiveEffort: "medium",
    effectiveEffortLabel: "Medium",
    effectiveSource: "session" as const,
    storedEffortStatus: "valid" as const,
    modelInfo: null,
  };
  const runtimeCommands = commands as typeof commands & {
    sessionRuntimeInfo: typeof commands.projectRuntimeInfo;
    updateSessionRuntime: typeof commands.updateProjectRuntime;
  };
  const sessionInfo = mock(async () => ({ status: "ok" as const, data: initial }));
  const update = mock(async () => ({
    status: "ok" as const,
    data: { ...initial, model: "fixture/model-beta", storedEffort: "ultra" },
  }));
  Object.assign(runtimeCommands, { sessionRuntimeInfo: sessionInfo, updateSessionRuntime: update });

  await useStore.getState().loadSessionRuntime(LOCAL_RUNNER, "c1");
  expect(useStore.getState().sessionRuntimeById.c1).toEqual(initial);
  await useStore.getState().setSessionRuntime(LOCAL_RUNNER, "c1", "fixture/model-beta", "ultra");
  expect(update).toHaveBeenCalledWith(LOCAL_RUNNER, "c1", "fixture/model-beta", "ultra");
  expect(useStore.getState().sessionRuntimeById.c1).toMatchObject({ model: "fixture/model-beta", storedEffort: "ultra" });

  delete (runtimeCommands as Partial<typeof runtimeCommands>).sessionRuntimeInfo;
  delete (runtimeCommands as Partial<typeof runtimeCommands>).updateSessionRuntime;
});

test("startChat returns false and does not focus on backend error", async () => {
  reset();
  const startChat = spyOn(commands, "startChatSession").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  const ok = await useStore.getState().startChat(LOCAL_RUNNER, "hey", null);
  expect(ok).toBe(false);
  expect(useStore.getState().focusedSession).toBeNull();
  startChat.mockRestore();
});

test("cloneProject clones via IPC and refreshes on success", async () => {
  reset();
  const clone = spyOn(commands, "cloneProject").mockResolvedValue({
    status: "ok",
    data: {
      projectId: "p9",
      name: "repo",
      workdir: "C:\\proj\\repo",
      source: "https://github.com/user/repo.git",
      model: null,
      effort: null,
      permMode: "default",
      createdAt: 1,
      isGit: true,
    },
  });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = mockGateways();

  // cloneProject is a project-management action — always runs on the local engine.
  const ok = await useStore.getState().cloneProject("https://github.com/user/repo.git", "C:\\proj");

  expect(ok).toBe(true);
  expect(clone).toHaveBeenCalledWith(LOCAL_RUNNER, "https://github.com/user/repo.git", "C:\\proj");
  expect(listProjects).toHaveBeenCalled();

  clone.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("a completed todowrite tool_call triggers a todo refetch for its session", () => {
  reset();
  const original = useNative.getState().loadTodos;
  const loadTodos = mock((_runnerId: string, _pk: string) => Promise.resolve());
  useNative.setState({ loadTodos });
  const s = useStore.getState();
  // Initial in_progress insert: the tool hasn't executed yet — no fetch.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 4,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "todowrite", input: { todos: [{ content: "a", status: "pending" }] } },
      tool_call_id: "tc-todo",
      status: "in_progress",
      tool_kind: "other",
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  expect(loadTodos).not.toHaveBeenCalled();
  // Completion re-emit (same seq, merged by toolCallId): the DB changed — refetch.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 4,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "todowrite", input: { todos: [{ content: "a", status: "pending" }] }, output: "Updated todo list (0/1 done)" },
      tool_call_id: "tc-todo",
      status: "completed",
      tool_kind: "other",
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  expect(loadTodos).toHaveBeenCalledTimes(1);
  expect(loadTodos).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  // Other completed tools never trigger a todo fetch.
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 5,
      role: "assistant",
      block_type: "tool_call",
      payload: { name: "bash", input: { command: "ls" }, output: "ok" },
      tool_call_id: "tc-bash",
      status: "completed",
      tool_kind: "execute",
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  expect(loadTodos).toHaveBeenCalledTimes(1);
  useNative.setState({ loadTodos: original });
});

test("send resolves true on success and false on backend error (drives composer draft restore)", async () => {
  reset();
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = mockGateways();

  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", "hi", null)).resolves.toBe(true);

  cont.mockResolvedValue({ status: "error", error: { message: "quota exhausted" } });
  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", "hi", null)).resolves.toBe(false);

  cont.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("send steers a RUNNING session instead of starting a new turn via continue", async () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  const steer = spyOn(commands, "steerSession").mockResolvedValue({ status: "ok", data: true });
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = mockGateways();

  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", "hold on", null)).resolves.toBe(true);
  expect(steer).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "hold on");
  expect(cont).not.toHaveBeenCalled();

  steer.mockRestore();
  cont.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("send falls back to continue for a session that is not running", async () => {
  reset();
  useStore.setState({ sessions: [{ ...runningSession("s1"), status: "idle" as const }] });
  const steer = spyOn(commands, "steerSession").mockResolvedValue({ status: "ok", data: true });
  const cont = spyOn(commands, "continueSession").mockResolvedValue({ status: "ok", data: null });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });
  const listGateways = mockGateways();

  await expect(useStore.getState().send(LOCAL_RUNNER, "s1", "go", null)).resolves.toBe(true);
  expect(cont).toHaveBeenCalled();
  expect(steer).not.toHaveBeenCalled();

  steer.mockRestore();
  cont.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
  listGateways.mockRestore();
});

test("setFocused marks the previously-focused session read up to its lastActive", () => {
  useUi.setState({ readAt: {} });
  useStore.setState({
    focusedSession: { runnerId: LOCAL_RUNNER, pk: "s1" },
    sessions: [
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s1",
        projectId: "p",
        agentSessionId: null,
        worktreePath: null,
        branch: null,
        title: "s1",
        status: "idle",
        startedBy: null,
        createdAt: 0,
        lastActive: 4200,
        resumeAttempts: 0,
        branchOwned: false,
        permMode: "default",
        kind: "project",
        speaker: null,
        agent: null,
        parentSessionPk: null,
      },
    ],
    loaded: { [k1]: true, [k2]: true },
  });
  useStore.getState().setFocused({ runnerId: LOCAL_RUNNER, pk: "s2" });
  expect(useUi.getState().readAt[k1]).toBe(4200);
  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "s2" });
});

// markFocusedSessionReadOnEvent is the extracted decision the init() coreEventMsg
// listener runs on every live event; it's exercised directly here since driving the
// real Tauri event subscription isn't practical in this harness.
test("markFocusedSessionReadOnEvent marks the focused session read as live activity streams in", () => {
  useUi.setState({ readAt: {} });
  const before = Date.now();
  markFocusedSessionReadOnEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "assistant",
      block_type: "text",
      payload: { text: "hi" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
    { runnerId: LOCAL_RUNNER, pk: "s1" },
  );
  expect(useUi.getState().readAt[k1]).toBeGreaterThanOrEqual(before);
});

test("markFocusedSessionReadOnEvent leaves read state untouched for events on a non-focused session", () => {
  useUi.setState({ readAt: {} });
  markFocusedSessionReadOnEvent(
    {
      kind: "message",
      session_pk: "s2",
      seq: 1,
      role: "assistant",
      block_type: "text",
      payload: { text: "hi" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
    { runnerId: LOCAL_RUNNER, pk: "s1" },
  );
  expect(useUi.getState().readAt[k2]).toBeUndefined();
});

test("two runners with the same session_pk keep separate transcripts and separate focus (collision safety)", () => {
  reset();
  const s = useStore.getState();
  const remote = "remote-1";
  const localKey = sessKey(LOCAL_RUNNER, "s1");
  const remoteKey = sessKey(remote, "s1");

  // sessionCreated seeds `loaded` for the composite key, so setFocused below never
  // trips a hydrateTranscript IPC call.
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" }, LOCAL_RUNNER);
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" }, remote);

  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "user",
      block_type: "text",
      payload: { text: "local hi" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    LOCAL_RUNNER,
  );
  s.applyCoreEvent(
    {
      kind: "message",
      session_pk: "s1",
      seq: 1,
      role: "user",
      block_type: "text",
      payload: { text: "remote hi" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
    remote,
  );

  // Same session_pk on two different runners must never share a transcript bucket.
  expect(useStore.getState().transcripts[localKey].map((r) => r.text)).toEqual(["local hi"]);
  expect(useStore.getState().transcripts[remoteKey].map((r) => r.text)).toEqual(["remote hi"]);

  // Focus is runner-qualified too: focusing the remote session must not read back as
  // "the same session" as the local one, even though their `pk` is identical.
  useStore.getState().setFocused({ runnerId: LOCAL_RUNNER, pk: "s1" });
  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "s1" });
  useStore.getState().setFocused({ runnerId: remote, pk: "s1" });
  expect(useStore.getState().focusedSession).toEqual({ runnerId: remote, pk: "s1" });
});

const qmsg = (id: string, text = id): QueuedMessage => ({ id, text, options: null });

test("enqueueMessage appends per session; removeQueued removes by id", () => {
  useStore.setState({ queued: {} });
  useStore.getState().enqueueMessage(LOCAL_RUNNER, "s1", qmsg("a"));
  useStore.getState().enqueueMessage(LOCAL_RUNNER, "s1", qmsg("b"));
  expect(useStore.getState().queued[k1].map((m) => m.id)).toEqual(["a", "b"]);
  useStore.getState().removeQueued(LOCAL_RUNNER, "s1", "a");
  expect(useStore.getState().queued[k1].map((m) => m.id)).toEqual(["b"]);
});

test("sendNextQueued sends the head and removes it on success", async () => {
  const calls: Array<[string, string, string]> = [];
  useStore.setState({
    queued: { [k1]: [qmsg("a", "hello"), qmsg("b", "world")] },
    send: async (runnerId, pk, text) => {
      calls.push([runnerId, pk, text]);
      return true;
    },
  });
  await useStore.getState().sendNextQueued(LOCAL_RUNNER, "s1");
  expect(calls).toEqual([[LOCAL_RUNNER, "s1", "hello"]]);
  expect(useStore.getState().queued[k1].map((m) => m.id)).toEqual(["b"]);
});

test("sendNextQueued unshifts the head back when send fails", async () => {
  useStore.setState({
    queued: { [k1]: [qmsg("a", "hello")] },
    send: async () => false,
  });
  await useStore.getState().sendNextQueued(LOCAL_RUNNER, "s1");
  expect(useStore.getState().queued[k1].map((m) => m.id)).toEqual(["a"]); // still queued
});

test("sendNextQueued on an empty queue does not call send", async () => {
  let called = false;
  useStore.setState({
    queued: {},
    send: async () => {
      called = true;
      return true;
    },
  });
  await useStore.getState().sendNextQueued(LOCAL_RUNNER, "s1");
  expect(called).toBe(false);
});

test("drainQueueOnEvent drains on result but not on error", () => {
  const drained: string[] = [];
  useStore.setState({ sendNextQueued: async (_runnerId, pk) => void drained.push(pk) });
  drainQueueOnEvent({ kind: "error", session_pk: "s1" } as never, LOCAL_RUNNER);
  expect(drained).toEqual([]);
  drainQueueOnEvent({ kind: "result", session_pk: "s1" } as never, LOCAL_RUNNER);
  expect(drained).toEqual(["s1"]);
});
