import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

test("queued continueSession resumes with the first run's agent session id", async () => {
  const resumes: (string | undefined)[] = [];
  class H implements Agent {
    readonly id = "claude-code";
    async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
      resumes.push(i.resume);
      if (!i.resume) yield { type: "init", sessionId: "agent-A" };
      await new Promise((r) => setTimeout(r, 10));
      yield { type: "result", usage: {} };
    }
  }
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo", harness: "claude-code", permMode: "default" });
  const sessions = new SessionsStore(db);
  sessions.insert({ sessionPk: "s1", projectId: "p1", worktreePath: "/wt", status: "idle" });
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new H());
  await Promise.all([cp.continueSession({ sessionPk: "s1", prompt: "a" }), cp.continueSession({ sessionPk: "s1", prompt: "b" })]);
  expect(resumes[0]).toBeUndefined();
  expect(resumes[1]).toBe("agent-A");
});

test("two continueSession calls on the same session run serially", async () => {
  const order: string[] = [];
  let active = 0;
  class SlowHarness implements Agent {
    readonly id = "claude-code";
    async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
      active++;
      order.push(`start(active=${active})`);
      await new Promise((r) => setTimeout(r, 20));
      active--;
      order.push("end");
      yield { type: "result", usage: {} };
    }
  }
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo", harness: "claude-code", permMode: "default" });
  const sessions = new SessionsStore(db);
  sessions.insert({ sessionPk: "s1", projectId: "p1", worktreePath: "/wt", status: "idle" });
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new SlowHarness());

  await Promise.all([cp.continueSession({ sessionPk: "s1", prompt: "a" }), cp.continueSession({ sessionPk: "s1", prompt: "b" })]);
  // never two active at once → no "start(active=2)"
  expect(order).toEqual(["start(active=1)", "end", "start(active=1)", "end"]);
});
