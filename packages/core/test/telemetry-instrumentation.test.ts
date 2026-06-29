// apps/router/test/telemetry-instrumentation.test.ts
import { test, expect } from "bun:test";
import type { Telemetry, Span, Attrs } from "../src/observability/types";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

class Recording implements Telemetry {
  spans: Array<{ name: string; attrs: Attrs; error?: string; ended: boolean }> = [];
  counts: Array<{ name: string; attrs: Attrs }> = [];
  records: Array<{ name: string; value: number }> = [];
  startSpan(name: string, attrs: Attrs = {}): Span {
    const rec = { name, attrs: { ...attrs }, ended: false } as { name: string; attrs: Attrs; error?: string; ended: boolean };
    this.spans.push(rec);
    return {
      setAttribute: (k, v) => {
        rec.attrs[k] = v;
      },
      setError: (m) => {
        rec.error = m;
      },
      end: () => {
        rec.ended = true;
      },
    };
  }
  count(name: string, attrs: Attrs = {}): void {
    this.counts.push({ name, attrs });
  }
  record(name: string, value: number): void {
    this.records.push({ name, value });
  }
}

function wire(events: AgentEvent[], permMode: "default" | "bypassPermissions" = "bypassPermissions") {
  class H implements Agent {
    readonly id = "claude-code";
    async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
      for (const e of events) yield e;
    }
  }
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "f", workdir: "/repo", harness: "claude-code", permMode });
  const sessions = new SessionsStore(db);
  const tel = new Recording();
  const cp = new ControlPlane({
    projects,
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
    telemetry: tel,
    worktree: { pathFor: (r, p, s) => `${r}/${p}/${s}`, create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("claude-code", () => new H());
  return { cp, sessions, tel };
}

test("startSession opens+ends a harness.run span and counts the run", async () => {
  const { cp, tel } = wire([{ type: "result", usage: {} }]);
  await cp.startSession({ projectId: "p1", prompt: "go" });
  const span = tel.spans.find((s) => s.name === "harness.run")!;
  expect(span.ended).toBe(true);
  expect(span.attrs.project_id).toBe("p1");
  expect(tel.counts.some((c) => c.name === "session.run")).toBe(true);
});

test("an error event sets the span error + counts harness.error", async () => {
  const { cp, tel } = wire([{ type: "error", message: "boom" }]);
  await cp.startSession({ projectId: "p1", prompt: "go" });
  const span = tel.spans.find((s) => s.name === "harness.run")!;
  expect(span.error).toBe("boom");
  expect(tel.counts.some((c) => c.name === "harness.error")).toBe(true);
});

test("requestApproval counts the decision (safe tool → approval.allow)", async () => {
  const { cp, sessions, tel } = wire([], "default");
  sessions.insert({ sessionPk: "s1", projectId: "p1", status: "running" });
  await cp.requestApproval({ sessionPk: "s1", tool: "Read", input: {} });
  expect(tel.counts.some((c) => c.name === "approval.allow" && c.attrs.tool === "Read")).toBe(true);
});
