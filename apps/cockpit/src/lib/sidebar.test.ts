import { describe, expect, test } from "bun:test";
import type { Project, Session, SessionStatus } from "../bindings";
import { archivedCount, isUnreadVisible, orderProjects, reorder, sessionTitle, sessionsForProject, type SessionFilterCtx } from "./sidebar";

function sess(pk: string, projectId: string, title: string | null, lastActive = 0): Session {
  return {
    sessionPk: pk,
    projectId,
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title,
    status: "idle",
    startedBy: null,
    createdAt: null,
    lastActive,
    resumeAttempts: 0,
    branchOwned: true,
  };
}

const sessions = [
  sess("a", "p1", "Fix auth", 10),
  sess("b", "p1", "Dark mode", 30),
  sess("c", "p2", "Other", 20),
  sess("d", "p1", null, 40),
];

const noFilter: SessionFilterCtx = { statuses: {}, unreadOnly: false, readAt: {}, focusedSessionPk: null };

describe("sidebar sessions", () => {
  test("filters by project and sorts newest first", () => {
    const out = sessionsForProject(sessions, "p1", "", false, {}, {}, noFilter);
    expect(out.map((s) => s.sessionPk)).toEqual(["d", "b", "a"]);
  });

  test("pinned sessions sort first", () => {
    const out = sessionsForProject(sessions, "p1", "", false, { a: true }, {}, noFilter);
    expect(out[0].sessionPk).toBe("a");
  });

  test("query matches the title case-insensitively", () => {
    const out = sessionsForProject(sessions, "p1", "dark", false, {}, {}, noFilter);
    expect(out.map((s) => s.sessionPk)).toEqual(["b"]);
  });

  test("archived sessions hide unless revealed", () => {
    expect(sessionsForProject(sessions, "p1", "", false, {}, { b: true }, noFilter).map((s) => s.sessionPk)).toEqual(["d", "a"]);
    expect(sessionsForProject(sessions, "p1", "", true, {}, { b: true }, noFilter).map((s) => s.sessionPk)).toEqual(["d", "b", "a"]);
    expect(archivedCount(sessions, "p1", { b: true, c: true })).toBe(1);
  });

  test("untitled sessions get a fallback title", () => {
    expect(sessionTitle(sess("x", "p1", null))).toBe("Untitled session");
    expect(sessionTitle(sess("x", "p1", "  "))).toBe("Untitled session");
  });

  test("orderProjects sorts by name only when asked", () => {
    const projects = [{ projectId: "z", name: "zeta" } as Project, { projectId: "a", name: "alpha" } as Project];
    expect(orderProjects(projects, "updated").map((p) => p.projectId)).toEqual(["z", "a"]);
    expect(orderProjects(projects, "name").map((p) => p.projectId)).toEqual(["a", "z"]);
  });
});

// Distinct from the `sess` helper above (different field defaults); named to
// avoid colliding with it while matching the read-state brief fixtures.
function unreadSess(pk: string, lastActive: number | null): Session {
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

describe("isUnreadVisible", () => {
  test("unread when lastActive is newer than the cursor", () => {
    expect(isUnreadVisible(unreadSess("s1", 500), { s1: 100 }, null)).toBe(true);
  });
  test("read when lastActive is at or before the cursor", () => {
    expect(isUnreadVisible(unreadSess("s1", 100), { s1: 100 }, null)).toBe(false);
    expect(isUnreadVisible(unreadSess("s1", 50), { s1: 100 }, null)).toBe(false);
  });
  test("absent cursor is not unread", () => {
    expect(isUnreadVisible(unreadSess("s1", 500), {}, null)).toBe(false);
  });
  test("null lastActive is not unread", () => {
    expect(isUnreadVisible(unreadSess("s1", null), { s1: 0 }, null)).toBe(false);
  });
  test("the focused session is never unread", () => {
    expect(isUnreadVisible(unreadSess("s1", 500), { s1: 100 }, "s1")).toBe(false);
  });
});

// Built on `unreadSess` (not the 4-arg `sess`) so the status-carrying fixture
// shares the same project id ("p") the filter-composition tests rely on.
function sessS(pk: string, status: SessionStatus, lastActive: number | null): Session {
  return { ...unreadSess(pk, lastActive), status };
}

describe("sessionsForProject filter", () => {
  test("empty status filter passes all statuses", () => {
    const rows = sessionsForProject([sessS("a", "idle", 2), sessS("b", "running", 1)], "p", "", false, {}, {}, noFilter);
    expect(rows.map((r) => r.sessionPk)).toEqual(["a", "b"]);
  });

  test("status filter keeps only matching statuses", () => {
    const rows = sessionsForProject(
      [sessS("a", "idle", 2), sessS("b", "running", 1)],
      "p",
      "",
      false,
      {},
      {},
      { ...noFilter, statuses: { running: true } },
    );
    expect(rows.map((r) => r.sessionPk)).toEqual(["b"]);
  });

  test("unreadOnly gates on isUnreadVisible", () => {
    // a: lastActive 500 > cursor 100 → unread; b: lastActive 50 <= 100 → read
    const rows = sessionsForProject(
      [sessS("a", "idle", 500), sessS("b", "idle", 50)],
      "p",
      "",
      false,
      {},
      {},
      { ...noFilter, unreadOnly: true, readAt: { a: 100, b: 100 } },
    );
    expect(rows.map((r) => r.sessionPk)).toEqual(["a"]);
  });

  test("filter composes with query and preserves pin/recency ordering", () => {
    const rows = sessionsForProject(
      [sessS("a", "running", 1), sessS("b", "running", 9)],
      "p",
      "",
      false,
      { a: true },
      {},
      { ...noFilter, statuses: { running: true } },
    );
    // both running; a is pinned so it sorts first despite older lastActive
    expect(rows.map((r) => r.sessionPk)).toEqual(["a", "b"]);
  });
});

test("reorder moves fromId to toId's slot (forward and backward), immutably", () => {
  const a = ["1", "2", "3", "4"];
  expect(reorder(a, "1", "3")).toEqual(["2", "3", "1", "4"]);
  expect(reorder(a, "4", "2")).toEqual(["1", "4", "2", "3"]);
  expect(a).toEqual(["1", "2", "3", "4"]); // original untouched
});

test("reorder no-ops on missing id or equal ids", () => {
  expect(reorder(["1", "2"], "x", "1")).toEqual(["1", "2"]);
  expect(reorder(["1", "2"], "1", "x")).toEqual(["1", "2"]);
  expect(reorder(["1", "2"], "1", "1")).toEqual(["1", "2"]);
});

test("sessionsForProject orders pinned by pinnedOrder index; unordered pinned fall after by recency", () => {
  const mk = (pk: string, lastActive: number) => ({
    sessionPk: pk,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: pk,
    status: "idle" as const,
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: false,
  });
  const sessions = [mk("a", 100), mk("b", 200), mk("c", 300)];
  const noFilter = { statuses: {}, unreadOnly: false, readAt: {}, focusedSessionPk: null };
  // a,b,c all pinned; pinnedOrder = [c, a] → c, a first (by order), then b (unordered → recency)
  const out = sessionsForProject(sessions, "p", "", false, { a: true, b: true, c: true }, {}, noFilter, ["c", "a"]);
  expect(out.map((s) => s.sessionPk)).toEqual(["c", "a", "b"]);
});

test("sessionsForProject with empty pinnedOrder keeps legacy pinned-first-then-recency", () => {
  const mk = (pk: string, lastActive: number) => ({
    sessionPk: pk,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: pk,
    status: "idle" as const,
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: false,
  });
  const sessions = [mk("a", 100), mk("b", 300)];
  const noFilter = { statuses: {}, unreadOnly: false, readAt: {}, focusedSessionPk: null };
  // a pinned, b not → a first despite older lastActive (no pinnedOrder arg → default [])
  expect(sessionsForProject(sessions, "p", "", false, { a: true }, {}, noFilter).map((s) => s.sessionPk)).toEqual(["a", "b"]);
});
