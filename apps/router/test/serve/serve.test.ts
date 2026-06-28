import { test, expect } from "bun:test";
import { openDb } from "../../src/store/db";
import { ProjectsStore } from "../../src/store/projects";
import { SessionsStore } from "../../src/store/sessions";
import { SettingsStore } from "../../src/config/store";
import { ControlPlane } from "../../src/core/control-plane";
import { startServeServer } from "../../src/serve/index";
import { FakeHarness } from "../helpers/fake-harness";
import type { Project } from "@harness/protocol";

function setup() {
  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const settings = new SettingsStore(db);
  // No real git worktrees in tests:
  const cp = new ControlPlane({
    projects,
    sessions,
    settings,
    workdirRoot: "/tmp",
    worktree: { pathFor: () => "/tmp/wt", create: async () => {}, remove: async () => {} },
  });
  cp.harnesses.register("fake", () => new FakeHarness());
  const project: Project = { projectId: "p1", name: "demo", workdir: "/tmp/demo", harness: "fake", permMode: "bypassPermissions" };
  projects.insert(project);
  const server = startServeServer(cp, { settings, host: "127.0.0.1", port: 0, localToken: "secret" });
  return { cp, server, project };
}

test("POST /rpc listProjects returns inserted projects", async () => {
  const { server } = setup();
  const res = await fetch(`${server.url}/rpc`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer secret" },
    body: JSON.stringify({ id: "1", method: "listProjects" }),
  });
  const data = (await res.json()) as { ok: boolean; result: Project[] };
  expect(data.ok).toBe(true);
  expect(data.result[0]?.projectId).toBe("p1");
  server.stop();
});

test("POST /rpc rejects a missing/invalid token with 401", async () => {
  const { server } = setup();
  const res = await fetch(`${server.url}/rpc`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer nope" },
    body: JSON.stringify({ id: "1", method: "listProjects" }),
  });
  expect(res.status).toBe(401);
  server.stop();
});
