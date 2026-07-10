import { test, expect, spyOn } from "bun:test";
import { useNative } from "./store-native";
import { commands } from "./bindings";

function reset() {
  useNative.setState({ agentsByProject: {}, commandsByProject: {}, todosBySession: {} });
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
  await useNative.getState().loadAgents("p1");
  expect(spy).toHaveBeenCalledWith("p1");
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
  await useNative.getState().loadTodos("s1");
  expect(spy).toHaveBeenCalledWith("s1");
  const todos = useNative.getState().todosBySession.s1;
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
  await useNative.getState().loadCommands("p1");
  expect(useNative.getState().commandsByProject.p1).toBeUndefined();
  spy.mockRestore();
});

test("exportSession returns the JSON payload", async () => {
  reset();
  const spy = spyOn(commands, "exportSession").mockResolvedValue({ status: "ok", data: '{"version":1}' });
  const out = await useNative.getState().exportSession("s1");
  expect(spy).toHaveBeenCalledWith("s1");
  expect(out).toBe('{"version":1}');
  spy.mockRestore();
});

test("shareSession returns the rendered HTML", async () => {
  reset();
  const spy = spyOn(commands, "shareSession").mockResolvedValue({
    status: "ok",
    data: "<!doctype html><title>x</title>",
  });
  const out = await useNative.getState().shareSession("s1");
  expect(spy).toHaveBeenCalledWith("s1");
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
  const ok = await useNative.getState().importSession("p1", '{"version":1}');
  expect(spy).toHaveBeenCalledWith("p1", '{"version":1}');
  expect(ok).toBe(true);
  spy.mockRestore();
});

test("loadTodos drops out-of-order responses (a stale fetch can't clobber newer data)", async () => {
  reset();
  type TodosResult = Awaited<ReturnType<typeof commands.sessionTodos>>;
  const resolvers: Array<(v: TodosResult) => void> = [];
  const spy = spyOn(commands, "sessionTodos").mockImplementation(() => new Promise<TodosResult>((resolve) => resolvers.push(resolve)));
  const first = useNative.getState().loadTodos("s1"); // older fetch…
  const second = useNative.getState().loadTodos("s1"); // …superseded by this one
  // The newer fetch resolves first with the fresh list.
  resolvers[1]({ status: "ok", data: [{ content: "execute", status: "in_progress" }] });
  await second;
  // The older fetch resolves late with the stale list — it must be ignored.
  resolvers[0]({ status: "ok", data: [{ content: "plan", status: "completed" }] });
  await first;
  expect(useNative.getState().todosBySession.s1).toEqual([{ content: "execute", status: "in_progress" }]);
  spy.mockRestore();
});

test("loadTodos tokens are per-session — one session's fetch never invalidates another's", async () => {
  reset();
  type TodosResult = Awaited<ReturnType<typeof commands.sessionTodos>>;
  const resolvers: Array<(v: TodosResult) => void> = [];
  const spy = spyOn(commands, "sessionTodos").mockImplementation(() => new Promise<TodosResult>((resolve) => resolvers.push(resolve)));
  const a = useNative.getState().loadTodos("s1");
  const b = useNative.getState().loadTodos("s2"); // different session, issued later
  resolvers[1]({ status: "ok", data: [{ content: "other", status: "pending" }] });
  await b;
  resolvers[0]({ status: "ok", data: [{ content: "mine", status: "pending" }] });
  await a;
  expect(useNative.getState().todosBySession.s1).toEqual([{ content: "mine", status: "pending" }]);
  expect(useNative.getState().todosBySession.s2).toEqual([{ content: "other", status: "pending" }]);
  spy.mockRestore();
});
