// apps/router/test/approval-coordinator.test.ts
import { test, expect } from "bun:test";
import type { ApprovalDecision } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";
import { FakeGateway } from "./fake-gateway";

function wire(permMode: "default" | "acceptEdits" | "bypassPermissions" = "default") {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  projects.insert({ projectId: "p1", name: "foo", workdir: "/repo", harness: "claude-code", permMode });
  const sessions = new SessionsStore(db);
  sessions.insert({ sessionPk: "s1", projectId: "p1", status: "running" });
  sessions.addSurface("fake", "c1", "s1");
  const settings = new SettingsStore(db);
  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot: "/root" });
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  return { cp, gw, settings };
}

test("safe tool auto-allows without touching the gateway", async () => {
  const { cp, gw } = wire();
  gw.approvalHandler = async () => ({ decision: "deny", actor: "x" }); // would deny if asked
  expect(await cp.requestApproval({ sessionPk: "s1", tool: "Read", input: {} })).toBe("allow");
  expect(gw.calls.some((c) => c.startsWith("requestApproval"))).toBe(false);
});

test("risky tool asks the gateway and honors deny", async () => {
  const { cp, gw } = wire();
  gw.approvalHandler = async () => ({ decision: "deny", actor: "u1" });
  expect(await cp.requestApproval({ sessionPk: "s1", tool: "Bash", input: { command: "rm -rf /" } })).toBe("deny");
});

test("risky tool allow", async () => {
  const { cp, gw } = wire();
  gw.approvalHandler = async () => ({ decision: "allow", actor: "u1" });
  expect(await cp.requestApproval({ sessionPk: "s1", tool: "Bash", input: {} })).toBe("allow");
});

test("timeout denies", async () => {
  const { cp, gw, settings } = wire();
  settings.set("approval_timeout_ms", "30");
  gw.approvalHandler = () => new Promise<ApprovalDecision>(() => {}); // never resolves
  expect(await cp.requestApproval({ sessionPk: "s1", tool: "Bash", input: {} })).toBe("deny");
});

test("unknown session denies", async () => {
  const { cp } = wire();
  expect(await cp.requestApproval({ sessionPk: "nope", tool: "Bash", input: {} })).toBe("deny");
});

test("emits approval.requested when asking", async () => {
  const { cp, gw } = wire();
  gw.approvalHandler = async () => ({ decision: "allow", actor: "u1" });
  const kinds: string[] = [];
  cp.subscribe((e) => kinds.push(e.kind));
  await cp.requestApproval({ sessionPk: "s1", tool: "Bash", input: {} });
  expect(kinds).toContain("approval.requested");
});

test("requestApproval passes gating fields to the gateway", async () => {
  const { cp, gw, settings } = wire();
  settings.set("approver_role_ids", "r1, r2");
  // session s1 was inserted by wire(); give it a starter
  (cp as unknown as { deps: { sessions: { update(pk: string, p: object): void } } }).deps.sessions.update("s1", { startedBy: "u-starter" });
  let seen: { approverRoleIds?: string[]; startedBy?: string; timeoutMs?: number } | undefined;
  gw.approvalHandler = async (req) => {
    seen = req;
    return { decision: "allow", actor: "u1" };
  };
  await cp.requestApproval({ sessionPk: "s1", tool: "Bash", input: {} });
  expect(seen?.approverRoleIds).toEqual(["r1", "r2"]);
  expect(seen?.startedBy).toBe("u-starter");
  expect(typeof seen?.timeoutMs).toBe("number");
});
