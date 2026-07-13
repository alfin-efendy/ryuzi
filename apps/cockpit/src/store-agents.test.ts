import { beforeEach, expect, mock, spyOn, test } from "bun:test";
import { toast } from "sonner";
import type {
  AgentDetailInfo,
  AgentModelInfo,
  AgentMutationInfo,
  AgentRegistryInfo,
  AgentSummaryInfo,
  CmdError,
  Result,
  SelectableModelInfo,
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
    model: route("smart"),
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
    subagentModel: route("fast"),
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
  };
}

const reviewerDetail = () => detailOf(reviewerSummary());

function reviewerInput(): AgentMutationInfo {
  return {
    name: "Reviewer",
    description: "",
    avatarColor: "#7C5CFF",
    model: route("smart"),
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

// ---------- Tauri boundary mocks (mock.module pattern; the destructured
// names below are test-local variables — production code always goes
// through `commands.*`) ----------

const listAgents = mock(async (_r: string | null) => ok(registry()));
const getAgent = mock(async (_r: string | null, id: string) => ok(id === "reviewer" ? reviewerDetail() : detailOf(ryuziSummary())));
const createAgent = mock(async (_r: string | null, _input: AgentMutationInfo) => ok(detailOf(summary("lead", "Lead"))));
const updateAgent = mock(async (_r: string | null, _id: string, input: AgentMutationInfo) => ok(detailOf(summary("reviewer", input.name))));
const duplicateAgent = mock(async (_r: string | null, _id: string) => ok(detailOf(summary("reviewer-copy", "Reviewer copy"))));
const deleteAgent = mock(async (_r: string | null, _id: string) =>
  ok({ ...registry(), agents: [ryuziSummary()] } satisfies AgentRegistryInfo),
);
const setDefaultAgent = mock(async (_r: string | null, id: string) => {
  const reg = registry();
  return ok({
    ...reg,
    defaultAgentId: id,
    agents: reg.agents.map((a) => ({ ...a, isDefault: a.id === id })),
  } satisfies AgentRegistryInfo);
});
const getSubagentModel = mock(async (_r: string | null) => ok(route("fast")));
const updateSubagentModel = mock(async (_r: string | null, model: AgentModelInfo) =>
  ok({ ...registry(), subagentModel: model } satisfies AgentRegistryInfo),
);
const listSelectableModels = mock(async (_r: string | null) => ok([selectable("smart"), selectable("fast")]));

mock.module("@/bindings", () => ({
  commands: {
    listAgents,
    getAgent,
    createAgent,
    updateAgent,
    duplicateAgent,
    deleteAgent,
    setDefaultAgent,
    getSubagentModel,
    updateSubagentModel,
    listSelectableModels,
  },
  events: {},
}));

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
  getSubagentModel,
  updateSubagentModel,
  listSelectableModels,
];

beforeEach(() => {
  for (const m of allMocks) m.mockClear();
  useAgents.setState({
    registry: null,
    detail: null,
    models: [],
    loaded: false,
    loading: false,
    saving: false,
  });
  useLearning.setState({ byAgent: {}, loading: {}, rollingBack: {}, requestGeneration: {} });
});

// ---------- load / loadDetail ----------

test("load hydrates registry and selected detail in parallel", async () => {
  await useAgents.getState().load("reviewer");
  expect(listAgents).toHaveBeenCalledWith(LOCAL_RUNNER);
  expect(listSelectableModels).toHaveBeenCalledWith(LOCAL_RUNNER);
  expect(getAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer");
  expect(useAgents.getState().registry).toEqual(registry());
  expect(useAgents.getState().detail?.summary.id).toBe("reviewer");
  expect(useAgents.getState().models.map((m) => m.requestValue)).toEqual(["smart", "fast"]);
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
  expect(await useAgents.getState().updateSubagentModel(route("smart"))).toBe(true);
  expect(updateSubagentModel).toHaveBeenCalledWith(LOCAL_RUNNER, route("smart"));
  expect(useAgents.getState().registry?.subagentModel).toEqual(route("smart"));

  updateSubagentModel.mockResolvedValueOnce(err("unknown route"));
  expect(await useAgents.getState().updateSubagentModel(route("bogus"))).toBe(false);
  expect(useAgents.getState().registry?.subagentModel).toEqual(route("smart"));
});
