import { test, expect } from "bun:test";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";

class CaptureHarness implements Agent {
  readonly id = "claude-code";
  prompts: string[] = [];
  async *run(i: AgentRunInput): AsyncIterable<AgentEvent> {
    this.prompts.push(i.prompt);
    yield { type: "result", usage: {} };
  }
}

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const fetchImpl = (async (url: string | URL) =>
  String(url) === "https://cdn/a" ? new Response(PNG) : new Response(null, { status: 404 })) as unknown as typeof fetch;

async function wire() {
  const root = mkdtempSync(join(tmpdir(), "cp-att-"));
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  settings.set("workdir_root", root);
  settings.set("attachment_allowed_hosts", ""); // placeholder host "cdn" is not a real Discord host; disable host gate
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const harness = new CaptureHarness();
  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot: root, fetchImpl });
  cp.harnesses.register("claude-code", () => harness);
  const project = await cp.connectProject({ gateway: "fake", workspaceId: "ws", actor: "u", name: "proj" });
  return { cp, sessions, harness, project, root, settings };
}

test("startSession with an attachment appends a manifest path to the prompt", async () => {
  const { cp, harness, project } = await wire();
  await cp.startSession({
    projectId: project.projectId,
    prompt: "look at this",
    actor: "u",
    attachments: [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }],
  });
  expect(harness.prompts[0]).toContain("look at this");
  expect(harness.prompts[0]).toContain(".harness-attachments");
  expect(harness.prompts[0]).toContain("a.png");
});

test("attachment dir lives outside the worktree and is removed on endSession", async () => {
  const { cp, sessions, root, project } = await wire();
  const session = await cp.startSession({
    projectId: project.projectId,
    prompt: "hi",
    actor: "u",
    attachments: [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }],
  });
  const dir = join(root, ".harness-attachments", session.sessionPk);
  expect(existsSync(dir)).toBe(true);
  expect(session.worktreePath?.includes(".harness-attachments")).toBeFalsy();
  await cp.endSession(session.sessionPk);
  expect(existsSync(dir)).toBe(false);
});

test("no attachments leaves the prompt unchanged", async () => {
  const { cp, harness, project } = await wire();
  await cp.startSession({ projectId: project.projectId, prompt: "plain", actor: "u" });
  expect(harness.prompts[0]).toBe("plain");
});

test("maxCount=0 disables attachments; prompt passes through unchanged", async () => {
  const { cp, harness, project, settings } = await wire();
  settings.set("attachment_max_count", "0");
  await cp.startSession({
    projectId: project.projectId,
    prompt: "hello",
    actor: "u",
    attachments: [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }],
  });
  expect(harness.prompts[0]).toBe("hello");
});

test("continueSession with an attachment injects the manifest", async () => {
  const { cp, harness, project } = await wire();
  const session = await cp.startSession({ projectId: project.projectId, prompt: "first", actor: "u" });
  await cp.continueSession({
    sessionPk: session.sessionPk,
    prompt: "and this",
    actor: "u",
    attachments: [{ name: "a.png", url: "https://cdn/a", contentType: "image/png", size: PNG.byteLength }],
  });
  expect(harness.prompts[1]).toContain("and this");
  expect(harness.prompts[1]).toContain("a.png");
});
