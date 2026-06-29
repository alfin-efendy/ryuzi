import { test, expect } from "bun:test";
import { ClaudeCodeHarness, type ClaudeRunner } from "../src/agents/claude-code/index";
import type { AgentEvent, AgentRunInput } from "../src/agents/types";

function runInput(over: Partial<AgentRunInput> = {}): AgentRunInput {
  return {
    workdir: "/wt",
    prompt: "go",
    permissionMode: "default",
    signal: new AbortController().signal,
    approve: async () => ({ behavior: "allow" }),
    ...over,
  };
}

const scripted =
  (lines: string[]): ClaudeRunner =>
  () =>
    (async function* () {
      for (const l of lines) yield l;
    })();

async function collect(it: AsyncIterable<AgentEvent>): Promise<AgentEvent[]> {
  const out: AgentEvent[] = [];
  for await (const e of it) out.push(e);
  return out;
}

test("new session emits init first, then parsed events", async () => {
  const lines = [
    JSON.stringify({ type: "assistant", message: { content: [{ type: "text", text: "hello" }] } }),
    JSON.stringify({ type: "result", is_error: false, result: "hello", session_id: "sx", usage: { output_tokens: 1 } }),
  ];
  const h = new ClaudeCodeHarness(scripted(lines));
  const events = await collect(h.run(runInput()));
  expect(events[0]?.type).toBe("init"); // our generated uuid
  expect(events.some((e) => e.type === "text" && e.text === "hello")).toBe(true);
  expect(events.at(-1)).toEqual({ type: "result", usage: { output_tokens: 1 }, sessionId: "sx" });
});

test("resume does NOT emit a leading init from us", async () => {
  const h = new ClaudeCodeHarness(scripted([]));
  const events = await collect(h.run(runInput({ resume: "prev" })));
  expect(events.find((e) => e.type === "init")).toBeUndefined();
});

test("runner error becomes an error event", async () => {
  const boom: ClaudeRunner = () =>
    (async function* () {
      throw new Error("spawn failed");
    })();
  const h = new ClaudeCodeHarness(boom);
  const events = await collect(h.run(runInput({ resume: "p" })));
  expect(events).toEqual([{ type: "error", message: "spawn failed" }]);
});
