import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { existsSync } from "node:fs";
import { worktreePathFor, createWorktree, removeWorktree } from "../src/agents/worktree";

async function tempRepo(): Promise<string> {
  const dir = mkdtempSync(join(tmpdir(), "harness-wt-"));
  await Bun.$`git -C ${dir} init -q`;
  await Bun.$`git -C ${dir} config user.email x@x.x`;
  await Bun.$`git -C ${dir} config user.name x`;
  await Bun.$`git -C ${dir} commit -q --allow-empty -m init`;
  return dir;
}

test("worktreePathFor composes the expected path", () => {
  expect(worktreePathFor("/root", "p1", "s1")).toBe("/root/.harness-worktrees/p1/s1");
});

test("create then remove a worktree on a real repo", async () => {
  const repo = await tempRepo();
  const wt = join(repo, "wt");
  await createWorktree(repo, wt, "harness/test");
  expect(existsSync(wt)).toBe(true);
  const branches = await Bun.$`git -C ${repo} branch --list harness/test`.text();
  expect(branches).toContain("harness/test");
  await removeWorktree(repo, wt);
  expect(existsSync(wt)).toBe(false);
});
