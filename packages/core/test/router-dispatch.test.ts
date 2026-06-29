// apps/router/test/router-dispatch.test.ts
import { test, expect } from "bun:test";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import type { AttachmentRef } from "@harness/protocol";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";
import { Router } from "../src/core/router";
import { FakeGateway } from "./fake-gateway";

class OneShot implements Agent {
  readonly id = "claude-code";
  async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
    yield { type: "text", text: "hi" };
    yield { type: "result", usage: {} };
  }
}

function wire() {
  const root = mkdtempSync(join(tmpdir(), "harness-disp-"));
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  settings.set("workdir_root", root);
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot: root });
  cp.harnesses.register("claude-code", () => new OneShot());
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  const router = new Router(cp, sessions, projects);
  return { cp, projects, sessions, gw, router };
}

test("onConnect creates workspace + project bound to it", async () => {
  const { router, projects, gw } = wire();
  const { workspaceId, project } = await router.onConnect("fake", "u1", { name: "foo" });
  expect(gw.calls).toContain("createWorkspace:foo");
  expect(projects.resolveByWorkspace("fake", workspaceId)?.projectId).toBe(project.projectId);
});

test("onStart in a connected workspace opens a conversation + runs a session", async () => {
  const { router, gw, sessions } = wire();
  const { workspaceId } = await router.onConnect("fake", "u1", { name: "bar" });
  await router.onStart("fake", workspaceId, "u1", "do the thing");
  await router.idle();
  expect(gw.calls.some((c) => c.startsWith("createConversation:"))).toBe(true);
  expect(gw.calls.some((c) => c.startsWith("postResult:"))).toBe(true);
  expect(sessions.list().length).toBe(1);
});

test("onStart in an unconnected workspace is ignored", async () => {
  const { router, gw, sessions } = wire();
  await router.onStart("fake", "no-such-ws", "u1", "hello");
  expect(sessions.list().length).toBe(0);
  expect(gw.calls.some((c) => c.startsWith("createConversation:"))).toBe(false);
});

test("onReply for an unknown conversation is ignored", async () => {
  const { router, sessions } = wire();
  await router.onReply("fake", "no-such-conv", "u1", "hello");
  expect(sessions.list().length).toBe(0);
});

test("onReply continues the session for that conversation", async () => {
  const { router, sessions } = wire();
  const { workspaceId } = await router.onConnect("fake", "u1", { name: "baz" });
  await router.onStart("fake", workspaceId, "u1", "first");
  await router.idle();
  const conv = sessions.surfaces(sessions.list()[0]!.sessionPk)[0]!.conversationId;
  await router.onReply("fake", conv, "u1", "second");
  await router.idle();
  expect(sessions.list()[0]!.status).toBe("idle"); // ran and settled
});

test("onStart forwards attachments so the manifest reaches the harness prompt", async () => {
  // local wiring with a prompt-capturing harness + fake fetch
  const { mkdtempSync } = await import("node:fs");
  const root = mkdtempSync(join(tmpdir(), "disp-att-"));
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  settings.set("workdir_root", root);
  settings.set("attachment_allowed_hosts", ""); // placeholder host "cdn" is not a real Discord host; disable host gate
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const fetchImpl = (async (u: string | URL) =>
    String(u) === "https://cdn/a" ? new Response(PNG) : new Response(null, { status: 404 })) as unknown as typeof fetch;
  const prompts: string[] = [];
  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot: root, fetchImpl });
  cp.harnesses.register("claude-code", () => ({
    id: "claude-code",
    async *run(i: AgentRunInput) {
      prompts.push(i.prompt);
      yield { type: "result", usage: {} } as AgentEvent;
    },
  }));
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  const router = new Router(cp, sessions, projects);

  const { workspaceId } = await router.onConnect("fake", "u1", { name: "att" });
  const atts: AttachmentRef[] = [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }];
  await router.onStart("fake", workspaceId, "u1", "see attached", atts);
  await router.idle();
  expect(prompts[0]).toContain("see attached");
  expect(prompts[0]).toContain("a.png");
});

test("onReply forwards attachments so the manifest reaches the harness prompt", async () => {
  const { mkdtempSync } = await import("node:fs");
  const root = mkdtempSync(join(tmpdir(), "disp-att2-"));
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  settings.set("workdir_root", root);
  settings.set("attachment_allowed_hosts", ""); // placeholder host "cdn" is not a real Discord host; disable host gate
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const fetchImpl = (async (u: string | URL) =>
    String(u) === "https://cdn/a" ? new Response(PNG) : new Response(null, { status: 404 })) as unknown as typeof fetch;
  const prompts: string[] = [];
  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot: root, fetchImpl });
  cp.harnesses.register("claude-code", () => ({
    id: "claude-code",
    async *run(i: AgentRunInput) {
      prompts.push(i.prompt);
      yield { type: "result", usage: {} } as AgentEvent;
    },
  }));
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  const router = new Router(cp, sessions, projects);
  const { workspaceId } = await router.onConnect("fake", "u1", { name: "att2" });
  await router.onStart("fake", workspaceId, "u1", "first");
  await router.idle();
  const conv = sessions.surfaces(sessions.list()[0]!.sessionPk)[0]!.conversationId;
  const atts: AttachmentRef[] = [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }];
  await router.onReply("fake", conv, "u1", "see reply", atts);
  await router.idle();
  expect(prompts[1]).toContain("see reply");
  expect(prompts[1]).toContain("a.png");
});
