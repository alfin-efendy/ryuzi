import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { existsSync } from "node:fs";
import { worktreePathFor, createWorktree, removeWorktree, resolveFreshBase } from "../src/agents/worktree";

const tmpDirs: string[] = [];
function mkTmp(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  tmpDirs.push(dir);
  return dir;
}
afterAll(() => {
  for (const d of tmpDirs) rmSync(d, { recursive: true, force: true });
});

async function tempRepo(): Promise<string> {
  const dir = mkTmp("harness-wt-");
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

// A bare "remote" on `main` plus a fresh clone (clone sets origin + origin/HEAD).
async function tempClone(): Promise<{ remote: string; clone: string }> {
  const remote = mkTmp("harness-remote-");
  await Bun.$`git -C ${remote} init -q --bare -b main`.quiet();
  const seed = mkTmp("harness-seed-");
  await Bun.$`git -C ${seed} init -q -b main`.quiet();
  await Bun.$`git -C ${seed} config user.email x@x.x`.quiet();
  await Bun.$`git -C ${seed} config user.name x`.quiet();
  await Bun.$`git -C ${seed} commit -q --allow-empty -m init`.quiet();
  await Bun.$`git -C ${seed} remote add origin ${remote}`.quiet();
  await Bun.$`git -C ${seed} push -q origin main`.quiet();
  const clone = mkTmp("harness-clone-");
  await Bun.$`git clone -q ${remote} ${clone}`.quiet();
  return { remote, clone };
}

async function pushEmptyCommit(remote: string, msg: string): Promise<string> {
  const work = mkTmp("harness-push-");
  await Bun.$`git clone -q ${remote} ${work}`.quiet();
  await Bun.$`git -C ${work} config user.email x@x.x`.quiet();
  await Bun.$`git -C ${work} config user.name x`.quiet();
  await Bun.$`git -C ${work} commit -q --allow-empty -m ${msg}`.quiet();
  await Bun.$`git -C ${work} push -q origin main`.quiet();
  return (await Bun.$`git -C ${work} rev-parse HEAD`.text()).trim();
}

test("resolveFreshBase returns origin/<default> for a repo with a remote", async () => {
  const { clone } = await tempClone();
  expect(await resolveFreshBase(clone)).toBe("origin/main");
});

test("resolveFreshBase returns undefined when there is no origin remote", async () => {
  const repo = await tempRepo();
  expect(await resolveFreshBase(repo)).toBeUndefined();
});

test("resolveFreshBase fetches: origin/main advances to a newly pushed commit", async () => {
  const { remote, clone } = await tempClone();
  const tip = await pushEmptyCommit(remote, "second");
  await resolveFreshBase(clone);
  const localOriginMain = (await Bun.$`git -C ${clone} rev-parse origin/main`.text()).trim();
  expect(localOriginMain).toBe(tip);
});

test("createWorktree with a baseRef branches off that ref", async () => {
  const { remote, clone } = await tempClone();
  const tip = await pushEmptyCommit(remote, "newer");
  await Bun.$`git -C ${clone} fetch -q origin`.quiet();
  const wt = join(clone, "wt");
  await createWorktree(clone, wt, "harness/with-base", "origin/main");
  const wtHead = (await Bun.$`git -C ${wt} rev-parse HEAD`.text()).trim();
  expect(wtHead).toBe(tip);
  await removeWorktree(clone, wt);
});
