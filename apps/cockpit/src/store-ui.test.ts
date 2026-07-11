import { test, expect, beforeEach } from "bun:test";
import { openFileTab, closeTab, normalizeActive, setTabMode, useUi, type DockTab } from "./store-ui";
import type { Session } from "./bindings";

const fileTab = (path: string): DockTab => ({ id: path, kind: "file", path, title: path.split("/").pop() ?? path });

test("openFileTab appends a new tab and focuses it", () => {
  const r = openFileTab([], "/a/b.ts");
  expect(r.tabs).toHaveLength(1);
  expect(r.tabs[0]).toEqual({ id: "/a/b.ts", kind: "file", path: "/a/b.ts", title: "b.ts" });
  expect(r.activeTabId).toBe("/a/b.ts");
});

test("openFileTab dedupes by path and just refocuses", () => {
  const start = [fileTab("/a/b.ts"), fileTab("/c/d.ts")];
  const r = openFileTab(start, "/a/b.ts");
  expect(r.tabs).toHaveLength(2);
  expect(r.activeTabId).toBe("/a/b.ts");
});

test("closeTab on active focuses the right neighbor", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts"), fileTab("/c.ts")];
  const r = closeTab(start, "/b.ts", "/b.ts");
  expect(r.tabs.map((t) => t.id)).toEqual(["/a.ts", "/c.ts"]);
  expect(r.activeTabId).toBe("/c.ts");
});

test("closeTab on last active focuses the left neighbor", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts")];
  const r = closeTab(start, "/b.ts", "/b.ts");
  expect(r.activeTabId).toBe("/a.ts");
});

test("closeTab on the only tab yields null active", () => {
  const r = closeTab([fileTab("/a.ts")], "/a.ts", "/a.ts");
  expect(r.tabs).toHaveLength(0);
  expect(r.activeTabId).toBeNull();
});

test("closeTab on a non-active tab keeps the active focus", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts")];
  const r = closeTab(start, "/a.ts", "/b.ts");
  expect(r.activeTabId).toBe("/a.ts");
});

test("normalizeActive maps empty/null to null, keeps real ids", () => {
  expect(normalizeActive("")).toBeNull();
  expect(normalizeActive(null)).toBeNull();
  expect(normalizeActive("/a.ts")).toBe("/a.ts");
});

test("setTabMode sets mode on the target tab only", () => {
  const start = [fileTab("/a/readme.md"), fileTab("/c/d.ts")];
  const r = setTabMode(start, "/a/readme.md", "code");
  expect(r.find((t) => t.id === "/a/readme.md")?.mode).toBe("code");
  expect(r.find((t) => t.id === "/c/d.ts")?.mode).toBeUndefined();
});

test("setTabMode with an unknown id leaves tabs unchanged", () => {
  const start = [fileTab("/a.ts")];
  expect(setTabMode(start, "/nope.ts", "view")).toEqual(start);
});

test("tabs persisted before the mode field stay valid and can adopt a mode", () => {
  // Exact shape old clients wrote to localStorage (no mode key).
  const legacy = JSON.parse('[{"id":"/a.md","kind":"file","path":"/a.md","title":"a.md"}]') as DockTab[];
  expect(legacy[0].mode).toBeUndefined();
  expect(setTabMode(legacy, "/a.md", "view")[0].mode).toBe("view");
});

test("hideInvalidModels toggle flips state and persists to localStorage", () => {
  // happy-dom preload (bunfig.toml) gives a fresh localStorage, so the
  // store initialized with the false default at module load.
  expect(useUi.getState().hideInvalidModels).toBe(false);
  useUi.getState().toggleHideInvalidModels();
  expect(useUi.getState().hideInvalidModels).toBe(true);
  expect(localStorage.getItem("cockpit.ui.hideInvalidModels")).toBe("1");
  useUi.getState().toggleHideInvalidModels();
  expect(useUi.getState().hideInvalidModels).toBe(false);
  expect(localStorage.getItem("cockpit.ui.hideInvalidModels")).toBe("0");
});

test("notificationsEnabled defaults on and toggles + persists", () => {
  useUi.setState({ notificationsEnabled: true });
  expect(useUi.getState().notificationsEnabled).toBe(true);
  useUi.getState().toggleNotifications();
  expect(useUi.getState().notificationsEnabled).toBe(false);
  expect(localStorage.getItem("cockpit.ui.notificationsEnabled")).toBe("0");
  useUi.getState().toggleNotifications();
  expect(useUi.getState().notificationsEnabled).toBe(true);
  expect(localStorage.getItem("cockpit.ui.notificationsEnabled")).toBe("1");
});

function sess(pk: string, lastActive: number | null): Session {
  return {
    sessionPk: pk,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: pk,
    status: "idle",
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: false,
  };
}

beforeEach(() => {
  localStorage.clear();
  useUi.setState({ readAt: {} });
});

test("markRead sets and persists the cursor", () => {
  useUi.getState().markRead("s1", 1000);
  expect(useUi.getState().readAt.s1).toBe(1000);
  expect(JSON.parse(localStorage.getItem("cockpit.ui.readAt")!)).toEqual({ s1: 1000 });
});

test("seedReadState fills only absent keys, never overwriting an advanced cursor", () => {
  useUi.getState().markRead("s1", 5000); // already read at 5000
  useUi.getState().seedReadState([sess("s1", 9000), sess("s2", 200)]);
  // s1 keeps its advanced cursor; s2 seeded to its lastActive.
  expect(useUi.getState().readAt).toEqual({ s1: 5000, s2: 200 });
});

test("seedReadState treats null lastActive as 0", () => {
  useUi.getState().seedReadState([sess("s3", null)]);
  expect(useUi.getState().readAt.s3).toBe(0);
});

test("markAllRead advances every session to its lastActive", () => {
  useUi.setState({ readAt: { s1: 1 } });
  useUi.getState().markAllRead([sess("s1", 400), sess("s2", 700)]);
  expect(useUi.getState().readAt).toEqual({ s1: 400, s2: 700 });
});

test("toggleStatusFilter and toggleUnreadOnly persist", () => {
  useUi.setState({ sessionFilter: { statuses: {}, unreadOnly: false } });
  useUi.getState().toggleStatusFilter("running");
  useUi.getState().toggleUnreadOnly();
  expect(useUi.getState().sessionFilter).toEqual({ statuses: { running: true }, unreadOnly: true });
  const saved = JSON.parse(localStorage.getItem("cockpit.ui.sessionFilter")!);
  expect(saved).toEqual({ statuses: { running: true }, unreadOnly: true });
  // toggling again removes the status
  useUi.getState().toggleStatusFilter("running");
  expect(useUi.getState().sessionFilter.statuses).toEqual({});
});

test("togglePin maintains pinnedOrder (append on pin, remove on unpin) and persists", () => {
  useUi.setState({ pinned: {}, pinnedOrder: [] });
  useUi.getState().togglePin("a");
  useUi.getState().togglePin("b");
  expect(useUi.getState().pinnedOrder).toEqual(["a", "b"]);
  expect(JSON.parse(localStorage.getItem("cockpit.ui.pinnedOrder") ?? "[]")).toEqual(["a", "b"]);
  useUi.getState().togglePin("a"); // unpin a
  expect(useUi.getState().pinnedOrder).toEqual(["b"]);
  expect(useUi.getState().pinned.a).toBeUndefined();
});

test("reorderPinned reorders and persists pinnedOrder", () => {
  useUi.setState({ pinned: { a: true, b: true, c: true }, pinnedOrder: ["a", "b", "c"] });
  useUi.getState().reorderPinned("a", "c");
  expect(useUi.getState().pinnedOrder).toEqual(["b", "c", "a"]);
  expect(JSON.parse(localStorage.getItem("cockpit.ui.pinnedOrder") ?? "[]")).toEqual(["b", "c", "a"]);
});
