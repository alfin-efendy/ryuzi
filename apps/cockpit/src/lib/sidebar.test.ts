import { describe, expect, test } from "bun:test";
import type { Project, Session } from "../bindings";
import { archivedCount, chatSessions, orderProjects, sessionTitle, sessionsForProject } from "./sidebar";

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
    kind: "project",
    speaker: null,
    agent: null,
    parentSessionPk: null,
  };
}

const sessions = [
  sess("a", "p1", "Fix auth", 10),
  sess("b", "p1", "Dark mode", 30),
  sess("c", "p2", "Other", 20),
  sess("d", "p1", null, 40),
];

describe("sidebar sessions", () => {
  test("filters by project and sorts newest first", () => {
    const out = sessionsForProject(sessions, "p1", "", false, {}, {});
    expect(out.map((s) => s.sessionPk)).toEqual(["d", "b", "a"]);
  });

  test("pinned sessions sort first", () => {
    const out = sessionsForProject(sessions, "p1", "", false, { a: true }, {});
    expect(out[0].sessionPk).toBe("a");
  });

  test("query matches the title case-insensitively", () => {
    const out = sessionsForProject(sessions, "p1", "dark", false, {}, {});
    expect(out.map((s) => s.sessionPk)).toEqual(["b"]);
  });

  test("archived sessions hide unless revealed", () => {
    expect(sessionsForProject(sessions, "p1", "", false, {}, { b: true }).map((s) => s.sessionPk)).toEqual(["d", "a"]);
    expect(sessionsForProject(sessions, "p1", "", true, {}, { b: true }).map((s) => s.sessionPk)).toEqual(["d", "b", "a"]);
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

  test("chat sessions bucket separately from project sessions", () => {
    const sessions = [
      { sessionPk: "c1", projectId: null, kind: "chat" },
      { sessionPk: "p1", projectId: "proj", kind: "project" },
    ] as any;
    expect(chatSessions(sessions).map((s) => s.sessionPk)).toEqual(["c1"]);
    expect(sessionsForProject(sessions, "proj", "", false, {}, {}).map((s) => s.sessionPk)).toEqual(["p1"]);
  });
});
