import { describe, expect, test } from "bun:test";
import { goBackHistory, goForwardHistory, navigateHistory, useNav, type NavHistory, type View, choosePrimaryAgent } from "./store-nav";
import type { AgentSummaryInfo } from "./bindings";

const home: View = { kind: "home" };
const models: View = { kind: "models" };
const detail: View = { kind: "providerDetail", provider: "openai" };

const start: NavHistory = { back: [], current: home, forward: [] };

describe("nav history", () => {
  test("navigate pushes current onto back and clears forward", () => {
    const h1 = navigateHistory(start, models);
    expect(h1.current).toEqual(models);
    expect(h1.back).toEqual([home]);
    const h2 = navigateHistory(h1, detail);
    expect(h2.back).toEqual([home, models]);
    expect(h2.forward).toEqual([]);
  });

  test("navigating to the same view is a no-op", () => {
    expect(navigateHistory(start, home)).toBe(start);
  });

  test("back and forward walk the stacks", () => {
    const h = navigateHistory(navigateHistory(start, models), detail);
    const back1 = goBackHistory(h);
    expect(back1.current).toEqual(models);
    expect(back1.forward).toEqual([detail]);
    const back2 = goBackHistory(back1);
    expect(back2.current).toEqual(home);
    const fwd = goForwardHistory(back2);
    expect(fwd.current).toEqual(models);
    expect(fwd.forward).toEqual([detail]);
  });

  test("back at the root and forward at the tip are no-ops", () => {
    expect(goBackHistory(start)).toBe(start);
    expect(goForwardHistory(start)).toBe(start);
  });

  test("navigate after back drops the forward branch", () => {
    const h = goBackHistory(navigateHistory(start, models));
    const h2 = navigateHistory(h, detail);
    expect(h2.forward).toEqual([]);
    expect(h2.current).toEqual(detail);
  });
});

import { sanitizeRightTab, clampPanelSize, readClampedPanelSize, RIGHT_WIDTH, BOTTOM_HEIGHT } from "./store-nav";

test("sanitizeRightTab keeps valid tabs and maps legacy/unknown to review", () => {
  expect(sanitizeRightTab("file")).toBe("file");
  expect(sanitizeRightTab("review")).toBe("review");
  expect(sanitizeRightTab("term")).toBe("review"); // legacy persisted value
  expect(sanitizeRightTab(null)).toBe("review");
});

test("clampPanelSize clamps to min and viewport fraction", () => {
  expect(clampPanelSize(100, 1600, RIGHT_WIDTH)).toBe(320);
  expect(clampPanelSize(560, 1600, RIGHT_WIDTH)).toBe(560);
  expect(clampPanelSize(5000, 1600, RIGHT_WIDTH)).toBe(1280); // 80% of 1600
  expect(clampPanelSize(50, 900, BOTTOM_HEIGHT)).toBe(120);
  expect(clampPanelSize(1000, 900, BOTTOM_HEIGHT)).toBe(540); // 60% of 900
});

test("readClampedPanelSize parses, defaults, and clamps a persisted size to the viewport", () => {
  expect(readClampedPanelSize("560", 1600, RIGHT_WIDTH)).toBe(560); // stored & in range
  expect(readClampedPanelSize("5000", 1600, RIGHT_WIDTH)).toBe(1280); // saved on a bigger monitor → shrunk
  expect(readClampedPanelSize("100", 1600, RIGHT_WIDTH)).toBe(320); // below min → min
  expect(readClampedPanelSize(null, 1600, RIGHT_WIDTH)).toBe(RIGHT_WIDTH.def); // nothing persisted
  expect(readClampedPanelSize("garbage", 1600, RIGHT_WIDTH)).toBe(RIGHT_WIDTH.def); // invalid → default
  expect(readClampedPanelSize("0", 1600, RIGHT_WIDTH)).toBe(RIGHT_WIDTH.def); // non-positive → default
  expect(readClampedPanelSize("2000", 900, BOTTOM_HEIGHT)).toBe(540); // 60% of 900
});

test("composer nav omits obsolete chat-only model and effort state", () => {
  const nav = useNav.getState() as unknown as Record<string, unknown>;
  expect(nav.composerModel).toBeUndefined();
  expect(nav.setComposerModel).toBeUndefined();
  expect(nav.composerEffort).toBeUndefined();
  expect(nav.setComposerEffort).toBeUndefined();
});

test("composer git controls default to worktree ON, no branch until the list loads", () => {
  const s = useNav.getState();
  expect(s.composerBranch).toBeNull();
  expect(s.composerUseWorktree).toBe(true);
});

test("composer git setters update and reset state", () => {
  useNav.getState().setComposerBranch("feature/x");
  useNav.getState().setComposerUseWorktree(false);
  expect(useNav.getState().composerBranch).toBe("feature/x");
  expect(useNav.getState().composerUseWorktree).toBe(false);
  // setComposerBranch(null) clears the selection (project switch).
  useNav.getState().setComposerBranch(null);
  expect(useNav.getState().composerBranch).toBeNull();
  // restore defaults for other tests in this file
  useNav.getState().setComposerUseWorktree(true);
});

import { readDrafts, upsertDraft } from "./store-nav";

test("readDrafts parses the persisted map and collapses garbage to {}", () => {
  expect(readDrafts(null)).toEqual({});
  expect(readDrafts("not json")).toEqual({});
  expect(readDrafts('["a"]')).toEqual({});
  expect(readDrafts('{"s1":"hello","home:p1":"hi","bad":42,"empty":""}')).toEqual({ s1: "hello", "home:p1": "hi" });
});

test("upsertDraft sets, replaces, and deletes-on-empty per key", () => {
  const a = upsertDraft({}, "s1", "hello");
  expect(a).toEqual({ s1: "hello" });
  const b = upsertDraft(a, "home:p1", "hi");
  expect(b).toEqual({ s1: "hello", "home:p1": "hi" });
  expect(upsertDraft(b, "s1", "edited")).toEqual({ s1: "edited", "home:p1": "hi" });
  expect(upsertDraft(b, "s1", "")).toEqual({ "home:p1": "hi" });
  expect(upsertDraft(b, "missing", "")).toBe(b); // identity no-op: nothing to delete
});

test("draft actions: per-key isolation, clear, restore only into an empty slot", () => {
  useNav.setState({ drafts: {} });
  useNav.getState().setDraft("s1", "draft one");
  useNav.getState().setDraft("home:p1", "draft two");
  expect(useNav.getState().drafts).toEqual({ s1: "draft one", "home:p1": "draft two" });
  useNav.getState().clearDraft("s1");
  expect(useNav.getState().drafts).toEqual({ "home:p1": "draft two" });
  // Failed send: restore refills the cleared key…
  useNav.getState().restoreDraft("s1", "draft one");
  expect(useNav.getState().drafts.s1).toBe("draft one");
  // …but never clobbers text the user typed in the meantime.
  useNav.getState().restoreDraft("home:p1", "stale resend");
  expect(useNav.getState().drafts["home:p1"]).toBe("draft two");
  useNav.setState({ drafts: {} });
});

test("setDraft persists the map to localStorage", () => {
  useNav.setState({ drafts: {} });
  useNav.getState().setDraft("s9", "keep me");
  expect(JSON.parse(localStorage.getItem("cockpit.composer.drafts") ?? "{}")).toEqual({ s9: "keep me" });
  useNav.getState().clearDraft("s9");
  expect(JSON.parse(localStorage.getItem("cockpit.composer.drafts") ?? "{}")).toEqual({});
  useNav.setState({ drafts: {} });
});

test("sanitizeRightTab accepts agents, keeps file/review, falls back on unknown", () => {
  expect(sanitizeRightTab("agents")).toBe("agents");
  expect(sanitizeRightTab("file")).toBe("file");
  expect(sanitizeRightTab("review")).toBe("review");
  expect(sanitizeRightTab("bogus")).toBe("review");
  expect(sanitizeRightTab(null)).toBe("review");
});

test("agent detail participates in browser-style history", () => {
  const agentsStart: NavHistory = { back: [], current: { kind: "agents" } as View, forward: [] };
  const next = navigateHistory(agentsStart, { kind: "agentDetail", agentId: "reviewer" });
  expect(next.back).toEqual([{ kind: "agents" }]);
  expect(goBackHistory(next).current).toEqual({ kind: "agents" });
});

test("openAgentChat records the primary agent and opens New session", () => {
  useNav.setState({ history: { back: [], current: { kind: "agents" }, forward: [] }, pendingPrimaryAgentId: null });
  useNav.getState().openAgentChat("reviewer");
  expect(useNav.getState().pendingPrimaryAgentId).toBe("reviewer");
  expect(useNav.getState().history.current).toEqual({ kind: "home" });
  // the hub stays reachable via back
  expect(useNav.getState().history.back).toEqual([{ kind: "agents" }]);
});

test("choosePrimaryAgent prefers requested, then stored, default, then first executable", () => {
  const agent = (id: string, executable = true): AgentSummaryInfo => ({
    id,
    name: id,
    description: "",
    avatarColor: "violet",
    model: { kind: "route", route: "smart" },
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable,
    validation: [],
    isDefault: false,
  });
  const agents = [agent("invalid", false), agent("first"), agent("default"), agent("stored"), agent("requested")];

  expect(choosePrimaryAgent(agents, "requested", "stored", "default")).toBe("requested");
  expect(choosePrimaryAgent(agents, "missing", "stored", "default")).toBe("stored");
  expect(choosePrimaryAgent(agents, "missing", "missing", "default")).toBe("default");
  expect(choosePrimaryAgent(agents, "missing", "missing", "missing")).toBe("first");
  expect(choosePrimaryAgent([agent("invalid", false)], null, null, null)).toBeNull();
});
test("consumePendingPrimaryAgentId is a one-shot handoff", () => {
  useNav.setState({ pendingPrimaryAgentId: "reviewer" });
  expect(useNav.getState().consumePendingPrimaryAgentId()).toBe("reviewer");
  expect(useNav.getState().pendingPrimaryAgentId).toBeNull();
  expect(useNav.getState().consumePendingPrimaryAgentId()).toBeNull();
});
