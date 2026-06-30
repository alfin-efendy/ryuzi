import { test, expect } from "bun:test";
import { buildClaudeArgs } from "../src/agents/claude-code/args";
import type { AgentRunInput } from "../src/agents/types";

function input(over: Partial<AgentRunInput> = {}): AgentRunInput {
  return {
    workdir: "/wt",
    prompt: "do it",
    permissionMode: "default",
    signal: new AbortController().signal,
    approve: async () => ({ behavior: "allow" }),
    ...over,
  };
}

test("new session uses --session-id and always sets stream-json + verbose", () => {
  const a = buildClaudeArgs(input(), "uuid-1");
  expect(a).toContain("--session-id");
  expect(a[a.indexOf("--session-id") + 1]).toBe("uuid-1");
  expect(a).not.toContain("--resume");
  expect(a).toEqual(expect.arrayContaining(["-p", "do it", "--output-format", "stream-json", "--verbose", "--permission-mode", "default"]));
});

test("resume uses --resume and not --session-id", () => {
  const a = buildClaudeArgs(input({ resume: "prev-id" }), "uuid-2");
  expect(a).toContain("--resume");
  expect(a[a.indexOf("--resume") + 1]).toBe("prev-id");
  expect(a).not.toContain("--session-id");
});

test("model and effort included only when set", () => {
  expect(buildClaudeArgs(input(), "u")).not.toContain("--model");
  const a = buildClaudeArgs(input({ model: "haiku", effort: "low" }), "u");
  expect(a[a.indexOf("--model") + 1]).toBe("haiku");
  expect(a[a.indexOf("--effort") + 1]).toBe("low");
});

test("includes --append-system-prompt only when systemPromptAppend is set", () => {
  expect(buildClaudeArgs(input(), "u")).not.toContain("--append-system-prompt");
  const a = buildClaudeArgs(input({ systemPromptAppend: "rename your branch" }), "u");
  expect(a[a.indexOf("--append-system-prompt") + 1]).toBe("rename your branch");
});
