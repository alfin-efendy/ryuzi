import { beforeEach, expect, mock, spyOn, test } from "bun:test";
import { toast } from "sonner";
import type { AgentLearningInfo, CmdError, KnowledgeConceptInfo, KnowledgeConceptMutationInfo, Result } from "@/bindings";

const ok = <T>(data: T): Result<T, CmdError> => ({ status: "ok", data });
const err = (message: string): Result<never, CmdError> => ({ status: "error", error: { message } });

const concept = (title: string): KnowledgeConceptInfo => ({
  id: title.toLowerCase().split(" ").join("-"),
  relativePath: `memory/user/${title}.md`,
  conceptType: "memory",
  title,
  description: "",
  body: title,
  scope: "user",
  projectId: null,
  tags: [],
  timestamp: "2026-03-01T00:00:00Z",
});
const learning = (title: string): AgentLearningInfo => ({
  concepts: [concept(title)],
  invalid: [],
  journey: [],
  skillUsage: [],
  reviews: [],
  curator: { concept: null, lastEventId: null },
  curatorHistory: [],
});
const ryuziLearning = learning("Ryuzi memory");
const reviewerLearning = learning("Reviewer memory");
const conceptInput: KnowledgeConceptMutationInfo = {
  title: "Prefer concise summaries",
  description: "Keep reviews focused.",
  body: "Lead with the finding.",
  scope: "user",
  projectId: null,
  tags: ["reviews"],
};

const getAgentLearning = mock(async (id: string) => ok(id === "ryuzi" ? ryuziLearning : reviewerLearning));
const createAgentConcept = mock(async (_id: string, input: KnowledgeConceptMutationInfo) => ok(concept(input.title)));
const updateAgentConcept = mock(async (_id: string, conceptId: string, _input: KnowledgeConceptMutationInfo) => ok(concept(conceptId)));
const deleteAgentConcept = mock(async (_id: string, _conceptId: string) => ok(learning("After delete")));
const validateAgentConceptRaw = mock(async (_id: string, _path: string, _raw: string) => ok(concept("Validated")));
const replaceAgentConceptRaw = mock(async (_id: string, _path: string, _raw: string) => ok(concept("Replaced")));
const deleteInvalidAgentConcept = mock(async (_id: string, _path: string) => ok(learning("After repair delete")));
const rollbackAgentLearning = mock(async (_id: string, _snapshotId: string) => ok(learning("Rolled back")));

mock.module("@/bindings", () => ({
  commands: {
    getAgentLearning,
    createAgentConcept,
    updateAgentConcept,
    deleteAgentConcept,
    validateAgentConceptRaw,
    replaceAgentConceptRaw,
    deleteInvalidAgentConcept,
    rollbackAgentLearning,
  },
}));

const { useLearning } = await import("./store-learning");
const allMocks = [
  getAgentLearning,
  createAgentConcept,
  updateAgentConcept,
  deleteAgentConcept,
  validateAgentConceptRaw,
  replaceAgentConceptRaw,
  deleteInvalidAgentConcept,
  rollbackAgentLearning,
];

beforeEach(() => {
  for (const command of allMocks) command.mockClear();
  getAgentLearning.mockImplementation(async (id: string) => ok(id === "ryuzi" ? ryuziLearning : reviewerLearning));
  useLearning.setState({ byAgent: {}, loading: {}, rollingBack: {}, requestGeneration: {} });
});

test("load keeps Learning snapshots keyed by agent ID", async () => {
  await Promise.all([useLearning.getState().load("ryuzi"), useLearning.getState().load("reviewer")]);
  expect(useLearning.getState().byAgent.ryuzi).toEqual(ryuziLearning);
  expect(useLearning.getState().byAgent.reviewer).toEqual(reviewerLearning);
});

test("newest same-agent load wins when responses resolve out of order", async () => {
  let resolveOld!: (value: Result<AgentLearningInfo, CmdError>) => void;
  let resolveNew!: (value: Result<AgentLearningInfo, CmdError>) => void;
  getAgentLearning
    .mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)))
    .mockImplementationOnce(() => new Promise((resolve) => (resolveNew = resolve)));

  const oldLoad = useLearning.getState().load("reviewer");
  const newLoad = useLearning.getState().load("reviewer");
  resolveNew(ok(learning("Newest snapshot")));
  await newLoad;
  resolveOld(ok(learning("Stale snapshot")));
  await oldLoad;

  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Newest snapshot");
  expect(useLearning.getState().loading.reviewer).toBe(false);
});

test("eviction fences stale requests when an agent ID is recreated", async () => {
  let resolveDeleted!: (value: Result<AgentLearningInfo, CmdError>) => void;
  let resolveRecreated!: (value: Result<AgentLearningInfo, CmdError>) => void;
  getAgentLearning
    .mockImplementationOnce(() => new Promise((resolve) => (resolveDeleted = resolve)))
    .mockImplementationOnce(() => new Promise((resolve) => (resolveRecreated = resolve)));

  const deletedLoad = useLearning.getState().load("reviewer");
  useLearning.getState().evictAgent("reviewer");
  const recreatedLoad = useLearning.getState().load("reviewer");
  resolveRecreated(ok(learning("Recreated agent")));
  await recreatedLoad;
  resolveDeleted(ok(learning("Deleted agent")));
  await deletedLoad;

  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Recreated agent");
  expect(useLearning.getState().loading.reviewer).toBe(false);
});

test("mutation refresh cannot be overwritten by an older in-flight load", async () => {
  let resolveOld!: (value: Result<AgentLearningInfo, CmdError>) => void;
  getAgentLearning
    .mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)))
    .mockResolvedValueOnce(ok(learning("After mutation")));

  const oldLoad = useLearning.getState().load("reviewer");
  expect(await useLearning.getState().createConcept("reviewer", conceptInput)).toBe(true);
  resolveOld(ok(learning("Before mutation")));
  await oldLoad;

  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("After mutation");
});

test("concept mutations always send agent ID and reload only that agent", async () => {
  await useLearning.getState().createConcept("reviewer", conceptInput);
  expect(createAgentConcept).toHaveBeenCalledWith("reviewer", conceptInput);
  expect(getAgentLearning).toHaveBeenLastCalledWith("reviewer");
  expect(getAgentLearning).not.toHaveBeenCalledWith("ryuzi");
});

test("all successful memory mutations refresh or replace only their agent snapshot", async () => {
  useLearning.setState({ byAgent: { ryuzi: ryuziLearning, reviewer: reviewerLearning } });
  expect(await useLearning.getState().updateConcept("reviewer", "memory-1", conceptInput)).toBe(true);
  expect(updateAgentConcept).toHaveBeenCalledWith("reviewer", "memory-1", conceptInput);
  expect(await useLearning.getState().deleteConcept("reviewer", "memory-1")).toBe(true);
  expect(deleteAgentConcept).toHaveBeenCalledWith("reviewer", "memory-1");
  expect(await useLearning.getState().replaceRaw("reviewer", "memory/user/broken.md", "# fixed")).toBe(true);
  expect(replaceAgentConceptRaw).toHaveBeenCalledWith("reviewer", "memory/user/broken.md", "# fixed");
  expect(await useLearning.getState().deleteInvalid("reviewer", "memory/user/broken.md")).toBe(true);
  expect(deleteInvalidAgentConcept).toHaveBeenCalledWith("reviewer", "memory/user/broken.md");
  expect(useLearning.getState().byAgent.ryuzi).toEqual(ryuziLearning);
});

test("validateRaw returns the parsed concept without writing or reloading", async () => {
  expect((await useLearning.getState().validateRaw("reviewer", "memory/user/broken.md", "# fixed"))?.title).toBe("Validated");
  expect(validateAgentConceptRaw).toHaveBeenCalledWith("reviewer", "memory/user/broken.md", "# fixed");
  expect(replaceAgentConceptRaw).not.toHaveBeenCalled();
  expect(getAgentLearning).not.toHaveBeenCalled();
});

test("failed mutations preserve snapshots and never put backend paths in toasts", async () => {
  const toastSpy = spyOn(toast, "error");
  createAgentConcept.mockResolvedValueOnce(
    err(String.raw`journal write failed at C:\Users\Alice\AppData\Roaming\ryuzi\agents\reviewer\knowledge\tx`),
  );
  useLearning.setState({ byAgent: { reviewer: reviewerLearning } });
  expect(await useLearning.getState().createConcept("reviewer", conceptInput)).toBe(false);
  expect(useLearning.getState().byAgent.reviewer).toEqual(reviewerLearning);
  expect(toastSpy.mock.calls[0]?.[0]).toBe("Create memory failed");
  expect(toastSpy.mock.calls[0]?.[0]).not.toContain("C:\\Users");
  toastSpy.mockRestore();
});

test("failed curator rollback preserves the prior snapshot", async () => {
  useLearning.setState({ byAgent: { reviewer: reviewerLearning }, loading: {}, rollingBack: {} });
  rollbackAgentLearning.mockResolvedValueOnce(err("journal write failed"));
  expect(await useLearning.getState().rollback("reviewer", "snapshot-1")).toBe(false);
  expect(useLearning.getState().byAgent.reviewer).toEqual(reviewerLearning);
  expect(useLearning.getState().rollingBack.reviewer).toBeNull();
});

test("rollback result wins over an older load and clears loading", async () => {
  let resolveOld!: (value: Result<AgentLearningInfo, CmdError>) => void;
  getAgentLearning.mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)));

  const oldLoad = useLearning.getState().load("reviewer");
  expect(await useLearning.getState().rollback("reviewer", "snapshot-1")).toBe(true);
  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Rolled back");
  expect(useLearning.getState().loading.reviewer).toBe(false);

  resolveOld(ok(learning("Before rollback")));
  await oldLoad;
  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Rolled back");
  expect(useLearning.getState().loading.reviewer).toBe(false);
});

test("newer rollback remains active and wins when an older rollback resolves first", async () => {
  let resolveOld!: (value: Result<AgentLearningInfo, CmdError>) => void;
  let resolveNew!: (value: Result<AgentLearningInfo, CmdError>) => void;
  rollbackAgentLearning
    .mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)))
    .mockImplementationOnce(() => new Promise((resolve) => (resolveNew = resolve)));

  const oldRollback = useLearning.getState().rollback("reviewer", "snapshot-1");
  const newRollback = useLearning.getState().rollback("reviewer", "snapshot-2");
  resolveOld(ok(learning("Older rollback")));
  await oldRollback;
  expect(useLearning.getState().rollingBack.reviewer).toBe("snapshot-2");
  expect(useLearning.getState().byAgent.reviewer).toBeUndefined();

  resolveNew(ok(learning("Newest rollback")));
  await newRollback;
  expect(useLearning.getState().rollingBack.reviewer).toBeNull();
  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Newest rollback");
});

test("rollback started after a load wins when responses resolve out of order", async () => {
  let resolveLoad!: (value: Result<AgentLearningInfo, CmdError>) => void;
  let resolveRollback!: (value: Result<AgentLearningInfo, CmdError>) => void;
  getAgentLearning.mockImplementationOnce(() => new Promise((resolve) => (resolveLoad = resolve)));
  rollbackAgentLearning.mockImplementationOnce(() => new Promise((resolve) => (resolveRollback = resolve)));

  const load = useLearning.getState().load("reviewer");
  const rollback = useLearning.getState().rollback("reviewer", "snapshot-1");
  resolveLoad(ok(learning("Stale load")));
  await load;
  expect(useLearning.getState().loading.reviewer).toBe(false);

  resolveRollback(ok(learning("Rollback result")));
  await rollback;
  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Rollback result");
});

test("latest failed load clears loading without replacing the snapshot", async () => {
  useLearning.setState({ byAgent: { reviewer: reviewerLearning } });
  getAgentLearning.mockResolvedValueOnce(err("unavailable"));

  await useLearning.getState().load("reviewer");

  expect(useLearning.getState().byAgent.reviewer).toEqual(reviewerLearning);
  expect(useLearning.getState().loading.reviewer).toBe(false);
});

test("successful rollback stores the returned snapshot without a reload", async () => {
  expect(await useLearning.getState().rollback("reviewer", "snapshot-1")).toBe(true);
  expect(rollbackAgentLearning).toHaveBeenCalledWith("reviewer", "snapshot-1");
  expect(useLearning.getState().byAgent.reviewer?.concepts[0]?.title).toBe("Rolled back");
  expect(getAgentLearning).not.toHaveBeenCalled();
});
