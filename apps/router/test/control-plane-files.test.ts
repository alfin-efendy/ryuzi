import { test, expect } from "bun:test";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ControlPlane } from "../src/core/control-plane";

function cpWithSession(worktreePath: string) {
  const session = { sessionPk: "s1", projectId: "p1", status: "idle" as const, worktreePath };
  // Minimal fake deps — only sessions.get is exercised by listDir/readFile.
  const deps: any = {
    sessions: {
      get: (pk: string) => (pk === "s1" ? session : undefined),
      list: () => [],
      insert() {},
      update() {},
      addSurface() {},
    },
    projects: { get: () => undefined, list: () => [] },
    settings: { get: () => undefined },
    workdirRoot: "/tmp",
  };
  return new ControlPlane(deps);
}

test("ControlPlane.listDir/readFile read the session worktree", async () => {
  const root = mkdtempSync(join(tmpdir(), "cp-"));
  writeFileSync(join(root, "a.txt"), "hello\n");
  const cp = cpWithSession(root);
  expect((await cp.listDir({ sessionPk: "s1", path: "" })).map((e) => e.name)).toContain("a.txt");
  expect((await cp.readFile({ sessionPk: "s1", path: "a.txt" })).content).toBe("hello\n");
  rmSync(root, { recursive: true, force: true });
});

test("ControlPlane.readFile throws when the session has no worktree", async () => {
  const cp = cpWithSession("/nonexistent-but-unused");
  await expect(cp.readFile({ sessionPk: "missing", path: "a.txt" })).rejects.toThrow();
});
