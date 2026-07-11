import { expect, test } from "bun:test";
import { subagentSummaries } from "./subagents";
import type { Row } from "./transcript";

function row(over: Partial<Row>): Row {
  return {
    seq: 0,
    role: "assistant",
    blockType: "tool_call",
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
    speaker: null,
    taskId: null,
    ...over,
  };
}

test("groups tool rows by sub-agent, counts, flags running, sorts by recency", () => {
  const rows = [
    row({ toolSubagent: "explore", toolStatus: "completed", createdAt: 100 }),
    row({ toolSubagent: "explore", toolStatus: "in_progress", createdAt: 300 }),
    row({ toolSubagent: "plan", toolStatus: "completed", createdAt: 200 }),
    row({ toolSubagent: null, toolStatus: "completed", createdAt: 400 }), // no label → ignored
    row({ blockType: "text", toolSubagent: "explore", createdAt: 999 }), // not a tool_call → ignored
  ];
  const out = subagentSummaries(rows);
  expect(out.map((a) => a.name)).toEqual(["explore", "plan"]); // explore lastActivity 300 > plan 200
  const explore = out.find((a) => a.name === "explore");
  expect(explore).toEqual({ name: "explore", toolCount: 2, running: true, lastActivity: 300 });
  const plan = out.find((a) => a.name === "plan");
  expect(plan).toEqual({ name: "plan", toolCount: 1, running: false, lastActivity: 200 });
});

test("empty / no-subagent transcript yields an empty roster", () => {
  expect(subagentSummaries([])).toEqual([]);
  expect(subagentSummaries([row({ toolSubagent: null })])).toEqual([]);
});

test("running is true when a pending tool exists", () => {
  const out = subagentSummaries([row({ toolSubagent: "x", toolStatus: "pending", createdAt: 1 })]);
  expect(out[0].running).toBe(true);
});
