import { test, expect } from "bun:test";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";
import { Router } from "../src/core/router";
import { FakeGateway } from "./fake-gateway";

test("notice event posts standalone message via postResult and does not cache a status ref", async () => {
  const db = openDb(":memory:");
  const sessions = new SessionsStore(db);
  const cp = new ControlPlane({
    projects: new ProjectsStore(db),
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
  });
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  const router = new Router(cp, sessions, new ProjectsStore(db));

  sessions.insert({ sessionPk: "s1", projectId: "p1", status: "running" });
  sessions.addSurface("fake", "c1", "s1");
  sessions.addSurface("fake", "c2", "s1");

  cp.emit({ kind: "notice", sessionPk: "s1", text: "update available" });
  await router.idle();

  // notice is delivered to all surfaces as a standalone postResult
  expect(gw.calls).toContain("postResult:c1:update available");
  expect(gw.calls).toContain("postResult:c2:update available");
  // no status was posted or edited — the st.status cache was not touched
  expect(gw.calls.some((c) => c.startsWith("postStatus:"))).toBe(false);
  expect(gw.calls.some((c) => c.startsWith("editStatus:"))).toBe(false);
});

test("renders status (post then edit), buffers text, posts result to all surfaces", async () => {
  const db = openDb(":memory:");
  const sessions = new SessionsStore(db);
  const cp = new ControlPlane({
    projects: new ProjectsStore(db),
    sessions,
    settings: new SettingsStore(db),
    workdirRoot: "/root",
  });
  const gw = new FakeGateway();
  cp.gateways.register(gw);
  const router = new Router(cp, sessions, new ProjectsStore(db));

  // a session with two surfaces (fan-out)
  sessions.insert({ sessionPk: "s1", projectId: "p1", status: "running" });
  sessions.addSurface("fake", "c1", "s1");
  sessions.addSurface("fake", "c2", "s1");

  cp.emit({ kind: "status", sessionPk: "s1", text: "step one" });
  cp.emit({ kind: "status", sessionPk: "s1", text: "step two" });
  cp.emit({ kind: "text", sessionPk: "s1", text: "the answer" });
  cp.emit({ kind: "result", sessionPk: "s1", usage: {} });
  await router.idle();

  // first status posts, second edits — for each surface
  expect(gw.calls).toContain("postStatus:c1:step one");
  expect(gw.calls).toContain("editStatus:m-1:step two");
  expect(gw.calls).toContain("postStatus:c2:step one");
  expect(gw.calls).toContain("editStatus:m-2:step two");
  // result carries the buffered text, to both surfaces
  expect(gw.calls).toContain("postResult:c1:the answer");
  expect(gw.calls).toContain("postResult:c2:the answer");
});
