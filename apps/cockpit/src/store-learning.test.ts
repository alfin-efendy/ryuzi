import { test, expect, spyOn } from "bun:test";
import { useLearning, buildActivityFeed, canRollback } from "./store-learning";
import { commands, type CuratorRun, type SkillUsage } from "./bindings";

function reset() {
  useLearning.setState({
    graph: { nodes: [], edges: [] },
    graphLoaded: false,
    skills: [],
    skillsLoaded: false,
    curator: null,
    curatorLoaded: false,
    memoryScope: "global",
    memory: [],
    memoryLoaded: false,
    rollingBack: null,
  });
}

// ---------- loadGraph (Step 1 of the Task-12 brief, adapted to this repo's
// spyOn(commands, ...) test convention — see store-native.test.ts) ----------

test("loadGraph commits nodes only on ok", async () => {
  reset();
  const spy = spyOn(commands, "learningGraph").mockResolvedValue({
    status: "ok",
    data: { nodes: [{ id: "skill:deploy", kind: "skill", label: "deploy", state: "active", scope: null }], edges: [] },
  });
  await useLearning.getState().loadGraph();
  expect(useLearning.getState().graph.nodes.map((n) => n.id)).toEqual(["skill:deploy"]);
  expect(useLearning.getState().graphLoaded).toBe(true);
  spy.mockRestore();
});

test("loadGraph leaves the graph untouched on error", async () => {
  reset();
  const spy = spyOn(commands, "learningGraph").mockResolvedValue({ status: "error", error: { message: "boom" } });
  await useLearning.getState().loadGraph();
  expect(useLearning.getState().graph.nodes).toEqual([]);
  expect(useLearning.getState().graphLoaded).toBe(false);
  spy.mockRestore();
});

test("loadCurator commits the run history only on ok", async () => {
  reset();
  const spy = spyOn(commands, "curatorStatus").mockResolvedValue({
    status: "ok",
    data: { lastRunAt: 5000, recent: [] },
  });
  await useLearning.getState().loadCurator();
  expect(useLearning.getState().curator).toEqual({ lastRunAt: 5000, recent: [] });
  spy.mockRestore();
});

test("loadSkills commits the skill list only on ok", async () => {
  reset();
  const spy = spyOn(commands, "listSkillUsage").mockResolvedValue({ status: "ok", data: [] });
  await useLearning.getState().loadSkills();
  expect(useLearning.getState().skillsLoaded).toBe(true);
  spy.mockRestore();
});

// ---------- ReviewFeed composition (Task-12 resolution #1: the activity feed
// is composed client-side from skill_usage + curator run history, since no
// dedicated review-notice command exists) ----------

function skill(overrides: Partial<SkillUsage>): SkillUsage {
  return {
    name: "deploy",
    createdBy: null,
    useCount: 0,
    viewCount: 0,
    patchCount: 0,
    lastUsedAt: null,
    lastViewedAt: null,
    lastPatchedAt: null,
    state: "active",
    pinned: false,
    archivedAt: null,
    createdAt: 1000,
    ...overrides,
  };
}

function run(overrides: Partial<CuratorRun>): CuratorRun {
  return {
    id: "run-1",
    startedAt: 2000,
    finishedAt: 2100,
    status: "ok",
    transitioned: 0,
    consolidated: false,
    snapshotPath: null,
    error: null,
    log: null,
    ...overrides,
  };
}

test("buildActivityFeed keeps agent-authored and patched skills, drops the rest, and sorts newest-first", () => {
  const agentAuthored = skill({ name: "deploy", createdBy: "agent", createdAt: 1000 });
  const humanPatched = skill({ name: "review", createdBy: null, patchCount: 2, lastPatchedAt: 5000 });
  const untouchedHuman = skill({ name: "unused", createdBy: null, patchCount: 0 });
  const curatorRun = run({ id: "sweep-1", startedAt: 3000 });

  const feed = buildActivityFeed([agentAuthored, humanPatched, untouchedHuman], [curatorRun]);

  // The untouched human-authored, never-patched skill must not appear.
  expect(feed.some((i) => i.kind === "skill" && i.skill.name === "unused")).toBe(false);
  // Newest first: humanPatched (5000) > curatorRun (3000) > agentAuthored (1000).
  expect(feed.map((i) => (i.kind === "skill" ? i.skill.name : i.run.id))).toEqual(["review", "sweep-1", "deploy"]);
});

test("buildActivityFeed falls back through lastPatchedAt -> lastUsedAt -> createdAt for its sort key", () => {
  const onlyCreated = skill({ name: "a", createdBy: "agent", createdAt: 100, lastUsedAt: null, lastPatchedAt: null });
  const usedNotPatched = skill({ name: "b", createdBy: "agent", createdAt: 50, lastUsedAt: 200, lastPatchedAt: null });
  const feed = buildActivityFeed([onlyCreated, usedNotPatched], []);
  expect(feed.map((i) => (i.kind === "skill" ? i.skill.name : i.run.id))).toEqual(["b", "a"]);
});

// ---------- rollback gating (Task-12 resolution #2: rollback only becomes
// available once a consolidation run produced a snapshot to roll back to) ----------

test("canRollback is true only for a consolidated run with a snapshot path", () => {
  expect(canRollback(run({ consolidated: true, snapshotPath: "/tmp/snap.tar.gz" }))).toBe(true);
  expect(canRollback(run({ consolidated: false, snapshotPath: null }))).toBe(false);
  // Defensive: consolidated flag true but no snapshot recorded (should not happen, but never enable the button).
  expect(canRollback(run({ consolidated: true, snapshotPath: null }))).toBe(false);
});

// ---------- memory writes ----------

test("addMemory reloads the scope's entries on ok", async () => {
  reset();
  const writeSpy = spyOn(commands, "writeMemory").mockResolvedValue({ status: "ok", data: null });
  const readSpy = spyOn(commands, "readMemory").mockResolvedValue({ status: "ok", data: ["prefers dark mode"] });
  const ok = await useLearning.getState().addMemory("user", "prefers dark mode");
  expect(ok).toBe(true);
  expect(writeSpy).toHaveBeenCalledWith("user", "add", "prefers dark mode", null);
  expect(useLearning.getState().memory).toEqual(["prefers dark mode"]);
  writeSpy.mockRestore();
  readSpy.mockRestore();
});

test("removeMemory reports failure and does not reload on error", async () => {
  reset();
  const writeSpy = spyOn(commands, "writeMemory").mockResolvedValue({ status: "error", error: { message: "no entry contains `x`" } });
  const readSpy = spyOn(commands, "readMemory");
  const ok = await useLearning.getState().removeMemory("global", "x");
  expect(ok).toBe(false);
  expect(readSpy).not.toHaveBeenCalled();
  writeSpy.mockRestore();
  readSpy.mockRestore();
});

test("setSkillPinned paints optimistically then reconciles from the reload", async () => {
  reset();
  useLearning.setState({ skills: [skill({ name: "deploy", pinned: false })] });
  const setSpy = spyOn(commands, "setSkillPinned").mockResolvedValue({ status: "ok", data: null });
  const listSpy = spyOn(commands, "listSkillUsage").mockResolvedValue({ status: "ok", data: [skill({ name: "deploy", pinned: true })] });
  await useLearning.getState().setSkillPinned("deploy", true);
  expect(setSpy).toHaveBeenCalledWith("deploy", true);
  expect(useLearning.getState().skills[0].pinned).toBe(true);
  setSpy.mockRestore();
  listSpy.mockRestore();
});
