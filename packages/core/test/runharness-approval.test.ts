// packages/core/test/runharness-approval.test.ts
import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

function wire(permMode: "default" | "bypassPermissions") {
  const captured: Array<AgentRunInput["approval"]> = [];
  class Cap implements Agent {
    readonly id = "claude-code";
    async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
      captured.push(i.approval);
      yield { type: "result", usage: {} };
    }
  }
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "f", workdir: "/repo", harness: "claude-code", permMode });
  const sessions = new SessionsStore(db);
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new Cap());
  cp.approvalUrl = "http://127.0.0.1:1234";
  cp.hookBinPath = "/abs/hook.ts";
  return { cp, captured };
}

test("default mode threads approval into the harness run", async () => {
  const { cp, captured } = wire("default");
  await cp.startSession({ projectId: "p1", prompt: "go", actor: "u1" });
  expect(captured[0]).toEqual({ url: "http://127.0.0.1:1234", sessionPk: expect.any(String), hookBinPath: "/abs/hook.ts" });
});

test("bypass mode does not thread approval", async () => {
  const { cp, captured } = wire("bypassPermissions");
  await cp.startSession({ projectId: "p1", prompt: "go", actor: "u1" });
  expect(captured[0]).toBeUndefined();
});
