// apps/router/test/lifecycle.test.ts
import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import type { CoreEvent } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

function base(worktree?: any) {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo/foo", harness: "claude-code", permMode: "default" });
  const removed: string[] = [];
  const cp = new ControlPlane({
    projects,
    sessions: new SessionsStore(db),
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: worktree ?? {
      pathFor: (r: string, p: string, s: string) => `${r}/${p}/${s}`,
      create: async () => {},
      remove: async (_repo: string, path: string) => {
        removed.push(path);
      },
    },
  });
  return { cp, removed, db };
}

test("startSession rolls back the worktree if session insert fails", async () => {
  let created = "";
  const { cp, removed } = base({
    pathFor: (r: string, p: string, s: string) => `${r}/${p}/${s}`,
    create: async (_repo: string, path: string) => {
      created = path;
    },
    remove: async (_repo: string, path: string) => {
      removed.push(path);
    },
  });
  // force the insert to throw by inserting a duplicate sessionPk is hard; instead stub sessions.insert
  (cp as any).deps.sessions.insert = () => {
    throw new Error("insert boom");
  };
  await expect(cp.startSession({ projectId: "p1", prompt: "x" })).rejects.toThrow("insert boom");
  expect(removed).toEqual([created]); // worktree cleaned up
});

test("endSession aborts, removes worktree, marks ended, emits", async () => {
  class Never implements Agent {
    readonly id = "claude-code";
    async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
      // block until aborted
      await new Promise<void>((resolve) => i.signal.addEventListener("abort", () => resolve()));
    }
  }
  const { cp, removed } = base();
  cp.harnesses.register("claude-code", () => new Never());
  const events: CoreEvent[] = [];
  cp.subscribe((e) => events.push(e));
  const startP = cp.startSession({ projectId: "p1", prompt: "go" });
  // let startSession create the session + begin running
  await new Promise((r) => setTimeout(r, 10));
  const pk = cp.listSessions("p1")[0]!.sessionPk;
  await cp.endSession(pk);
  await startP; // run loop ended via abort
  expect(removed.length).toBe(1);
  expect(cp.listSessions("p1")[0]!.status).toBe("ended");
  expect(events.some((e) => e.kind === "session.ended")).toBe(true);
});
