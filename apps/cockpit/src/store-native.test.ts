import { afterEach, expect, spyOn, test } from "bun:test";
import type { CommandInfo, ProjectCommandInfo } from "./bindings";
import { useNative } from "./store-native";
import { commands } from "./bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const s1 = sessKey(LOCAL_RUNNER, "s1");
const s2 = sessKey(LOCAL_RUNNER, "s2");

function reset() {
  useNative.setState({ agentsByProject: {}, commandsByProject: {}, projectCommandsByProject: {}, todosBySession: {} });
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

test("loadTodos tokens are per-session — one session's fetch never invalidates another's", async () => {
  reset();
  type TodosResult = Awaited<ReturnType<typeof commands.sessionTodos>>;
  const resolvers: Array<(v: TodosResult) => void> = [];
  const spy = spyOn(commands, "sessionTodos").mockImplementation(() => new Promise<TodosResult>((resolve) => resolvers.push(resolve)));
  const a = useNative.getState().loadTodos(LOCAL_RUNNER, "s1");
  const b = useNative.getState().loadTodos(LOCAL_RUNNER, "s2"); // different session, issued later
  resolvers[1]({ status: "ok", data: [{ content: "other", status: "pending" }] });
  await b;
  resolvers[0]({ status: "ok", data: [{ content: "mine", status: "pending" }] });
  await a;
  expect(useNative.getState().todosBySession[s1]).toEqual([{ content: "mine", status: "pending" }]);
  expect(useNative.getState().todosBySession[s2]).toEqual([{ content: "other", status: "pending" }]);
  spy.mockRestore();
});
