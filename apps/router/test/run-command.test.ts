// apps/router/test/run-command.test.ts
import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runCli, type CliDeps, type IO } from "../src/cli/run";
import { detectClaude, detectGit } from "@harness/core";
import type { Agent, AgentEvent, AgentRunInput } from "@harness/core";

async function tempRepo(): Promise<string> {
  const dir = mkdtempSync(join(tmpdir(), "harness-run-"));
  await Bun.$`git -C ${dir} init -q`;
  await Bun.$`git -C ${dir} config user.email x@x.x`;
  await Bun.$`git -C ${dir} config user.name x`;
  await Bun.$`git -C ${dir} commit -q --allow-empty -m init`;
  return dir;
}

class FakeHarness implements Agent {
  readonly id = "claude-code";
  async *run(_i: AgentRunInput): AsyncIterable<AgentEvent> {
    yield { type: "init", sessionId: "agent-x" };
    yield { type: "text", text: "all done" };
    yield { type: "result", usage: {} };
  }
}

test("harness run drives a session and prints streamed events", async () => {
  const repo = await tempRepo();
  const lines: string[] = [];
  const io: IO = { out: (s) => lines.push(s), err: (s) => lines.push("ERR " + s), prompt: async () => "" };
  const deps: CliDeps = {
    io,
    dbPath: ":memory:",
    detect: { claude: detectClaude, git: detectGit },
    harnessFactory: () => new FakeHarness(),
  };
  const code = await runCli(["run", "--dir", repo, "--prompt", "do it"], deps);
  expect(code).toBe(0);
  const text = lines.join("\n");
  expect(text).toContain("all done");
});

test("harness run requires --dir and --prompt", async () => {
  const io: IO = { out: () => {}, err: () => {}, prompt: async () => "" };
  const deps: CliDeps = { io, dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit } };
  expect(await runCli(["run", "--prompt", "x"], deps)).toBe(1);
});

test("harness run rejects an invalid --mode", async () => {
  const io: IO = { out: () => {}, err: () => {}, prompt: async () => "" };
  const deps: CliDeps = { io, dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit } };
  expect(await runCli(["run", "--dir", "/tmp/whatever", "--prompt", "x", "--mode", "bogus"], deps)).toBe(1);
});
