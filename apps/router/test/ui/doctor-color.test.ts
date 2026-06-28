import { test, expect } from "bun:test";
import { runCli, type CliDeps, type IO } from "../../src/cli/run";
import { detectClaude, detectGit } from "../../src/harness/detect";

const okRun = async (cmd: string[]) =>
  cmd[0] === "git" ? { exitCode: 0, stdout: "git version 2.45.0" } : { exitCode: 0, stdout: "2.1.89 (Claude Code)" };

test("doctor output stays plain & format-stable under non-TTY (CI/test)", async () => {
  const lines: string[] = [];
  const io: IO = { out: (s) => lines.push(s), err: (s) => lines.push(s), prompt: async () => "" };
  const deps: CliDeps = { io, dbPath: ":memory:", detect: { claude: () => detectClaude(okRun), git: () => detectGit(okRun) } };
  await runCli(["doctor"], deps);
  const out = lines.join("\n");
  expect(out).toContain("git:    OK 2.45.0"); // exact format preserved
  expect(out).toContain("doctor: FAIL"); // settings missing → FAIL, plain
  expect(out).not.toContain("\x1b["); // no escape codes when not a TTY
});
