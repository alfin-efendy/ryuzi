import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CuratorRun, type CuratorStatus, type LearningGraph, type SkillUsage } from "./bindings";

// Learning domain store (Task 12): the Cockpit-side state for the Learning
// panel's journey graph, memory editor, curator status, and skill usage —
// all read/written through the Task-11 `learning_*` / `curator_*` /
// `*_skill_usage`/`*_skill_pinned` Tauri commands.

export type MemoryScope = "global" | "user" | "project";
export const MEMORY_SCOPES: MemoryScope[] = ["global", "user", "project"];

type LearningState = {
  graph: LearningGraph;
  graphLoaded: boolean;
  skills: SkillUsage[];
  skillsLoaded: boolean;
  curator: CuratorStatus | null;
  curatorLoaded: boolean;
  memoryScope: MemoryScope;
  memory: string[];
  memoryLoaded: boolean;
  /** run id currently mid-rollback, or null — drives the button's busy state. */
  rollingBack: string | null;

  loadGraph: () => Promise<void>;
  loadSkills: () => Promise<void>;
  loadCurator: () => Promise<void>;
  loadMemory: (scope: MemoryScope) => Promise<void>;
  addMemory: (scope: MemoryScope, text: string) => Promise<boolean>;
  replaceMemory: (scope: MemoryScope, matcher: string, text: string) => Promise<boolean>;
  removeMemory: (scope: MemoryScope, matcher: string) => Promise<boolean>;
  setSkillPinned: (name: string, pinned: boolean) => Promise<void>;
  rollbackCurator: (runId: string) => Promise<void>;
};

export const useLearning = create<LearningState>((set, get) => ({
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

  loadGraph: async () => {
    const res = await commands.learningGraph();
    if (res.status === "ok") set({ graph: res.data, graphLoaded: true });
    else toast.error(`Learning graph failed: ${res.error.message}`);
  },

  loadSkills: async () => {
    const res = await commands.listSkillUsage();
    if (res.status === "ok") set({ skills: res.data, skillsLoaded: true });
    else toast.error(`Skill usage failed: ${res.error.message}`);
  },

  loadCurator: async () => {
    const res = await commands.curatorStatus();
    if (res.status === "ok") set({ curator: res.data, curatorLoaded: true });
    else toast.error(`Curator status failed: ${res.error.message}`);
  },

  loadMemory: async (scope) => {
    const res = await commands.readMemory(scope);
    if (res.status === "ok") set({ memoryScope: scope, memory: res.data, memoryLoaded: true });
    else toast.error(`Memory load failed: ${res.error.message}`);
  },

  addMemory: async (scope, text) => {
    const res = await commands.writeMemory(scope, "add", text, null);
    if (res.status === "error") {
      toast.error(`Memory add failed: ${res.error.message}`);
      return false;
    }
    await get().loadMemory(scope);
    return true;
  },

  replaceMemory: async (scope, matcher, text) => {
    const res = await commands.writeMemory(scope, "replace", text, matcher);
    if (res.status === "error") {
      toast.error(`Memory update failed: ${res.error.message}`);
      return false;
    }
    await get().loadMemory(scope);
    return true;
  },

  removeMemory: async (scope, matcher) => {
    const res = await commands.writeMemory(scope, "remove", null, matcher);
    if (res.status === "error") {
      toast.error(`Memory remove failed: ${res.error.message}`);
      return false;
    }
    await get().loadMemory(scope);
    return true;
  },

  setSkillPinned: async (name, pinned) => {
    // Optimistic paint (mirrors usePlugins.pin) so the toggle feels instant;
    // the reload below reconciles with the persisted `skill_usage.pinned`
    // flag either way — success or error — so a failed write never leaves a
    // stale optimistic pin behind.
    set({ skills: get().skills.map((s) => (s.name === name ? { ...s, pinned } : s)) });
    const res = await commands.setSkillPinned(name, pinned);
    if (res.status === "error") toast.error(`Pin update failed: ${res.error.message}`);
    await get().loadSkills();
  },

  // `snapshotPath` is always null until the opt-in consolidation pass ships
  // (Task-12 resolution #2), so this always errors today — the UI gates the
  // control on `canRollback` and disables it, but if it's ever invoked the
  // failure must reach the user, never be swallowed.
  rollbackCurator: async (runId) => {
    set({ rollingBack: runId });
    try {
      const res = await commands.curatorRollback(runId);
      if (res.status === "error") {
        toast.error(`Rollback failed: ${res.error.message}`);
        return;
      }
      toast.success("Curator run rolled back");
      await get().loadCurator();
    } finally {
      set({ rollingBack: null });
    }
  },
}));

/** Whether a curator run's rollback control may be enabled. Only a
 *  consolidation run produces a pre-mutation snapshot (`snapshotPath`); a
 *  plain deterministic sweep has nothing to roll back to. Both fields are
 *  checked (not just `consolidated`) so a run that claims consolidation but
 *  recorded no snapshot path never enables the control. */
export function canRollback(run: Pick<CuratorRun, "consolidated" | "snapshotPath">): boolean {
  return run.consolidated === true && run.snapshotPath !== null;
}

/** One row in the self-improvement activity feed. */
export type ActivityItem = { kind: "skill"; at: number; skill: SkillUsage } | { kind: "curator"; at: number; run: CuratorRun };

/** Compose the "what the learning loop did" feed (Task-12 resolution #1):
 *  skills the agent authored or patched, plus curator sweep runs, merged
 *  newest-first by timestamp. Skills the user authored and never patched are
 *  ordinary catalog entries, not learning-loop activity, so they're dropped. */
export function buildActivityFeed(skills: SkillUsage[], curatorRuns: CuratorRun[]): ActivityItem[] {
  const skillItems: ActivityItem[] = skills
    .filter((s) => s.createdBy === "agent" || s.patchCount > 0)
    .map((s) => ({ kind: "skill", at: s.lastPatchedAt ?? s.lastUsedAt ?? s.createdAt ?? 0, skill: s }));
  const curatorItems: ActivityItem[] = curatorRuns.map((r) => ({ kind: "curator", at: r.startedAt, run: r }));
  return [...skillItems, ...curatorItems].sort((a, b) => b.at - a.at);
}

/** Compact relative-time label ("3h ago", "just now") for feed rows — no
 *  shared date-formatting helper exists yet elsewhere in Cockpit, so this
 *  stays local and pure. */
export function formatRelativeTime(ms: number, now: number = Date.now()): string {
  const deltaSec = Math.max(0, Math.round((now - ms) / 1000));
  if (deltaSec < 60) return "just now";
  const deltaMin = Math.round(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m ago`;
  const deltaHr = Math.round(deltaMin / 60);
  if (deltaHr < 24) return `${deltaHr}h ago`;
  const deltaDay = Math.round(deltaHr / 24);
  if (deltaDay < 30) return `${deltaDay}d ago`;
  const deltaMonth = Math.round(deltaDay / 30);
  if (deltaMonth < 12) return `${deltaMonth}mo ago`;
  return `${Math.round(deltaMonth / 12)}y ago`;
}
