import { test, expect } from "bun:test";
import {
  buildTranscript,
  closeDanglingFence,
  editCardsForGroups,
  formatTurnDuration,
  groupRows,
  messageToRow,
  turnDurationMs,
  type Row,
  type TurnBlock,
} from "./transcript";

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
  createdAt: null,
  attachments: [],
  toolPath: null,
  ...partial,
});

test("consecutive assistant text chunks coalesce into one markdown group, joined with ''", () => {
  const groups = groupRows([row({ seq: 1, text: "Hello **wor" }), row({ seq: 2, text: "ld**" }), row({ seq: 3, text: "!" })]);
  expect(groups).toHaveLength(1);
  expect(groups[0]).toEqual({ type: "agent", key: "s1", markdown: "Hello **world**!" });
});

test("whitespace-only chunks are kept inside a run but never form a group alone", () => {
  const groups = groupRows([row({ seq: 1, text: "para one" }), row({ seq: 2, text: "\n\n" }), row({ seq: 3, text: "para two" })]);
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
    row({
      seq: 1,
      blockType: "tool_call",
      toolCallId: "t1",
      toolName: "Bash",
      toolKind: "execute",
      toolStatus: "completed",
      toolOutput: "ok",
    }),
    row({ seq: 2, role: "system", blockType: "status", text: "wrote a.txt" }),
    row({ seq: 3, role: "system", blockType: "status", text: "  " }),
    row({ seq: 4, blockType: "tool_call", toolCallId: "t2", toolName: null, toolKind: null, toolStatus: "pending" }),
    row({ seq: 5, text: "done" }),
  ]);
  expect(groups.map((g) => g.type)).toEqual(["activity", "agent"]);
  if (groups[0].type !== "activity") throw new Error("expected activity group");
  expect(groups[0].items).toEqual([
    { type: "tool", key: "s1", name: "Bash", kind: "execute", status: "completed", output: "ok", path: null },
    { type: "status", key: "s2", text: "wrote a.txt" },
    { type: "tool", key: "s4", name: "Tool", kind: null, status: "pending", output: null, path: null },
  ]);
});

test("editCardsForGroups dedupes completed edit/delete/move targets", () => {
  const rows: Row[] = [
    row({
      seq: 1,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t1",
      toolName: "edit",
      toolKind: "edit",
      toolStatus: "completed",
      toolPath: "src/a.ts",
    }),
    row({
      seq: 2,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t2",
      toolName: "edit",
      toolKind: "edit",
      toolStatus: "completed",
      toolPath: "src/a.ts",
    }),
    row({
      seq: 3,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t3",
      toolName: "delete",
      toolKind: "delete",
      toolStatus: "completed",
      toolPath: "src/b.ts",
    }),
    row({
      seq: 4,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t4",
      toolName: "edit",
      toolKind: "edit",
      toolStatus: "failed",
      toolPath: "src/c.ts",
    }),
    row({
      seq: 5,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t5",
      toolName: "read",
      toolKind: "read",
      toolStatus: "completed",
      toolPath: "src/d.ts",
    }),
  ];
  expect(editCardsForGroups(groupRows(rows))).toEqual([
    { path: "src/a.ts", kind: "edit" },
    { path: "src/b.ts", kind: "delete" },
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

test("notice rows (e.g. compaction) get their own group, distinct from errors and agent text", () => {
  const groups = groupRows([
    row({ seq: 1, blockType: "notice", text: "Context compacted: ~100k → ~20k tokens", role: "system" }),
  ]);
  expect(groups).toEqual([{ type: "notice", key: "s1", text: "Context compacted: ~100k → ~20k tokens" }]);
});

test("closeDanglingFence closes an odd number of line-start fences and leaves balanced ones alone", () => {
  expect(closeDanglingFence("```ts\nconst x = 1;")).toBe("```ts\nconst x = 1;\n```");
  expect(closeDanglingFence("```ts\nx\n```")).toBe("```ts\nx\n```");
  expect(closeDanglingFence("inline ``` mention")).toBe("inline ``` mention");
  expect(closeDanglingFence("no fences")).toBe("no fences");
});

test("messageToRow extracts attachments metadata from user payloads", () => {
  const row = messageToRow(
    3,
    "user",
    "text",
    { text: "look", attachments: [{ name: "a.png", path: "C:\\att\\a.png", contentType: "image/png", size: 42 }] },
    null,
    null,
    null,
    1700000000000,
  );
  expect(row.text).toBe("look");
  expect(row.createdAt).toBe(1700000000000);
  expect(row.attachments).toEqual([{ name: "a.png", path: "C:\\att\\a.png", contentType: "image/png", size: 42 }]);
});

test("messageToRow tolerates missing/malformed attachments", () => {
  expect(messageToRow(1, "user", "text", { text: "hi" }, null, null, null, null).attachments).toEqual([]);
  expect(messageToRow(1, "user", "text", { text: "hi", attachments: "junk" }, null, null, null, null).attachments).toEqual([]);
  expect(messageToRow(1, "user", "text", { text: "hi", attachments: [{ nope: true }] }, null, null, null, null).attachments).toEqual([]);
});

test("messageToRow extracts the tool target path from tool_call input", () => {
  const row = messageToRow(2, "assistant", "tool_call", { name: "edit", input: { path: "src/app.ts" } }, "t1", "completed", "edit", null);
  expect(row.toolPath).toBe("src/app.ts");
  const alt = messageToRow(
    2,
    "assistant",
    "tool_call",
    { name: "edit", input: { file_path: "src/b.ts" } },
    "t1",
    "completed",
    "edit",
    null,
  );
  expect(alt.toolPath).toBe("src/b.ts");
  const none = messageToRow(2, "assistant", "tool_call", { name: "bash", input: { command: "ls" } }, "t1", "completed", "execute", null);
  expect(none.toolPath).toBeNull();
});

const turn: Row[] = [
  row({ seq: 1, role: "user", blockType: "text", text: "do it", createdAt: 1000 }),
  row({ seq: 2, role: "assistant", blockType: "thought", text: "hmm", createdAt: 2000 }),
  row({
    seq: 3,
    role: "assistant",
    blockType: "tool_call",
    toolCallId: "t1",
    toolName: "edit",
    toolKind: "edit",
    toolStatus: "completed",
    toolPath: "src/a.ts",
    createdAt: 3000,
  }),
  row({ seq: 4, role: "assistant", blockType: "text", text: "done!", createdAt: 37000 }),
];

test("completed turn collapses thought+activity into one summary, text stays visible", () => {
  const blocks = buildTranscript(turn, false);
  expect(blocks.map((b) => b.type)).toEqual(["user", "summary", "agent"]);
  const summary = blocks[1] as Extract<(typeof blocks)[number], { type: "summary" }>;
  expect(summary.groups.map((g) => g.type)).toEqual(["thought", "activity"]);
  expect(summary.durationMs).toBe(36000);
});

test("summary blocks carry the turn's edit cards", () => {
  const blocks = buildTranscript(turn, false);
  const summary = blocks.find((b) => b.type === "summary") as Extract<(typeof blocks)[number], { type: "summary" }>;
  expect(summary.editCards).toEqual([{ path: "src/a.ts", kind: "edit" }]);
});

test("the live last turn stays uncollapsed while running", () => {
  const blocks = buildTranscript(turn, true);
  expect(blocks.map((b) => b.type)).toEqual(["user", "thought", "activity", "agent"]);
});

test("earlier turns collapse even while a later turn runs", () => {
  const second = [
    row({ seq: 5, role: "user", blockType: "text", text: "more", createdAt: 40000 }),
    row({
      seq: 6,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t2",
      toolName: "read",
      toolKind: "read",
      toolStatus: "in_progress",
      createdAt: 41000,
    }),
  ];
  const blocks = buildTranscript([...turn, ...second], true);
  expect(blocks.map((b) => b.type)).toEqual(["user", "summary", "agent", "user", "activity"]);
});

test("a turn with no activity has no summary", () => {
  const chat: Row[] = [
    row({ seq: 1, role: "user", blockType: "text", text: "hi", createdAt: 1 }),
    row({ seq: 2, role: "assistant", blockType: "text", text: "hello", createdAt: 2 }),
  ];
  expect(buildTranscript(chat, false).map((b) => b.type)).toEqual(["user", "agent"]);
});

test("turn duration is null without timestamps", () => {
  expect(turnDurationMs([row({ seq: 1, role: "user", blockType: "text", createdAt: null })])).toBeNull();
});

test("formatTurnDuration", () => {
  expect(formatTurnDuration(36000)).toBe("36s");
  expect(formatTurnDuration(239000)).toBe("3m 59s");
  expect(formatTurnDuration(null)).toBe("");
});

test("transient (seq 0) rows in different turns get distinct keys", () => {
  // Both error rows sit at local index 1 of their turn slice; without an
  // absolute offset their fallback keys would both be "i1" and collide.
  const chat: Row[] = [
    row({ seq: 1, role: "user", blockType: "text", text: "first", createdAt: 1 }),
    row({ seq: 0, role: "system", blockType: "error", text: "boom one" }),
    row({ seq: 2, role: "user", blockType: "text", text: "second", createdAt: 2 }),
    row({ seq: 0, role: "system", blockType: "error", text: "boom two" }),
  ];
  const blocks = buildTranscript(chat, false);
  const keys = blocks.map((b) => b.key);
  expect(new Set(keys).size).toBe(keys.length);
  const errorKeys = blocks.flatMap((b) => (b.type === "error" ? [b.key] : []));
  expect(errorKeys).toHaveLength(2);
  expect(errorKeys[0]).not.toBe(errorKeys[1]);
});

test("only the last agent text of a completed turn is flagged turnEnd", () => {
  const rows: Row[] = [
    row({ seq: 1, role: "user", blockType: "text", text: "q", createdAt: 1 }),
    row({ seq: 2, role: "assistant", blockType: "text", text: "part 1", createdAt: 2 }),
    row({
      seq: 3,
      role: "assistant",
      blockType: "tool_call",
      toolCallId: "t1",
      toolName: "read",
      toolKind: "read",
      toolStatus: "completed",
      createdAt: 3,
    }),
    row({ seq: 4, role: "assistant", blockType: "text", text: "part 2", createdAt: 4 }),
  ];
  const blocks = buildTranscript(rows, false);
  const agents = blocks.filter((b): b is Extract<TurnBlock, { type: "agent" }> => b.type === "agent");
  expect(agents.map((a) => a.turnEnd === true)).toEqual([false, true]);
  // and never while the turn is live:
  const live = buildTranscript(rows, true).filter((b) => b.type === "agent");
  expect(live.every((a) => !("turnEnd" in a) || a.turnEnd !== true)).toBe(true);
});
