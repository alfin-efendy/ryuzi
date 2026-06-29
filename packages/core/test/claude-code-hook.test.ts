// apps/router/test/claude-code-hook.test.ts
import { test, expect } from "bun:test";
import { buildClaudeArgs } from "../src/agents/claude-code/args";
import { ClaudeCodeHarness, type ClaudeRunner } from "../src/agents/claude-code/index";
import type { AgentRunInput } from "../src/agents/types";

function input(over: Partial<AgentRunInput> = {}): AgentRunInput {
  return {
    workdir: "/wt",
    prompt: "go",
    permissionMode: "default",
    signal: new AbortController().signal,
    approve: async () => ({ behavior: "allow" }),
    ...over,
  };
}

test("default mode + approval adds --settings with the hook", () => {
  const a = buildClaudeArgs(input({ approval: { url: "http://x", sessionPk: "s1", hookBinPath: "/h.ts" } }), "u");
  const i = a.indexOf("--settings");
  expect(i).toBeGreaterThan(-1);
  expect(a[i + 1]).toContain("PreToolUse");
  expect(a[i + 1]).toContain("bun /h.ts");
});

test("bypass mode does not add --settings", () => {
  const a = buildClaudeArgs(
    input({ permissionMode: "bypassPermissions", approval: { url: "http://x", sessionPk: "s1", hookBinPath: "/h.ts" } }),
    "u",
  );
  expect(a).not.toContain("--settings");
});

test("acceptEdits mode does not add --settings", () => {
  const a = buildClaudeArgs(
    input({ permissionMode: "acceptEdits", approval: { url: "http://x", sessionPk: "s1", hookBinPath: "/h.ts" } }),
    "u",
  );
  expect(a).not.toContain("--settings");
});

test("runner receives HARNESS_* env when approval is set", async () => {
  let capturedEnv: Record<string, string> | undefined;
  const runner: ClaudeRunner = (_args, opts) => {
    capturedEnv = (opts as { env?: Record<string, string> }).env;
    return (async function* () {
      yield JSON.stringify({ type: "result", is_error: false, result: "ok", session_id: "x", usage: {} });
    })();
  };
  const h = new ClaudeCodeHarness(runner);
  const it = h.run(input({ approval: { url: "http://ipc", sessionPk: "s9", hookBinPath: "/h.ts" } }));
  for await (const _e of it) {
    /* drain */
  }
  expect(capturedEnv?.HARNESS_APPROVAL_URL).toBe("http://ipc");
  expect(capturedEnv?.HARNESS_SESSION_PK).toBe("s9");
});
