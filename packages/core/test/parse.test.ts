import { test, expect } from "bun:test";
import { parseLine } from "../src/agents/claude-code/parse";

test("init event yields init with session id", () => {
  const line = JSON.stringify({ type: "system", subtype: "init", session_id: "abc" });
  expect(parseLine(line)).toEqual([{ type: "init", sessionId: "abc" }]);
});

test("other system subtypes are ignored", () => {
  expect(parseLine(JSON.stringify({ type: "system", subtype: "thinking_tokens", session_id: "abc" }))).toEqual([]);
});

test("assistant text + tool_use blocks map to text + status", () => {
  const line = JSON.stringify({
    type: "assistant",
    session_id: "abc",
    message: {
      role: "assistant",
      content: [
        { type: "text", text: "hi" },
        { type: "tool_use", name: "Bash", input: { command: "echo hi" } },
        { type: "tool_use", name: "Edit", input: { file_path: "src/foo.ts" } },
      ],
    },
  });
  expect(parseLine(line)).toEqual([
    { type: "text", text: "hi" },
    { type: "status", text: "Bash: echo hi" },
    { type: "status", text: "Edit: src/foo.ts" },
  ]);
});

test("result success yields result with usage + session id", () => {
  const line = JSON.stringify({
    type: "result",
    subtype: "success",
    is_error: false,
    result: "ready",
    session_id: "abc",
    usage: { output_tokens: 3 },
  });
  expect(parseLine(line)).toEqual([{ type: "result", usage: { output_tokens: 3 }, sessionId: "abc" }]);
});

test("result error yields error", () => {
  const line = JSON.stringify({ type: "result", subtype: "error", is_error: true, result: "boom", session_id: "abc" });
  expect(parseLine(line)).toEqual([{ type: "error", message: "boom" }]);
});

test("non-json / unknown lines yield nothing", () => {
  expect(parseLine("not json")).toEqual([]);
  expect(parseLine(JSON.stringify({ type: "rate_limit_event" }))).toEqual([]);
});
