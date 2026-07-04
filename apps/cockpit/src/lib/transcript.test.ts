import { test, expect } from "bun:test";
import { closeDanglingFence, groupRows, type Row } from "./transcript";

const row = (partial: Partial<Row>): Row => ({
  seq: 0,
  role: "assistant",
  blockType: "text",
  text: "",
  toolCallId: null,
  toolStatus: null,
  toolKind: null,
  toolName: null,
  toolOutput: null,
  ...partial,
});

test("consecutive assistant text chunks coalesce into one markdown group, joined with ''", () => {
  const groups = groupRows([
    row({ seq: 1, text: "Hello **wor" }),
    row({ seq: 2, text: "ld**" }),
    row({ seq: 3, text: "!" }),
  ]);
  expect(groups).toHaveLength(1);
  expect(groups[0]).toEqual({ type: "agent", key: "s1", markdown: "Hello **world**!" });
});

test("whitespace-only chunks are kept inside a run but never form a group alone", () => {
  const groups = groupRows([
    row({ seq: 1, text: "para one" }),
    row({ seq: 2, text: "\n\n" }),
    row({ seq: 3, text: "para two" }),
  ]);
  expect(groups).toHaveLength(1);
  if (groups[0].type !== "agent") throw new Error("expected agent group");
  expect(groups[0].markdown).toBe("para one\n\npara two");
  expect(groupRows([row({ seq: 1, text: "  \n " })])).toHaveLength(0);
});

test("thought runs group separately from answer text", () => {
  const groups = groupRows([
    row({ seq: 1, blockType: "thought", text: "hmm " }),
    row({ seq: 2, blockType: "thought", text: "okay" }),
    row({ seq: 3, text: "The answer." }),
  ]);
  expect(groups.map((g) => g.type)).toEqual(["thought", "agent"]);
  if (groups[0].type !== "thought") throw new Error("expected thought group");
  expect(groups[0].markdown).toBe("hmm okay");
});

test("a user row breaks an agent run; blank user rows are dropped", () => {
  const groups = groupRows([
    row({ seq: 1, text: "first" }),
    row({ seq: 2, role: "user", text: "question?" }),
    row({ seq: 3, text: "second" }),
    row({ seq: 4, role: "user", text: "   " }),
  ]);
  expect(groups.map((g) => g.type)).toEqual(["agent", "user", "agent"]);
});

test("consecutive tool_call/status rows cluster into one activity group", () => {
  const groups = groupRows([
    row({ seq: 1, blockType: "tool_call", toolCallId: "t1", toolName: "Bash", toolKind: "execute", toolStatus: "completed", toolOutput: "ok" }),
    row({ seq: 2, role: "system", blockType: "status", text: "wrote a.txt" }),
    row({ seq: 3, role: "system", blockType: "status", text: "  " }),
    row({ seq: 4, blockType: "tool_call", toolCallId: "t2", toolName: null, toolKind: null, toolStatus: "pending" }),
    row({ seq: 5, text: "done" }),
  ]);
  expect(groups.map((g) => g.type)).toEqual(["activity", "agent"]);
  if (groups[0].type !== "activity") throw new Error("expected activity group");
  expect(groups[0].items).toEqual([
    { type: "tool", key: "s1", name: "Bash", kind: "execute", status: "completed", output: "ok" },
    { type: "status", key: "s2", text: "wrote a.txt" },
    { type: "tool", key: "s4", name: "Tool", kind: null, status: "pending", output: null },
  ]);
});

test("error rows and unknown block types: error gets its own group, unknown renders as agent text", () => {
  const groups = groupRows([
    row({ seq: 0, blockType: "error", text: "boom", role: "system" }),
    row({ seq: 2, blockType: "somethingnew", text: "future" }),
  ]);
  expect(groups.map((g) => g.type)).toEqual(["error", "agent"]);
  expect(groups[0].key).toBe("i0"); // transient rows (seq 0) key by index
  expect(groups[1].key).toBe("s2"); // persisted rows key by seq
});

test("closeDanglingFence closes an odd number of line-start fences and leaves balanced ones alone", () => {
  expect(closeDanglingFence("```ts\nconst x = 1;")).toBe("```ts\nconst x = 1;\n```");
  expect(closeDanglingFence("```ts\nx\n```")).toBe("```ts\nx\n```");
  expect(closeDanglingFence("inline ``` mention")).toBe("inline ``` mention");
  expect(closeDanglingFence("no fences")).toBe("no fences");
});
