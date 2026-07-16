import { afterEach, expect, spyOn, test } from "bun:test";
import type { CommandInfo, ProjectCommandInfo } from "./bindings";
import { useNative } from "./store-native";
import { commands } from "./bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const s1 = sessKey(LOCAL_RUNNER, "s1");

function reset() {
  useNative.setState({ agentsByProject: {}, commandsByProject: {}, projectCommandsByProject: {}, todosBySession: {}, queuedBySession: {} });
}

afterEach(reset);

const projectCommand: ProjectCommandInfo = {
  name: "review",
  description: "Review the change",
  template: "Review $ARGUMENTS",
  agent: null,
  model: null,
  subtask: false,
  revision: "rev-1",
};

const effectiveGlobalCommand: CommandInfo = {
  name: "review",
  description: "Global review",
  agent: null,
  model: null,
  subtask: false,
  origin: "global",
  effective: true,
  shadowsGlobal: false,
};

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

test("loadAgents drops out-of-order responses (a stale fetch can't clobber newer data)", async () => {
  reset();
  type AgentsResult = Awaited<ReturnType<typeof commands.nativeAgents>>;
  const resolvers: Array<(v: AgentsResult) => void> = [];
  const spy = spyOn(commands, "nativeAgents").mockImplementation(() => new Promise<AgentsResult>((resolve) => resolvers.push(resolve)));
  const first = useNative.getState().loadAgents(LOCAL_RUNNER, "p1"); // older fetch…
  const second = useNative.getState().loadAgents(LOCAL_RUNNER, "p1"); // …superseded by this one
  // The newer fetch resolves first with the fresh list.
  resolvers[1]({ status: "ok", data: [{ name: "newer", description: "Newer", mode: "subagent", builtin: true }] });
  await second;
  // The older fetch resolves late with the stale list — it must be ignored.
  resolvers[0]({ status: "ok", data: [{ name: "older", description: "Older", mode: "subagent", builtin: true }] });
  await first;
  expect(useNative.getState().agentsByProject.p1.map((a) => a.name)).toEqual(["newer"]);
  spy.mockRestore();
});

test("project command CRUD calls the generated APIs and updates only that project's cache", async () => {
  reset();
  const listed = spyOn(commands, "listProjectCommands").mockResolvedValue({ status: "ok", data: [projectCommand] });
  const created = spyOn(commands, "createProjectCommand").mockResolvedValue({ status: "ok", data: projectCommand });
  const updated = spyOn(commands, "updateProjectCommand").mockResolvedValue({
    status: "ok",
    data: { ...projectCommand, description: "Updated" },
  });
  const deleted = spyOn(commands, "deleteProjectCommand").mockResolvedValue({ status: "ok", data: null });

  await useNative.getState().loadProjectCommands(LOCAL_RUNNER, "p1");
  expect(listed).toHaveBeenCalledWith(LOCAL_RUNNER, "p1");
  expect(useNative.getState().projectCommandsByProject.p1).toEqual([projectCommand]);

  await useNative.getState().createProjectCommand(LOCAL_RUNNER, "p1", {
    name: "review",
    description: "Review the change",
    template: "Review $ARGUMENTS",
    agent: null,
    model: null,
    subtask: false,
  });
  expect(created).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", expect.objectContaining({ name: "review" }));

  await useNative.getState().updateProjectCommand(LOCAL_RUNNER, "p1", projectCommand, {
    description: "Updated",
    template: projectCommand.template,
    agent: null,
    model: null,
    subtask: false,
  });
  expect(updated).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "review", "rev-1", expect.objectContaining({ description: "Updated" }));
  expect(useNative.getState().projectCommandsByProject.p1[0]?.description).toBe("Updated");

  await useNative.getState().deleteProjectCommand(LOCAL_RUNNER, "p1", { ...projectCommand, description: "Updated" });
  expect(deleted).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "review", "rev-1");
  expect(useNative.getState().projectCommandsByProject.p1).toEqual([]);

  listed.mockRestore();
  created.mockRestore();
  updated.mockRestore();
  deleted.mockRestore();
});
test("successful project command create and delete refresh effective command cache", async () => {
  reset();
  const globalCommand = { ...effectiveGlobalCommand, name: "deploy" };
  const projectDeploy = { ...projectCommand, name: "deploy" };
  const effectiveProjectDeploy: CommandInfo = { ...globalCommand, origin: "project", shadowsGlobal: true };
  const nativeCommands = spyOn(commands, "nativeCommands")
    .mockResolvedValueOnce({ status: "ok", data: [globalCommand] })
    .mockResolvedValueOnce({ status: "ok", data: [effectiveProjectDeploy] })
    .mockResolvedValueOnce({ status: "ok", data: [globalCommand] });
  const created = spyOn(commands, "createProjectCommand").mockResolvedValue({ status: "ok", data: projectDeploy });
  const deleted = spyOn(commands, "deleteProjectCommand").mockResolvedValue({ status: "ok", data: null });

  await useNative.getState().loadCommands(LOCAL_RUNNER, "p1");
  await useNative.getState().createProjectCommand(LOCAL_RUNNER, "p1", projectDeploy);
  expect(useNative.getState().commandsByProject.p1).toEqual([effectiveProjectDeploy]);

  await useNative.getState().deleteProjectCommand(LOCAL_RUNNER, "p1", projectDeploy);
  expect(useNative.getState().commandsByProject.p1).toEqual([globalCommand]);
  expect(nativeCommands).toHaveBeenCalledTimes(3);

  nativeCommands.mockRestore();
  created.mockRestore();
  deleted.mockRestore();
});

test("a successful command mutation invalidates a deferred stale load", async () => {
  reset();
  type CommandsResult = Awaited<ReturnType<typeof commands.nativeCommands>>;
  const resolvers: Array<(result: CommandsResult) => void> = [];
  const nativeCommands = spyOn(commands, "nativeCommands").mockImplementation(
    () => new Promise<CommandsResult>((resolve) => resolvers.push(resolve)),
  );
  const createdCommand = { ...projectCommand, name: "ship" };
  const created = spyOn(commands, "createProjectCommand").mockResolvedValue({ status: "ok", data: createdCommand });
  const staleLoad = useNative.getState().loadCommands(LOCAL_RUNNER, "p1");
  const mutation = useNative.getState().createProjectCommand(LOCAL_RUNNER, "p1", createdCommand);

  await Promise.resolve();
  expect(nativeCommands).toHaveBeenCalledTimes(2);
  resolvers[1]({ status: "ok", data: [{ ...effectiveGlobalCommand, name: "ship", origin: "project", shadowsGlobal: true }] });
  await mutation;
  resolvers[0]({ status: "ok", data: [effectiveGlobalCommand] });
  await staleLoad;

  expect(useNative.getState().commandsByProject.p1).toEqual([
    { ...effectiveGlobalCommand, name: "ship", origin: "project", shadowsGlobal: true },
  ]);
  nativeCommands.mockRestore();
  created.mockRestore();
});

test("a successful command create ignores a failed effective-command reload", async () => {
  reset();
  const cachedCommand = { ...effectiveGlobalCommand, name: "deploy" };
  const createdCommand = { ...projectCommand, name: "deploy" };
  const nativeCommands = spyOn(commands, "nativeCommands")
    .mockResolvedValueOnce({ status: "ok", data: [cachedCommand] })
    .mockRejectedValueOnce(new Error("effective commands unavailable"));
  const created = spyOn(commands, "createProjectCommand").mockResolvedValue({ status: "ok", data: createdCommand });

  await useNative.getState().loadCommands(LOCAL_RUNNER, "p1");
  const result = await useNative.getState().createProjectCommand(LOCAL_RUNNER, "p1", createdCommand);

  expect(result).toEqual({ status: "success" });
  expect(useNative.getState().commandsByProject.p1).toEqual([cachedCommand]);
  nativeCommands.mockRestore();
  created.mockRestore();
});

test("a successful command mutation invalidates a deferred stale project-command load", async () => {
  reset();
  type CommandsResult = Awaited<ReturnType<typeof commands.listProjectCommands>>;
  const resolvers: Array<(result: CommandsResult) => void> = [];
  const listed = spyOn(commands, "listProjectCommands").mockImplementation(
    () => new Promise<CommandsResult>((resolve) => resolvers.push(resolve)),
  );
  const created = spyOn(commands, "createProjectCommand").mockResolvedValue({ status: "ok", data: { ...projectCommand, name: "ship" } });

  const staleLoad = useNative.getState().loadProjectCommands(LOCAL_RUNNER, "p1");
  await useNative.getState().createProjectCommand(LOCAL_RUNNER, "p1", { ...projectCommand, name: "ship" });
  resolvers[0]({ status: "ok", data: [projectCommand] });
  await staleLoad;

  expect(useNative.getState().projectCommandsByProject.p1.map((command) => command.name)).toEqual(["ship"]);
  listed.mockRestore();
  created.mockRestore();
});

test("command conflicts return structured outcomes and reload the latest project cache", async () => {
  reset();
  const listed = spyOn(commands, "listProjectCommands").mockResolvedValue({
    status: "ok",
    data: [{ ...projectCommand, description: "Latest", revision: "rev-2" }],
  });
  const updated = spyOn(commands, "updateProjectCommand").mockResolvedValue({
    status: "error",
    error: { message: "revision conflict" },
  });

  const result = await useNative.getState().updateProjectCommand(LOCAL_RUNNER, "p1", projectCommand, {
    description: "Mine",
    template: projectCommand.template,
    agent: null,
    model: null,
    subtask: false,
  });

  expect(result).toEqual({ status: "conflict", message: "revision conflict" });
  expect(listed).toHaveBeenCalledWith(LOCAL_RUNNER, "p1");
  expect(useNative.getState().projectCommandsByProject.p1[0]?.description).toBe("Latest");
  listed.mockRestore();
  updated.mockRestore();
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
      primaryAgentId: null,
      primaryAgentSnapshot: null,
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

test("a successful enqueue does not duplicate an id already loaded from the server", async () => {
  reset();
  type QueueResult = Awaited<ReturnType<typeof commands.sessionQueue>>;
  let resolveFetch!: (value: QueueResult) => void;
  const queue = spyOn(commands, "sessionQueue").mockImplementation(
    () =>
      new Promise<QueueResult>((resolve) => {
        resolveFetch = resolve;
      }),
  );
  const enqueue = spyOn(commands, "enqueueSessionMessage").mockResolvedValue({ status: "ok", data: { id: "new", text: "new message" } });

  const load = useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  resolveFetch({ status: "ok", data: [{ id: "new", text: "new message" }] });
  await load;
  expect(await useNative.getState().enqueueQueueMessage(LOCAL_RUNNER, "s1", "new message", null)).toBe(true);

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "new", text: "new message" }]);
  queue.mockRestore();
  enqueue.mockRestore();
});

test("a stale queue fetch cannot overwrite a successful enqueue", async () => {
  reset();
  type QueueResult = Awaited<ReturnType<typeof commands.sessionQueue>>;
  let resolveFetch!: (value: QueueResult) => void;
  const queue = spyOn(commands, "sessionQueue").mockImplementation(
    () =>
      new Promise<QueueResult>((resolve) => {
        resolveFetch = resolve;
      }),
  );
  const enqueue = spyOn(commands, "enqueueSessionMessage").mockResolvedValue({ status: "ok", data: { id: "new", text: "new message" } });

  const load = useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  expect(await useNative.getState().enqueueQueueMessage(LOCAL_RUNNER, "s1", "new message", null)).toBe(true);
  resolveFetch({ status: "ok", data: [{ id: "old", text: "stale message" }] });
  await load;

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "new", text: "new message" }]);
  queue.mockRestore();
  enqueue.mockRestore();
});

test("a stale queue fetch cannot restore a successfully removed message", async () => {
  reset();
  useNative.setState({ queuedBySession: { [s1]: [{ id: "remove", text: "remove me" }] } });
  type QueueResult = Awaited<ReturnType<typeof commands.sessionQueue>>;
  let resolveFetch!: (value: QueueResult) => void;
  const queue = spyOn(commands, "sessionQueue").mockImplementation(
    () =>
      new Promise<QueueResult>((resolve) => {
        resolveFetch = resolve;
      }),
  );
  const remove = spyOn(commands, "removeSessionMessage").mockResolvedValue({ status: "ok", data: true });

  const load = useNative.getState().loadQueue(LOCAL_RUNNER, "s1");
  expect(await useNative.getState().removeQueueMessage(LOCAL_RUNNER, "s1", "remove")).toBe(true);
  resolveFetch({ status: "ok", data: [{ id: "remove", text: "stale message" }] });
  await load;

  expect(useNative.getState().queuedBySession[s1]).toEqual([]);
  queue.mockRestore();
  remove.mockRestore();
});

test("a rejected queue load leaves cached messages unchanged", async () => {
  reset();
  useNative.setState({ queuedBySession: { [s1]: [{ id: "kept", text: "keep me" }] } });
  const queue = spyOn(commands, "sessionQueue").mockRejectedValue(new Error("boom"));

  await useNative.getState().loadQueue(LOCAL_RUNNER, "s1");

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "kept", text: "keep me" }]);
  queue.mockRestore();
});

test("a rejected queue enqueue returns false and leaves cached messages unchanged", async () => {
  reset();
  useNative.setState({ queuedBySession: { [s1]: [{ id: "kept", text: "keep me" }] } });
  const enqueue = spyOn(commands, "enqueueSessionMessage").mockRejectedValue(new Error("boom"));

  expect(await useNative.getState().enqueueQueueMessage(LOCAL_RUNNER, "s1", "new message", null)).toBe(false);

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "kept", text: "keep me" }]);
  enqueue.mockRestore();
});

test("a rejected queue removal returns false and leaves cached messages unchanged", async () => {
  reset();
  useNative.setState({ queuedBySession: { [s1]: [{ id: "kept", text: "keep me" }] } });
  const remove = spyOn(commands, "removeSessionMessage").mockRejectedValue(new Error("boom"));

  expect(await useNative.getState().removeQueueMessage(LOCAL_RUNNER, "s1", "kept")).toBe(false);

  expect(useNative.getState().queuedBySession[s1]).toEqual([{ id: "kept", text: "keep me" }]);
  remove.mockRestore();
});
