import { test, expect, spyOn } from "bun:test";
import { useNative } from "./store-native";
import { commands } from "./bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const s1 = sessKey(LOCAL_RUNNER, "s1");
const s2 = sessKey(LOCAL_RUNNER, "s2");

function reset() {
  useNative.setState({ agentsByProject: {}, commandsByProject: {}, todosBySession: {}, queuedBySession: {} });
}

test("loadAgents caches the project's agents", async () => {
  reset();
  const spy = spyOn(commands, "nativeAgents").mockResolvedValue({
    status: "ok",
    data: [
      { name: "build", description: "Full access", mode: "primary", builtin: true },
      { name: "explore", description: "Read-only", mode: "subagent", builtin: true },
    ],
  });
  await useNative.getState().loadAgents(LOCAL_RUNNER, "p1");
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "p1");
  expect(useNative.getState().agentsByProject.p1.map((a) => a.name)).toEqual(["build", "explore"]);
  spy.mockRestore();
});

test("loadTodos caches a session's todo list", async () => {
  reset();
  const spy = spyOn(commands, "sessionTodos").mockResolvedValue({
    status: "ok",
    data: [
      { content: "step one", status: "completed" },
      { content: "step two", status: "in_progress" },
    ],
  });
  await useNative.getState().loadTodos(LOCAL_RUNNER, "s1");
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  const todos = useNative.getState().todosBySession[s1];
  expect(todos).toHaveLength(2);
  expect(todos[1]).toEqual({ content: "step two", status: "in_progress" });
  spy.mockRestore();
});

test("a failed command leaves the cache untouched", async () => {
  reset();
  const spy = spyOn(commands, "nativeCommands").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await useNative.getState().loadCommands(LOCAL_RUNNER, "p1");
  expect(useNative.getState().commandsByProject.p1).toBeUndefined();
  spy.mockRestore();
});

test("exportSession returns the JSON payload", async () => {
  reset();
  const spy = spyOn(commands, "exportSession").mockResolvedValue({ status: "ok", data: '{"version":1}' });
  const out = await useNative.getState().exportSession(LOCAL_RUNNER, "s1");
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  expect(out).toBe('{"version":1}');
  spy.mockRestore();
});

test("shareSession returns the rendered HTML", async () => {
  reset();
  const spy = spyOn(commands, "shareSession").mockResolvedValue({
    status: "ok",
    data: "<!doctype html><title>x</title>",
  });
  const out = await useNative.getState().shareSession(LOCAL_RUNNER, "s1");
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  expect(out).toContain("<!doctype html>");
  spy.mockRestore();
});

test("importSession reports success", async () => {
  reset();
  const spy = spyOn(commands, "importSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "new",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: null,
      title: "Imported",
      status: "ended",
      permMode: "default",
      startedBy: "import",
      createdAt: 0,
      lastActive: 0,
      resumeAttempts: 0,
      branchOwned: true,
      kind: "project",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  });
  const ok = await useNative.getState().importSession(LOCAL_RUNNER, "p1", '{"version":1}');
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", '{"version":1}');
  expect(ok).toBe(true);
  spy.mockRestore();
});

test("loadTodos drops out-of-order responses (a stale fetch can't clobber newer data)", async () => {
  reset();
  type TodosResult = Awaited<ReturnType<typeof commands.sessionTodos>>;
  const resolvers: Array<(v: TodosResult) => void> = [];
  const spy = spyOn(commands, "sessionTodos").mockImplementation(() => new Promise<TodosResult>((resolve) => resolvers.push(resolve)));
  const first = useNative.getState().loadTodos(LOCAL_RUNNER, "s1"); // older fetch…
  const second = useNative.getState().loadTodos(LOCAL_RUNNER, "s1"); // …superseded by this one
  // The newer fetch resolves first with the fresh list.
  resolvers[1]({ status: "ok", data: [{ content: "execute", status: "in_progress" }] });
  await second;
  // The older fetch resolves late with the stale list — it must be ignored.
  resolvers[0]({ status: "ok", data: [{ content: "plan", status: "completed" }] });
  await first;
  expect(useNative.getState().todosBySession[s1]).toEqual([{ content: "execute", status: "in_progress" }]);
  spy.mockRestore();
});

test("loadQueue keeps same session pks separate across runners", async () => {
  reset();
  const remote = "remote-1";
  const localKey = sessKey(LOCAL_RUNNER, "s1");
  const remoteKey = sessKey(remote, "s1");
  const spy = spyOn(commands, "sessionQueue").mockImplementation(async (runnerId) => ({
    status: "ok",
    data: [{ id: runnerId === LOCAL_RUNNER ? "local" : "remote", text: "queued" }],
  }));

  await useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  await useNative.getState().loadQueue(remote, "s1");

  expect(useNative.getState().queuedBySession[localKey]).toEqual([{ id: "local", text: "queued" }]);
  expect(useNative.getState().queuedBySession[remoteKey]).toEqual([{ id: "remote", text: "queued" }]);
  spy.mockRestore();
});

test("enqueueQueueMessage appends the server message and removeQueueMessage filters it", async () => {
  reset();
  const spyEnqueue = spyOn(commands, "enqueueSessionMessage").mockResolvedValue({ status: "ok", data: { id: "a", text: "hello" } });
  const spyRemove = spyOn(commands, "removeSessionMessage").mockResolvedValue({ status: "ok", data: true });

  expect(await useNative.getState().enqueueQueueMessage(LOCAL_RUNNER, "s1", "hello", null)).toBe(true);
  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "a", text: "hello" }]);
  expect(await useNative.getState().removeQueueMessage(LOCAL_RUNNER, "s1", "a")).toBe(true);
  expect(useNative.getState().queuedBySession[s1]).toEqual([]);
  expect(spyEnqueue).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "hello", null);
  expect(spyRemove).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "a");
  spyEnqueue.mockRestore();
  spyRemove.mockRestore();
});

test("failed queue mutations leave the cached queue unchanged", async () => {
  reset();
  useNative.setState({ queuedBySession: { [s1]: [{ id: "a", text: "kept" }] } });
  const enqueue = spyOn(commands, "enqueueSessionMessage").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const remove = spyOn(commands, "removeSessionMessage").mockResolvedValue({ status: "error", error: { message: "boom" } });

  expect(await useNative.getState().enqueueQueueMessage(LOCAL_RUNNER, "s1", "new", null)).toBe(false);
  expect(await useNative.getState().removeQueueMessage(LOCAL_RUNNER, "s1", "a")).toBe(false);
  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "a", text: "kept" }]);
  enqueue.mockRestore();
  remove.mockRestore();
});

test("loadQueue drops an out-of-order stale response", async () => {
  reset();
  type QueueResult = Awaited<ReturnType<typeof commands.sessionQueue>>;
  const resolvers: Array<(value: QueueResult) => void> = [];
  const spy = spyOn(commands, "sessionQueue").mockImplementation(() => new Promise<QueueResult>((resolve) => resolvers.push(resolve)));

  const first = useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  const second = useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  resolvers[1]({ status: "ok", data: [{ id: "new", text: "newest" }] });
  await second;
  resolvers[0]({ status: "ok", data: [{ id: "old", text: "stale" }] });
  await first;

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "new", text: "newest" }]);
  spy.mockRestore();
});
