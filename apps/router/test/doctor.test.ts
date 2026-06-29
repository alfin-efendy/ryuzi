import { test, expect } from "bun:test";
import { runCli, type CliDeps, type IO } from "../src/cli/run";
import type { Runner } from "@harness/core";
import { detectClaude, detectGit } from "@harness/core";

function depsWith(run: Runner, dbPath = ":memory:") {
  const lines: string[] = [];
  const io: IO = { out: (s) => lines.push(s), err: (s) => lines.push(s), prompt: async () => "" };
  const deps: CliDeps = {
    io,
    dbPath,
    detect: { claude: (r = run) => detectClaude(r), git: (r = run) => detectGit(r) },
  };
  return { lines, deps };
}

const okRun: Runner = async (cmd) =>
  cmd[0] === "git" ? { exitCode: 0, stdout: "git version 2.45.0" } : { exitCode: 0, stdout: "2.1.89 (Claude Code)" };

const noClaude: Runner = async (cmd) => (cmd[0] === "git" ? { exitCode: 0, stdout: "git version 2.45.0" } : { exitCode: 127, stdout: "" });

test("doctor fails when required settings missing even if tools present", async () => {
  const { deps, lines } = depsWith(okRun);
  expect(await runCli(["doctor"], deps)).toBe(1);
  expect(lines.join("\n")).toMatch(/missing/i);
});

test("doctor fails when claude not found", async () => {
  const { deps } = depsWith(noClaude);
  expect(await runCli(["doctor"], deps)).toBe(1);
});

test("doctor passes when tools present and required settings set", async () => {
  const dbPath = `/tmp/harness-doctor-${Bun.hash(Math.random().toString())}.sqlite`;
  const seed = depsWith(okRun, dbPath);
  for (const [k, v] of [
    ["discord.token", "t"],
    ["discord.app_id", "a"],
    ["discord.guild_id", "g"],
    ["workdir_root", "/repos"],
  ] as const) {
    await runCli(["config", "set", k, v], seed.deps);
  }
  const { deps } = depsWith(okRun, dbPath);
  expect(await runCli(["doctor"], deps)).toBe(0);
});
