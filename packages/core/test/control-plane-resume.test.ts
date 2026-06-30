import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import type { CoreEvent } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

function setup() {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo/foo", harness: "claude-code", permMode: "default" });
  const sessions = new SessionsStore(db);
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  return { cp, sessions };
}

const captured: AgentRunInput[] = [];
class CapHarness implements Agent {
  readonly id = "claude-code";
  async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
    captured.push(i);
    yield { type: "result", usage: {} };
  }
}

test("resumeSession re-runs the turn with --resume and the continuation prompt", async () => {
  captured.length = 0;
  const { cp, sessions } = setup();
  cp.harnesses.register("claude-code", () => new CapHarness());
  sessions.insert({ sessionPk: "s1", projectId: "p1", agentSessionId: "agent-9", worktreePath: "/wt", status: "running" });
  const seen: CoreEvent[] = [];
  cp.subscribe((e) => seen.push(e));

  await cp.resumeSession("s1", "restart");

  expect(captured).toHaveLength(1);
  expect(captured[0]!.resume).toBe("agent-9");
  expect(captured[0]!.prompt).toMatch(/interrupted/i);
  expect(seen.some((e) => e.kind === "status" && /resumed/i.test(e.text))).toBe(true);
  // clean completion resets the counter and idles the session
  expect(sessions.get("s1")!.resumeAttempts).toBe(0);
  expect(sessions.get("s1")!.status).toBe("idle");
});

test("resumeSession bumps and persists resumeAttempts before the run completes", async () => {
  const { cp, sessions } = setup();
  class Block implements Agent {
    readonly id = "claude-code";
    async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
      await new Promise<void>((resolve) => i.signal.addEventListener("abort", () => resolve()));
    }
  }
  cp.harnesses.register("claude-code", () => new Block());
  sessions.insert({ sessionPk: "s1", projectId: "p1", agentSessionId: "agent-9", worktreePath: "/wt", status: "running" });
  const p = cp.resumeSession("s1", "restart");
  await new Promise((r) => setTimeout(r, 10));
  expect(sessions.get("s1")!.resumeAttempts).toBe(1); // persisted before completion
  expect(sessions.get("s1")!.status).toBe("running");
  await cp.stopSession("s1"); // abort the blocked run
  await p;
  expect(sessions.get("s1")!.resumeAttempts).toBe(1); // not reset — run did not complete cleanly
});

test("resumeSession idles + notifies when there is no agentSessionId (no run)", async () => {
  captured.length = 0;
  const { cp, sessions } = setup();
  cp.harnesses.register("claude-code", () => new CapHarness());
  sessions.insert({ sessionPk: "s1", projectId: "p1", worktreePath: "/wt", status: "running" });
  const seen: CoreEvent[] = [];
  cp.subscribe((e) => seen.push(e));
  await cp.resumeSession("s1", "restart");
  expect(captured).toHaveLength(0);
  expect(sessions.get("s1")!.status).toBe("idle");
  expect(seen.some((e) => e.kind === "status" && /could not be auto-resumed/i.test(e.text))).toBe(true);
});

test("resumeSession gives up after 3 attempts (no run)", async () => {
  captured.length = 0;
  const { cp, sessions } = setup();
  cp.harnesses.register("claude-code", () => new CapHarness());
  sessions.insert({
    sessionPk: "s1",
    projectId: "p1",
    agentSessionId: "agent-9",
    worktreePath: "/wt",
    status: "running",
    resumeAttempts: 3,
  });
  const seen: CoreEvent[] = [];
  cp.subscribe((e) => seen.push(e));
  await cp.resumeSession("s1", "restart");
  expect(captured).toHaveLength(0);
  expect(sessions.get("s1")!.status).toBe("idle");
  expect(seen.some((e) => e.kind === "status" && /gave up/i.test(e.text))).toBe(true);
});
