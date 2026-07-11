import { test, expect } from "bun:test";
import {
  buildTranscript,
  closeDanglingFence,
  editCardsForGroups,
  formatToolDuration,
  formatTurnDuration,
  groupRows,
  mergeToolRow,
  messageToRow,
  partitionActivity,
  toolCardHeader,
  toolInputSummary,
  turnDurationMs,
  type ActivityItem,
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
  toolInput: null,
  toolDurationMs: null,
  toolExitCode: null,
  toolSummary: null,
  toolSubagent: null,
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
    {
      type: "tool",
      key: "s1",
      name: "Bash",
      kind: "execute",
      status: "completed",
      output: "ok",
      path: null,
      input: null,
      durationMs: null,
      exitCode: null,
      summary: null,
      subagent: null,
    },
    { type: "status", key: "s2", text: "wrote a.txt" },
    {
      type: "tool",
      key: "s4",
      name: "Tool",
      kind: null,
      status: "pending",
      output: null,
      path: null,
      input: null,
      durationMs: null,
      exitCode: null,
      summary: null,
      subagent: null,
    },
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
  const groups = groupRows([row({ seq: 1, blockType: "notice", text: "Context compacted: ~100k → ~20k tokens", role: "system" })]);
  expect(groups).toEqual([{ type: "notice", key: "s1", text: "Context compacted: ~100k → ~20k tokens" }]);
});

test("route switch copy groups as notices for model, account, failover, and combined changes", () => {
  const notices = [
    "Switched to 5.6 Sol · Ultra",
    "Account switched to Work Codex · round robin",
    "Account switched to Backup Codex · quota unavailable",
    "Switched to Opus 4.1 via Backup Claude · authentication unavailable",
  ];

  const groups = groupRows(notices.map((text, index) => row({ seq: index + 1, role: "system", blockType: "notice", text })));
  expect(groups).toEqual(notices.map((text, index) => ({ type: "notice", key: `s${index + 1}`, text })));
});

test("closeDanglingFence closes an odd number of line-start fences and leaves balanced ones alone", () => {
  expect(closeDanglingFence("```ts\nconst x = 1;")).toBe("```ts\nconst x = 1;\n```");
  expect(closeDanglingFence("```ts\nx\n```")).toBe("```ts\nx\n```");
  expect(closeDanglingFence("inline ``` mention")).toBe("inline ``` mention");
  expect(closeDanglingFence("no fences")).toBe("no fences");
});

test("messageToRow extracts attachments metadata from user payloads, preferring a recorded rel", () => {
  const row = messageToRow(
    3,
    "user",
    "text",
    {
      text: "look",
      attachments: [{ name: "a.png", path: "C:\\att\\sess-9\\a.png", contentType: "image/png", size: 42, rel: "sess-9/a.png" }],
    },
    null,
    null,
    null,
    1700000000000,
    "sess-9",
  );
  expect(row.text).toBe("look");
  expect(row.createdAt).toBe(1700000000000);
  expect(row.attachments).toEqual([
    { name: "a.png", path: "C:\\att\\sess-9\\a.png", contentType: "image/png", size: 42, rel: "sess-9/a.png" },
  ]);
});

test("messageToRow derives rel from sessionPk + basename for pre-P4-3 rows with no recorded rel", () => {
  const row = messageToRow(
    3,
    "user",
    "text",
    { text: "look", attachments: [{ name: "a.png", path: "C:\\att\\sess-9\\a.png", contentType: "image/png", size: 42 }] },
    null,
    null,
    null,
    null,
    "sess-9",
  );
  expect(row.attachments).toEqual([
    { name: "a.png", path: "C:\\att\\sess-9\\a.png", contentType: "image/png", size: 42, rel: "sess-9/a.png" },
  ]);
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

test("live startup activity renders after the initial user bubble", () => {
  const startupRows: Row[] = [
    row({ seq: 1, role: "system", blockType: "status", text: "Creating worktree…", createdAt: 1 }),
    row({ seq: 2, role: "system", blockType: "status", text: "Created and checked out branch harness/s1", createdAt: 2 }),
    row({ seq: 3, role: "system", blockType: "status", text: "Connecting tools…", createdAt: 3 }),
    row({ seq: 4, role: "user", blockType: "text", text: "Fix the issue", createdAt: 4 }),
  ];

  const blocks = buildTranscript(startupRows, true);

  expect(blocks.map((block) => block.type)).toEqual(["user", "activity"]);
  const activity = blocks[1] as Extract<TurnBlock, { type: "activity" }>;
  expect(activity.items).toEqual([
    { type: "status", key: "s1", text: "Creating worktree…" },
    { type: "status", key: "s2", text: "Created and checked out branch harness/s1" },
    { type: "status", key: "s3", text: "Connecting tools…" },
  ]);
});

test("live activity that already follows a user bubble keeps its order", () => {
  const rows: Row[] = [
    row({ seq: 1, role: "user", blockType: "text", text: "Fix the issue", createdAt: 1 }),
    row({ seq: 2, role: "system", blockType: "status", text: "Connecting tools…", createdAt: 2 }),
  ];

  expect(buildTranscript(rows, true).map((block) => block.type)).toEqual(["user", "activity"]);
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

test("messageToRow carries tool input, duration_ms, exit_code and summary from tool_call payloads", () => {
  const r = messageToRow(
    2,
    "assistant",
    "tool_call",
    { name: "bash", input: { command: "ls -la" }, output: "total 0", duration_ms: 1234, exit_code: 0, summary: "" },
    "t1",
    "completed",
    "execute",
    null,
  );
  expect(r.toolInput).toEqual({ command: "ls -la" });
  expect(r.toolDurationMs).toBe(1234);
  expect(r.toolExitCode).toBe(0);
  expect(r.toolSummary).toBeNull(); // empty string never renders
  const todo = messageToRow(
    3,
    "assistant",
    "tool_call",
    { name: "todowrite", summary: "todos: 1/2 done" },
    "t2",
    "completed",
    "other",
    null,
  );
  expect(todo.toolSummary).toBe("todos: 1/2 done");
  // Wrong types and non-tool_call rows stay null.
  const junk = messageToRow(
    4,
    "assistant",
    "tool_call",
    { name: "x", duration_ms: "fast", exit_code: "zero" },
    "t3",
    "completed",
    null,
    null,
  );
  expect(junk.toolDurationMs).toBeNull();
  expect(junk.toolExitCode).toBeNull();
  const text = messageToRow(5, "assistant", "text", { text: "hi", duration_ms: 5 }, null, null, null, null);
  expect(text.toolDurationMs).toBeNull();
  expect(text.toolInput).toBeNull();
});

test("mergeToolRow overlays duration/exit/summary/input from the merged payload and keeps prior values otherwise", () => {
  const prev = row({
    seq: 1,
    blockType: "tool_call",
    toolCallId: "t1",
    toolName: "bash",
    toolStatus: "in_progress",
    toolInput: { command: "ls" },
  });
  const merged = mergeToolRow(
    prev,
    { name: "bash", input: { command: "ls" }, output: "ok", duration_ms: 88, exit_code: 1, summary: "" },
    "failed",
    "execute",
  );
  expect(merged.toolDurationMs).toBe(88);
  expect(merged.toolExitCode).toBe(1);
  expect(merged.toolInput).toEqual({ command: "ls" });
  expect(merged.toolSummary).toBeNull();
  const keep = mergeToolRow(merged, { output: "more" }, "failed", "execute");
  expect(keep.toolDurationMs).toBe(88);
  expect(keep.toolExitCode).toBe(1);
  expect(keep.toolInput).toEqual({ command: "ls" });
});

test("groupRows tool items carry input/duration/exitCode/summary", () => {
  const groups = groupRows([
    row({
      seq: 1,
      blockType: "tool_call",
      toolCallId: "t1",
      toolName: "bash",
      toolKind: "execute",
      toolStatus: "completed",
      toolOutput: "ok",
      toolInput: { command: "ls" },
      toolDurationMs: 42,
      toolExitCode: 0,
      toolSummary: null,
    }),
  ]);
  if (groups[0].type !== "activity" || groups[0].items[0].type !== "tool") throw new Error("expected a tool item");
  expect(groups[0].items[0].input).toEqual({ command: "ls" });
  expect(groups[0].items[0].durationMs).toBe(42);
  expect(groups[0].items[0].exitCode).toBe(0);
  expect(groups[0].items[0].summary).toBeNull();
});

test("toolInputSummary derives a header line from the input shape", () => {
  expect(toolInputSummary({ command: "bun test\n# second line" }, null)).toBe("$ bun test");
  expect(toolInputSummary({ pattern: "TODO|FIXME" }, null)).toBe("TODO|FIXME");
  expect(toolInputSummary({ file_path: "src/a.ts" }, "src/a.ts")).toBe("src/a.ts");
  expect(toolInputSummary({ url: "https://example.com" }, null)).toBe("https://example.com");
  expect(toolInputSummary({ frobnicate: true, depth: 2 }, null)).toBe('{"frobnicate":true,"depth":2}');
  expect(toolInputSummary({}, null)).toBeNull();
  expect(toolInputSummary(null, null)).toBeNull();
});

test("toolCardHeader prefers summary extras and dedupes details already in the title", () => {
  // todo/task/memory: the summary display extra is the collapsed one-liner.
  expect(toolCardHeader({ name: "todowrite", input: { todos: [] }, path: null, summary: "todos: 1/2 done" })).toEqual({
    title: "todowrite",
    detail: "todos: 1/2 done",
  });
  // Native bash: name is the bare tool id, detail is the command.
  expect(toolCardHeader({ name: "bash", input: { command: "ls -la" }, path: null, summary: null })).toEqual({
    title: "bash",
    detail: "$ ls -la",
  });
  // ACP: the title already embeds the command — never double-print.
  expect(toolCardHeader({ name: "ls -la", input: { command: "ls -la" }, path: null, summary: null })).toEqual({
    title: "ls -la",
    detail: null,
  });
  expect(toolCardHeader({ name: "Read README.md", input: {}, path: "README.md", summary: null })).toEqual({
    title: "Read README.md",
    detail: null,
  });
});

test("formatToolDuration", () => {
  expect(formatToolDuration(312)).toBe("312ms");
  expect(formatToolDuration(1400)).toBe("1.4s");
  expect(formatToolDuration(36000)).toBe("36s");
  expect(formatToolDuration(239000)).toBe("3m 59s");
  expect(formatToolDuration(null)).toBe("");
});

function toolItem(key: string, status: string, over: Partial<Extract<ActivityItem, { type: "tool" }>> = {}) {
  return {
    type: "tool" as const,
    key,
    name: "read",
    kind: "read",
    status,
    subagent: null,
    output: null,
    path: null,
    input: null,
    durationMs: null,
    exitCode: null,
    summary: null,
    ...over,
  };
}
const statusItem = (key: string, text: string) => ({ type: "status" as const, key, text });

test("liveTail keeps the last 3 visible and folds the rest with the full run length", () => {
  const items = [1, 2, 3, 4, 5].map((n) => toolItem(`t${n}`, "completed"));
  const frags = partitionActivity(items, true);
  expect(frags[0]).toEqual({ kind: "fold", items: items.slice(0, 2), runLength: 5 });
  expect(frags.slice(1)).toEqual(items.slice(2).map((item) => ({ kind: "item", item })));
});

test("liveTail with 3 or fewer items folds nothing", () => {
  const items = [1, 2, 3].map((n) => toolItem(`t${n}`, "completed"));
  expect(partitionActivity(items, true)).toEqual(items.map((item) => ({ kind: "item", item })));
});

test("an in-progress tool never folds and splits the fold", () => {
  const items = [
    toolItem("t1", "completed"),
    toolItem("t2", "in_progress"),
    toolItem("t3", "completed"),
    toolItem("t4", "completed"),
    toolItem("t5", "completed"),
    toolItem("t6", "completed"),
  ];
  const frags = partitionActivity(items, true);
  // t1 folds; t2 standalone (in-progress); t3 folds; t4-t6 are the tail.
  expect(frags).toEqual([
    { kind: "fold", items: [items[0]], runLength: 6 },
    { kind: "item", item: items[1] },
    { kind: "fold", items: [items[2]], runLength: 6 },
    { kind: "item", item: items[3] },
    { kind: "item", item: items[4] },
    { kind: "item", item: items[5] },
  ]);
});

test("pending counts as in-progress; failed folds", () => {
  const items = [
    toolItem("t1", "failed"),
    toolItem("t2", "pending"),
    toolItem("t3", "completed"),
    toolItem("t4", "completed"),
    toolItem("t5", "completed"),
    toolItem("t6", "completed"),
  ];
  const frags = partitionActivity(items, false);
  expect(frags).toEqual([
    { kind: "fold", items: [items[0]], runLength: 6 },
    { kind: "item", item: items[1] },
    { kind: "fold", items: [items[2], items[3], items[4], items[5]], runLength: 6 },
  ]);
});

test("idle branch folds everything, including the most recent items", () => {
  const items = [1, 2, 3, 4].map((n) => toolItem(`t${n}`, "completed"));
  expect(partitionActivity(items, false)).toEqual([{ kind: "fold", items, runLength: 4 }]);
});

test("status items fold like completed tools", () => {
  const items = [statusItem("s1", "compacting"), toolItem("t1", "completed")];
  expect(partitionActivity(items, false)).toEqual([{ kind: "fold", items, runLength: 2 }]);
});

test("empty input yields no fragments", () => {
  expect(partitionActivity([], true)).toEqual([]);
});

test("sub-agent tool rows carry their subagent label through to activity items", () => {
  const r = messageToRow(
    5,
    "assistant",
    "tool_call",
    { name: "grep", input: { pattern: "x" }, subagent: "explore" },
    "tc-1",
    "in_progress",
    "search",
    null,
  );
  expect(r.toolSubagent).toBe("explore");
  const groups = groupRows([r]);
  if (groups[0].type !== "activity" || groups[0].items[0].type !== "tool") throw new Error("expected a tool item");
  expect(groups[0].items[0].subagent).toBe("explore");
});

test("parent tool rows have no subagent label", () => {
  const r = messageToRow(6, "assistant", "tool_call", { name: "bash", input: {} }, "tc-2", "completed", "execute", null);
  expect(r.toolSubagent).toBeNull();
});
