// apps/router/test/control-plane-run.test.ts
import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import type { CoreEvent } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

class FakeHarness implements Agent {
  readonly id = "claude-code";
  constructor(private events: AgentEvent[]) {}
  async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
    for (const e of this.events) yield e;
  }
}

function setup(events: AgentEvent[]) {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo/foo", harness: "claude-code", permMode: "default" });
  const wt: string[] = [];
  const cp = new ControlPlane({
    projects,
    sessions: new SessionsStore(db),
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: {
      pathFor: (root, pid, spk) => `${root}/.harness-worktrees/${pid}/${spk}`,
      create: async (_repo, path) => {
        wt.push("create:" + path);
      },
      remove: async () => {},
    },
  });
  cp.harnesses.register("claude-code", () => new FakeHarness(events));
  return { cp, wt };
}

test("startSession creates a worktree, persists session + agent id, emits events", async () => {
  const seen: CoreEvent[] = [];
  const { cp, wt } = setup([
    { type: "init", sessionId: "agent-1" },
    { type: "status", text: "Bash: echo hi" },
    { type: "text", text: "done" },
    { type: "result", usage: { output_tokens: 2 } },
  ]);
  cp.subscribe((e) => seen.push(e));
  const session = await cp.startSession({ projectId: "p1", prompt: "do the thing", actor: "u1" });

  expect(wt[0]).toContain("/root/.harness-worktrees/p1/");
  const stored = cp.listSessions("p1")[0]!;
  expect(stored.agentSessionId).toBe("agent-1");
  expect(stored.status).toBe("idle");
  expect(stored.worktreePath).toContain(".harness-worktrees");
  expect(seen.map((e) => e.kind)).toEqual(["session.created", "status", "text", "result"]);
  expect(session.projectId).toBe("p1");
});

test("continueSession resumes the stored agent session id", async () => {
  const captured: Array<string | undefined> = [];
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo/foo", harness: "claude-code", permMode: "default" });
  const sessions = new SessionsStore(db);
  class CapHarness implements Agent {
    readonly id = "claude-code";
    async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
      captured.push(i.resume);
      yield { type: "result", usage: {} };
    }
  }
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new CapHarness());
  sessions.insert({ sessionPk: "s1", projectId: "p1", agentSessionId: "agent-9", worktreePath: "/wt", status: "idle" });
  await cp.continueSession({ sessionPk: "s1", prompt: "more" });
  expect(captured).toEqual(["agent-9"]);
});

test("continueSession re-persists agentSessionId when result carries a rotated sessionId", async () => {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo/foo", harness: "claude-code", permMode: "default" });
  const sessions = new SessionsStore(db);
  class CapHarness implements Agent {
    readonly id = "claude-code";
    async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
      yield { type: "result", usage: {}, sessionId: "rotated" };
    }
  }
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new CapHarness());
  sessions.insert({ sessionPk: "s2", projectId: "p1", agentSessionId: "old", worktreePath: "/wt", status: "idle" });
  await cp.continueSession({ sessionPk: "s2", prompt: "continue" });
  expect(sessions.get("s2")!.agentSessionId).toBe("rotated");
});
