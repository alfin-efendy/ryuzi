import { test, expect } from "bun:test";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

function setup() {
  const root = mkdtempSync(join(tmpdir(), "harness-connect-"));
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  settings.set("workdir_root", root);
  const projects = new ProjectsStore(db);
  const cp = new ControlPlane({ projects, sessions: new SessionsStore(db), settings, workdirRoot: root });
  return { cp, projects, settings, root };
}

test("connectProject by name creates a git repo + binds it", async () => {
  const { cp, projects, root } = setup();
  const p = await cp.connectProject({ gateway: "discord", workspaceId: "chan-1", actor: "u1", name: "foo" });
  expect(p.workdir).toBe(join(root, "foo"));
  expect(existsSync(join(root, "foo", ".git"))).toBe(true);
  expect(projects.resolveByWorkspace("discord", "chan-1")?.projectId).toBe(p.projectId);
});

test("connectProject rejects unsafe names", async () => {
  const { cp } = setup();
  await expect(cp.connectProject({ gateway: "discord", workspaceId: "c", name: "../evil" })).rejects.toThrow();
  await expect(cp.connectProject({ gateway: "discord", workspaceId: "c", name: ".." })).rejects.toThrow();
  await expect(cp.connectProject({ gateway: "discord", workspaceId: "c", name: ".hidden" })).rejects.toThrow();
});

test("connectProject without name or gitUrl throws", async () => {
  const { cp } = setup();
  await expect(cp.connectProject({ gateway: "discord", workspaceId: "c" })).rejects.toThrow(/name or gitUrl/i);
});

test("connectProject uses default_runtime when harness not specified", async () => {
  const { cp, settings } = setup();
  settings.set("default_runtime", "my-custom-runtime");
  const p = await cp.connectProject({ gateway: "discord", workspaceId: "chan-dr", actor: "u1", name: "bar" });
  expect(p.harness).toBe("my-custom-runtime");
});

test("connectProject falls back to claude-code when default_runtime unset", async () => {
  const { cp } = setup();
  const p = await cp.connectProject({ gateway: "discord", workspaceId: "chan-fb", actor: "u1", name: "baz" });
  expect(p.harness).toBe("claude-code");
});

test("connectProject by gitUrl clones a local repo", async () => {
  const { cp, root } = setup();
  // make a local source repo to clone from (avoids network)
  const src = mkdtempSync(join(tmpdir(), "harness-src-"));
  await Bun.$`git -C ${src} init -q`;
  await Bun.$`git -C ${src} config user.email x@x.x`;
  await Bun.$`git -C ${src} config user.name x`;
  await Bun.$`git -C ${src} commit -q --allow-empty -m init`;
  const p = await cp.connectProject({ gateway: "discord", workspaceId: "c2", gitUrl: `${src}/.git` });
  expect(existsSync(join(p.workdir, ".git"))).toBe(true);
  expect(p.source).toBe(`${src}/.git`);
});
