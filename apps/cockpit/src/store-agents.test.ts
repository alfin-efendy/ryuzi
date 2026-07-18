import { afterAll, beforeEach, expect, spyOn, test } from "bun:test";
import { toast } from "sonner";
import { commands } from "@/bindings";
import type {
  AgentDetailInfo,
  AgentModelInfo,
  AgentMutationInfo,
  AgentRegistryInfo,
  AgentSummaryInfo,
  CmdError,
  Result,
  SelectableModelInfo,
  Session,
} from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const ok = <T>(data: T): Result<T, CmdError> => ({ status: "ok", data });
const err = (message: string): Result<never, CmdError> => ({ status: "error", error: { message } });

// ---------- fixtures (fresh objects per call so optimistic mutation in the
// store can never leak between tests through a shared reference) ----------

const route = (r: string): AgentModelInfo => ({ kind: "route", route: r });

function summary(id: string, name: string, overrides: Partial<AgentSummaryInfo> = {}): AgentSummaryInfo {
  return {
    id,
    name,
    description: "",
    avatarColor: "#7C5CFF",
    model: route("free"),
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: id === "ryuzi",
    ...overrides,
  };
}

const ryuziSummary = () => summary("ryuzi", "Ryuzi");
const reviewerSummary = () => summary("reviewer", "Reviewer");

function registry(): AgentRegistryInfo {
  return {
    agents: [ryuziSummary(), reviewerSummary()],
    defaultAgentId: "ryuzi",
    recovery: [],
    subagentModel: route("free"),
  };
}

function detailOf(s: AgentSummaryInfo): AgentDetailInfo {
  return {
    summary: s,
    permissionRules: [],
    skills: [],
    nativeTools: [],
    pluginTools: [],
    apps: [],
    maxTurns: 40,
    maxToolRounds: 80,
    modelInfo: null,
    personality: { preset: "helpful", custom: null },
  };
}

const reviewerDetail = () => detailOf(reviewerSummary());

function reviewerInput(): AgentMutationInfo {
  return {
    name: "Reviewer",
    description: "",
    avatarColor: "#7C5CFF",
    model: route("free"),
    personality: { preset: "helpful", custom: null },
    permissionMode: "ask",
    permissionRules: [],
    skills: [],
    nativeTools: [],
    pluginTools: [],
    apps: [],
    maxTurns: 40,
    maxToolRounds: 80,
  };
}

const selectable = (requestValue: string): SelectableModelInfo => ({
  kind: "concrete",
  requestValue,
  displayName: requestValue,
  preferenceKey: null,
  supported: [],
  configuredDefault: null,
  resolvedDefault: null,
  defaultSource: "none",
});

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

// ---------- Tauri boundary spies ----------

const listAgents = spyOn(commands, "listAgents");
const getAgent = spyOn(commands, "getAgent");
const createAgent = spyOn(commands, "createAgent");
const updateAgent = spyOn(commands, "updateAgent");
const duplicateAgent = spyOn(commands, "duplicateAgent");
const deleteAgent = spyOn(commands, "deleteAgent");
const setDefaultAgent = spyOn(commands, "setDefaultAgent");
const updateSubagentModel = spyOn(commands, "updateSubagentModel");
const listSelectableModels = spyOn(commands, "listSelectableModels");
const listAgentSessions = spyOn(commands, "listAgentSessions");

const resetCommandMocks = () => {
  listAgents.mockImplementation(async () => ok(registry()));
  getAgent.mockImplementation(async (_r, id) => ok(id === "reviewer" ? reviewerDetail() : detailOf(ryuziSummary())));
  createAgent.mockImplementation(async () => ok(detailOf(summary("lead", "Lead"))));
  updateAgent.mockImplementation(async (_r, _id, input) => ok(detailOf(summary("reviewer", input.name))));
  duplicateAgent.mockImplementation(async () => ok(detailOf(summary("reviewer-copy", "Reviewer copy"))));
  deleteAgent.mockImplementation(async () => ok({ ...registry(), agents: [ryuziSummary()] }));
  setDefaultAgent.mockImplementation(async (_r, id) => {
    const reg = registry();
    return ok({
      ...reg,
      defaultAgentId: id,
      agents: reg.agents.map((agent) => ({ ...agent, isDefault: agent.id === id })),
    });
  });
  updateSubagentModel.mockImplementation(async (_r, model) => ok({ ...registry(), subagentModel: model }));
  listAgentSessions.mockImplementation(async () => ok([]));
  listSelectableModels.mockImplementation(async () => ok([selectable("free"), selectable("free-2")]));
};

const { useAgents } = await import("./store-agents");
const { useLearning } = await import("./store-learning");

const allMocks = [
  listAgents,
  getAgent,
  createAgent,
  updateAgent,
  duplicateAgent,
  deleteAgent,
  setDefaultAgent,
  updateSubagentModel,
  listSelectableModels,
  listAgentSessions,
];

beforeEach(() => {
  for (const command of allMocks) command.mockClear();
  resetCommandMocks();
  useAgents.setState({
    registry: null,
    detail: null,
    models: [],
    recentSessionsByAgent: {},
    loaded: false,
    loading: false,
    saving: false,
  });
  useLearning.setState({ byAgent: {}, loading: {}, rollingBack: {}, requestGeneration: {} });
});

afterAll(() => {
  for (const command of allMocks) command.mockRestore();
});

// ---------- load / loadDetail ----------

test("loadRecentSessions calls the generated command and stores sessions under their owner", async () => {
  const sessions: Session[] = [
    {
      sessionPk: "s1",
      primaryAgentId: "reviewer",
      primaryAgentSnapshot: { id: "reviewer", name: "Reviewer", avatarColor: "violet" },
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: null,
      title: "Review",
      status: "idle",
      permMode: "default",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 2,
      resumeAttempts: 0,
      branchOwned: false,
      kind: "project",
      speaker: null,
      agent: null,
      parentSessionPk: null,
    },
  ];
  listAgentSessions.mockResolvedValueOnce(ok(sessions));

  await useAgents.getState().loadRecentSessions("reviewer");

  expect(listAgentSessions).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer", 10);
  expect(useAgents.getState().recentSessionsByAgent).toEqual({ reviewer: sessions });
});

test("load hydrates registry and selected detail in parallel", async () => {
  await useAgents.getState().load("reviewer");
  expect(listAgents).toHaveBeenCalledWith(LOCAL_RUNNER);
  expect(listSelectableModels).toHaveBeenCalledWith(LOCAL_RUNNER);
  expect(getAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail?.summary.id).toBe("reviewer");
  expect(useAgents.getState().models.map((m) => m.requestValue)).toEqual(["free", "free-2"]);
  expect(useAgents.getState().loaded).toBe(true);
  expect(useAgents.getState().loading).toBe(false);
});

test("load without an agent id skips the detail fetch", async () => {
  await useAgents.getState().load();
  expect(getAgent).not.toHaveBeenCalled();
  expect(useAgents.getState().detail).toBeNull();
  expect(useAgents.getState().loaded).toBe(true);
});

test("a failed registry load surfaces a toast and leaves the store not loaded", async () => {
  const toastSpy = spyOn(toast, "error");
  listAgents.mockResolvedValueOnce(err("boom"));
  await useAgents.getState().load();
  expect(useAgents.getState().registry).toBeNull();
  expect(useAgents.getState().loaded).toBe(false);
  expect(useAgents.getState().loading).toBe(false);
  expect(toastSpy.mock.calls[0]?.[0]).toContain("boom");
  toastSpy.mockRestore();
});

test("loadDetail replaces the focused detail", async () => {
  await useAgents.getState().loadDetail("reviewer");
  expect(getAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("newest load wins and stale load cannot clear its busy state", async () => {
  const oldRegistry = deferred<Result<AgentRegistryInfo, CmdError>>();
  const newRegistry = deferred<Result<AgentRegistryInfo, CmdError>>();
  listAgents.mockReturnValueOnce(oldRegistry.promise).mockReturnValueOnce(newRegistry.promise);

  const oldLoad = useAgents.getState().load();
  const newLoad = useAgents.getState().load();
  newRegistry.resolve(ok({ ...registry(), defaultAgentId: "reviewer" }));
  await newLoad;
  expect(useAgents.getState().registry?.defaultAgentId).toBe("reviewer");
  expect(useAgents.getState().loading).toBe(true);

  oldRegistry.resolve(ok(registry()));
  await oldLoad;
  expect(useAgents.getState().registry?.defaultAgentId).toBe("reviewer");
  expect(useAgents.getState().loading).toBe(false);
});

test("newest detail request wins when detail responses arrive out of order", async () => {
  const oldDetail = deferred<Result<AgentDetailInfo, CmdError>>();
  const newDetail = deferred<Result<AgentDetailInfo, CmdError>>();
  getAgent.mockReturnValueOnce(oldDetail.promise).mockReturnValueOnce(newDetail.promise);

  const first = useAgents.getState().loadDetail("ryuzi");
  const second = useAgents.getState().loadDetail("reviewer");
  newDetail.resolve(ok(reviewerDetail()));
  await second;
  oldDetail.resolve(ok(detailOf(ryuziSummary())));
  await first;

  expect(useAgents.getState().detail?.summary.id).toBe("reviewer");
});

test("a mutation fences an older load from overwriting committed state", async () => {
  const staleRegistry = deferred<Result<AgentRegistryInfo, CmdError>>();
  listAgents.mockReturnValueOnce(staleRegistry.promise);
  const loading = useAgents.getState().load();

  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  expect(await useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Lead" })).toBe(true);
  staleRegistry.resolve(ok(registry()));
  await loading;

  expect(useAgents.getState().registry?.agents[1]?.name).toBe("Lead");
  expect(useAgents.getState().detail?.summary.name).toBe("Lead");
});

test("mutations execute in request order and saving stays true until the queue drains", async () => {
  const firstResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(firstResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const first = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "First" });
  const second = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Second" });
  await Promise.resolve();
  expect(updateAgent).toHaveBeenCalledTimes(1);
  expect(useAgents.getState().saving).toBe(true);

  firstResult.resolve(ok(detailOf(summary("reviewer", "First"))));
  expect(await first).toBe(true);
  expect(useAgents.getState().saving).toBe(true);
  expect(updateAgent).toHaveBeenCalledTimes(2);
  expect(await second).toBe(true);
  expect(useAgents.getState().saving).toBe(false);
  expect(useAgents.getState().detail?.summary.name).toBe("Second");
});

test("thrown load and optimistic mutation errors toast and roll back", async () => {
  const toastSpy = spyOn(toast, "error");
  listAgents.mockRejectedValueOnce(new Error("invoke unavailable"));
  await useAgents.getState().load();
  expect(useAgents.getState().loading).toBe(false);
  expect(toastSpy.mock.calls.some(([value]) => String(value).includes("invoke unavailable"))).toBe(true);

  updateAgent.mockRejectedValueOnce(new Error("transport closed"));
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  expect(await useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Lead" })).toBe(false);
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
  expect(useAgents.getState().registry).toEqual(registry());
  expect(toastSpy.mock.calls.some(([value]) => String(value).includes("transport closed"))).toBe(true);
  toastSpy.mockRestore();
});

test("reads begun during successful create and duplicate keep roster IDs unique", async () => {
  useAgents.setState({ registry: registry(), loaded: true });

  const createResult = deferred<Result<AgentDetailInfo, CmdError>>();
  createAgent.mockReturnValueOnce(createResult.promise);
  const creating = useAgents.getState().create({ ...reviewerInput(), name: "Lead" });
  await Promise.resolve();

  const lead = detailOf(summary("lead", "Lead"));
  listAgents.mockResolvedValueOnce(ok({ ...registry(), agents: [...registry().agents, lead.summary] }));
  const createRead = useAgents.getState().load();
  await createRead;
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(true);

  createResult.resolve(ok(lead));
  expect((await creating)?.summary.id).toBe("lead");
  expect(useAgents.getState().registry?.agents.map((agent) => agent.id)).toEqual(["ryuzi", "reviewer", "lead"]);
  expect(useAgents.getState().loading).toBe(false);

  listAgents.mockClear();
  const duplicateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  duplicateAgent.mockReturnValueOnce(duplicateResult.promise);
  const duplicating = useAgents.getState().duplicate("reviewer");
  await Promise.resolve();

  const copy = detailOf(summary("reviewer-copy", "Reviewer copy"));
  listAgents.mockResolvedValueOnce(ok({ ...registry(), agents: [...registry().agents, lead.summary, copy.summary] }));
  const duplicateRead = useAgents.getState().load();
  await duplicateRead;
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(true);

  duplicateResult.resolve(ok(copy));
  expect((await duplicating)?.summary.id).toBe("reviewer-copy");
  expect(useAgents.getState().registry?.agents.map((agent) => agent.id)).toEqual(["ryuzi", "reviewer", "lead", "reviewer-copy"]);
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(false);
});

test("a failed registry read does not suppress failed update rollback", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  listAgents.mockResolvedValueOnce(err("registry unavailable"));
  await useAgents.getState().load();

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("a successful registry read survives failed update while a failed detail read rolls back", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  const registryTruth = { ...registry(), agents: [ryuziSummary(), summary("reviewer", "Registry truth")] };
  listAgents.mockResolvedValueOnce(ok(registryTruth));
  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().load("reviewer");

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry).toEqual(registryTruth);
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("a failed detail-only read does not suppress failed update rollback", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().loadDetail("reviewer");

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("a failed detail read does not suppress default rollback", async () => {
  const defaultResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  setDefaultAgent.mockReturnValueOnce(defaultResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const settingDefault = useAgents.getState().setDefault("reviewer");
  await Promise.resolve();
  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().loadDetail("reviewer");
  defaultResult.resolve(err("default refused"));

  expect(await settingDefault).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("a successful detail read does not suppress default rollback", async () => {
  const defaultResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  setDefaultAgent.mockReturnValueOnce(defaultResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const settingDefault = useAgents.getState().setDefault("reviewer");
  await Promise.resolve();
  const detailTruth = detailOf(summary("reviewer", "Detail truth"));
  getAgent.mockResolvedValueOnce(ok(detailTruth));
  await useAgents.getState().loadDetail("reviewer");
  defaultResult.resolve(err("default refused"));

  expect(await settingDefault).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(detailTruth);
});

test("detail reads do not suppress subagent rollback", async () => {
  const failedReadResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  updateSubagentModel.mockReturnValueOnce(failedReadResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const firstUpdate = useAgents.getState().updateSubagentModel(route("free"));
  await Promise.resolve();
  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().loadDetail("reviewer");
  failedReadResult.resolve(err("subagent refused"));
  expect(await firstUpdate).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());

  const successfulReadResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  updateSubagentModel.mockReturnValueOnce(successfulReadResult.promise);
  const secondUpdate = useAgents.getState().updateSubagentModel(route("free"));
  await Promise.resolve();
  const detailTruth = detailOf(summary("reviewer", "Detail truth"));
  getAgent.mockResolvedValueOnce(ok(detailTruth));
  await useAgents.getState().loadDetail("reviewer");
  successfulReadResult.resolve(err("subagent refused"));

  expect(await secondUpdate).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(detailTruth);
});

test("successful registry reads survive failed default and subagent mutations when detail reads fail", async () => {
  const defaultResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  setDefaultAgent.mockReturnValueOnce(defaultResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const settingDefault = useAgents.getState().setDefault("reviewer");
  await Promise.resolve();
  const defaultRegistryTruth = { ...registry(), subagentModel: route("default-read-truth") };
  listAgents.mockResolvedValueOnce(ok(defaultRegistryTruth));
  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().load("reviewer");
  defaultResult.resolve(err("default refused"));

  expect(await settingDefault).toBe(false);
  expect(useAgents.getState().registry).toEqual(defaultRegistryTruth);
  expect(useAgents.getState().detail).toEqual(reviewerDetail());

  const subagentResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  updateSubagentModel.mockReturnValueOnce(subagentResult.promise);
  const updatingSubagent = useAgents.getState().updateSubagentModel(route("free"));
  await Promise.resolve();
  const subagentRegistryTruth = { ...registry(), defaultAgentId: "reviewer" };
  listAgents.mockResolvedValueOnce(ok(subagentRegistryTruth));
  getAgent.mockResolvedValueOnce(err("detail unavailable"));
  await useAgents.getState().load("reviewer");
  subagentResult.resolve(err("subagent refused"));

  expect(await updatingSubagent).toBe(false);
  expect(useAgents.getState().registry).toEqual(subagentRegistryTruth);
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
});

test("a successful detail-only read survives failed update while registry rolls back", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  const detailTruth = detailOf(summary("reviewer", "Detail truth"));
  getAgent.mockResolvedValueOnce(ok(detailTruth));
  await useAgents.getState().loadDetail("reviewer");

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(detailTruth);
});

test("a successful different-agent detail read survives failed update rollback", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  const ryuziDetail = detailOf(summary("ryuzi", "Ryuzi truth"));
  getAgent.mockResolvedValueOnce(ok(ryuziDetail));
  await useAgents.getState().loadDetail("ryuzi");

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(ryuziDetail);
});

test("a newer authoritative read survives failed update rollback", async () => {
  const updateResult = deferred<Result<AgentDetailInfo, CmdError>>();
  updateAgent.mockReturnValueOnce(updateResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const updating = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Optimistic" });
  await Promise.resolve();

  const readDetail = detailOf(summary("reviewer", "Read truth"));
  listAgents.mockResolvedValueOnce(ok({ ...registry(), agents: [ryuziSummary(), readDetail.summary] }));
  getAgent.mockResolvedValueOnce(ok(readDetail));
  const reading = useAgents.getState().load("reviewer");
  await reading;
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(true);

  updateResult.resolve(err("disk full"));
  expect(await updating).toBe(false);
  expect(useAgents.getState().registry?.agents[1]?.name).toBe("Read truth");
  expect(useAgents.getState().detail?.summary.name).toBe("Read truth");
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(false);
});

test("a read begun during a failed delete remains authoritative", async () => {
  const deleteResult = deferred<Result<AgentRegistryInfo, CmdError>>();
  deleteAgent.mockReturnValueOnce(deleteResult.promise);
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });

  const deleting = useAgents.getState().remove("reviewer");
  await Promise.resolve();

  const readRegistry = { ...registry(), subagentModel: route("free") };
  listAgents.mockResolvedValueOnce(ok(readRegistry));
  const reading = useAgents.getState().load();
  await reading;
  expect(useAgents.getState().registry).toEqual(readRegistry);
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(true);

  deleteResult.resolve(err("delete refused"));
  expect(await deleting).toBe(false);
  expect(useAgents.getState().registry).toEqual(readRegistry);
  expect(useAgents.getState().loading).toBe(false);
  expect(useAgents.getState().saving).toBe(false);
});

// ---------- create / duplicate ----------

test("create returns the new detail and appends it to the roster", async () => {
  useAgents.setState({ registry: registry(), loaded: true });
  const created = await useAgents.getState().create({ ...reviewerInput(), name: "Lead" });
  expect(created?.summary.id).toBe("lead");
  expect(useAgents.getState().registry?.agents.map((a) => a.id)).toEqual(["ryuzi", "reviewer", "lead"]);
  expect(useAgents.getState().detail?.summary.id).toBe("lead");
  expect(useAgents.getState().saving).toBe(false);
});

test("a failed create returns null and leaves the roster alone", async () => {
  const toastSpy = spyOn(toast, "error");
  createAgent.mockResolvedValueOnce(err("invalid name"));
  useAgents.setState({ registry: registry(), loaded: true });
  expect(await useAgents.getState().create(reviewerInput())).toBeNull();
  expect(useAgents.getState().registry).toEqual(registry());
  expect(toastSpy.mock.calls[0]?.[0]).toContain("invalid name");
  toastSpy.mockRestore();
});

test("duplicate returns the copy and appends it to the roster", async () => {
  useAgents.setState({ registry: registry(), loaded: true });
  const copy = await useAgents.getState().duplicate("reviewer");
  expect(duplicateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  expect(copy?.summary.id).toBe("reviewer-copy");
  expect(useAgents.getState().registry?.agents.map((a) => a.id)).toEqual(["ryuzi", "reviewer", "reviewer-copy"]);
});

// ---------- update ----------

test("update commits the server detail and patches the roster entry", async () => {
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  const okFlag = await useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Lead" });
  expect(okFlag).toBe(true);
  expect(updateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer", { ...reviewerInput(), name: "Lead" });
  expect(useAgents.getState().detail?.summary.name).toBe("Lead");
  expect(useAgents.getState().registry?.agents[1]?.name).toBe("Lead");
});

test("failed update rolls optimistic detail and roster back", async () => {
  const toastSpy = spyOn(toast, "error");
  updateAgent.mockResolvedValueOnce(err("disk full"));
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  const okFlag = await useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Lead" });
  expect(okFlag).toBe(false);
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
  expect(useAgents.getState().registry?.agents[1]?.name).toBe("Reviewer");
  expect(toastSpy.mock.calls[0]?.[0]).toContain("disk full");
  toastSpy.mockRestore();
});

test("update while a different agent's detail is focused never paints that detail into the target row", async () => {
  // Focused detail belongs to ryuzi; the update targets reviewer. The
  // optimistic window must patch the reviewer row from the mutation input,
  // never from ryuzi's summary — and must leave the focused detail alone.
  let resolveUpdate: (r: Result<AgentDetailInfo, CmdError>) => void = () => {};
  updateAgent.mockReturnValueOnce(
    new Promise<Result<AgentDetailInfo, CmdError>>((resolve) => {
      resolveUpdate = resolve;
    }),
  );
  useAgents.setState({ registry: registry(), detail: detailOf(ryuziSummary()), loaded: true });
  const pending = useAgents.getState().update("reviewer", { ...reviewerInput(), name: "Lead" });
  await Promise.resolve();

  const during = useAgents.getState();
  expect(during.saving).toBe(true);
  const reviewerRow = during.registry?.agents.find((a) => a.id === "reviewer");
  expect(reviewerRow?.id).toBe("reviewer");
  expect(reviewerRow?.name).toBe("Lead"); // representable field previews the edit
  expect(reviewerRow?.isDefault).toBe(false); // server-derived field untouched
  expect(during.registry?.agents.map((a) => a.id)).toEqual(["ryuzi", "reviewer"]);
  expect(during.detail?.summary.id).toBe("ryuzi");
  expect(during.detail?.summary.name).toBe("Ryuzi"); // focused detail untouched

  resolveUpdate(ok(detailOf(summary("reviewer", "Lead"))));
  expect(await pending).toBe(true);
  expect(useAgents.getState().registry?.agents[1]?.name).toBe("Lead");
  expect(useAgents.getState().detail?.summary.id).toBe("ryuzi");
});

// ---------- remove ----------

test("remove commits the server roster, clears matching detail, and evicts Learning state", async () => {
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  useLearning.setState({
    byAgent: { reviewer: {} as never, ryuzi: {} as never },
    loading: { reviewer: true, ryuzi: false },
    rollingBack: { reviewer: "snapshot-1", ryuzi: null },
    requestGeneration: { reviewer: 4, ryuzi: 2 },
  });
  expect(await useAgents.getState().remove("reviewer")).toBe(true);
  expect(deleteAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  expect(useAgents.getState().registry?.agents.map((a) => a.id)).toEqual(["ryuzi"]);
  expect(useAgents.getState().detail).toBeNull();
  expect(useLearning.getState().byAgent.reviewer).toBeUndefined();
  expect(useLearning.getState().loading.reviewer).toBeUndefined();
  expect(useLearning.getState().rollingBack.reviewer).toBeUndefined();
  expect(useLearning.getState().requestGeneration.reviewer).toBe(5);
  expect(useLearning.getState().byAgent.ryuzi).toBeDefined();
  expect(useLearning.getState().requestGeneration.ryuzi).toBe(2);
});

test("failed delete keeps the agent and Learning state visible", async () => {
  deleteAgent.mockResolvedValueOnce(err("at least one main agent must remain"));
  useAgents.setState({ registry: registry(), detail: reviewerDetail(), loaded: true });
  useLearning.setState({
    byAgent: { reviewer: {} as never },
    loading: { reviewer: false },
    rollingBack: { reviewer: null },
    requestGeneration: { reviewer: 4 },
  });
  expect(await useAgents.getState().remove("reviewer")).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail).toEqual(reviewerDetail());
  expect(useLearning.getState().byAgent.reviewer).toBeDefined();
  expect(useLearning.getState().requestGeneration.reviewer).toBe(4);
});

// ---------- setDefault / updateSubagentModel ----------

test("setDefault flips the default optimistically and keeps the server registry", async () => {
  useAgents.setState({ registry: registry(), loaded: true });
  const pending = useAgents.getState().setDefault("reviewer");
  expect(useAgents.getState().saving).toBe(true);
  expect(await pending).toBe(true);
  expect(useAgents.getState().saving).toBe(false);
  expect(setDefaultAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  const reg = useAgents.getState().registry;
  expect(reg?.defaultAgentId).toBe("reviewer");
  expect(reg?.agents.map((a) => a.isDefault)).toEqual([false, true]);
});

test("failed setDefault restores the previous default", async () => {
  setDefaultAgent.mockResolvedValueOnce(err("agent is not executable"));
  useAgents.setState({ registry: registry(), loaded: true });
  expect(await useAgents.getState().setDefault("reviewer")).toBe(false);
  expect(useAgents.getState().registry).toEqual(registry());
});

test("updateSubagentModel commits on ok and rolls back on error", async () => {
  useAgents.setState({ registry: registry(), loaded: true });
  expect(await useAgents.getState().updateSubagentModel(route("free"))).toBe(true);
  expect(updateSubagentModel).toHaveBeenCalledWith(LOCAL_RUNNER, route("free"));
  expect(useAgents.getState().registry?.subagentModel).toEqual(route("free"));

  updateSubagentModel.mockResolvedValueOnce(err("unknown route"));
  expect(await useAgents.getState().updateSubagentModel(route("bogus"))).toBe(false);
  expect(useAgents.getState().registry?.subagentModel).toEqual(route("free"));
});
