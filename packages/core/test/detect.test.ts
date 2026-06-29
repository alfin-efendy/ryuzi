import { test, expect } from "bun:test";
import { defaultRunner, detectClaude, detectGit, type Runner } from "../src/agents/detect";

const fakeRun =
  (map: Record<string, { exitCode: number; stdout: string }>): Runner =>
  async (cmd) =>
    map[cmd.join(" ")] ?? { exitCode: 127, stdout: "" };

test("detectGit found", async () => {
  const run = fakeRun({ "git --version": { exitCode: 0, stdout: "git version 2.45.0" } });
  const info = await detectGit(run);
  expect(info.found).toBe(true);
  expect(info.version).toContain("2.45.0");
});

test("detectGit not found", async () => {
  const run = fakeRun({});
  expect((await detectGit(run)).found).toBe(false);
});

test("detectClaude found", async () => {
  const run = fakeRun({ "claude --version": { exitCode: 0, stdout: "2.1.89 (Claude Code)" } });
  const info = await detectClaude(run);
  expect(info.found).toBe(true);
  expect(info.version).toContain("2.1.89");
});

test("defaultRunner returns nonzero (does not throw) for a missing executable", async () => {
  const res = await defaultRunner(["definitely-not-a-real-command-xyz123", "--version"]);
  expect(res.exitCode).not.toBe(0);
  // detect functions built on it must report not-found rather than crashing
  expect((await detectClaude(defaultRunner)).found).toBeTypeOf("boolean");
});
