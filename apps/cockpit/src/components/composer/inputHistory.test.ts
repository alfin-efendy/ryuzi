import { describe, expect, test } from "bun:test";
import type { Row } from "@/lib/transcript";
import { HISTORY_IDLE, historyEntries, shouldNavigateHistory, stepHistory } from "./inputHistory";

function row(overrides: Partial<Row>): Row {
  return {
    seq: 1,
    role: "user",
    blockType: "text",
    text: "hi",
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
    ...overrides,
  };
}

describe("historyEntries", () => {
  test("collects sent user text rows newest-first, skipping non-user/non-text/empty rows", () => {
    const rows: Row[] = [
      row({ seq: 1, text: "first" }),
      row({ seq: 2, role: "assistant", text: "reply" }),
      row({ seq: 3, blockType: "status", text: "ignored status" }),
      row({ seq: 4, text: "   " }), // attachment-only user row: whitespace text
      row({ seq: 5, text: "second" }),
    ];
    expect(historyEntries(rows)).toEqual(["second", "first"]);
  });

  test("empty transcript yields no entries", () => {
    expect(historyEntries([])).toEqual([]);
  });
});

describe("shouldNavigateHistory", () => {
  test("suppressed while a slash/@ MenuPanel is open", () => {
    expect(shouldNavigateHistory("up", "", 0, 0, true)).toBe(false);
    expect(shouldNavigateHistory("down", "abc", 3, 3, true)).toBe(false);
  });

  test("suppressed while text is selected", () => {
    expect(shouldNavigateHistory("up", "abc", 0, 2, false)).toBe(false);
  });

  test("empty field triggers both directions", () => {
    expect(shouldNavigateHistory("up", "", 0, 0, false)).toBe(true);
    expect(shouldNavigateHistory("down", "", 0, 0, false)).toBe(true);
  });

  test("up only when the caret is on the first line", () => {
    const v = "line1\nline2";
    expect(shouldNavigateHistory("up", v, 3, 3, false)).toBe(true); // inside line1
    expect(shouldNavigateHistory("up", v, 8, 8, false)).toBe(false); // inside line2
  });

  test("down only when the caret is on the last line", () => {
    const v = "line1\nline2";
    expect(shouldNavigateHistory("down", v, 8, 8, false)).toBe(true); // inside line2
    expect(shouldNavigateHistory("down", v, 3, 3, false)).toBe(false); // inside line1
  });
});

describe("stepHistory", () => {
  const entries = ["newest", "older", "oldest"]; // newest-first

  test("up from idle stashes the live draft (pending buffer) and shows the newest entry", () => {
    const step = stepHistory("up", entries, HISTORY_IDLE, "wip draft");
    expect(step).toEqual({ state: { index: 0, pending: "wip draft" }, text: "newest" });
  });

  test("up keeps walking older entries and stops at the oldest (no wrap-around)", () => {
    const s1 = stepHistory("up", entries, { index: 0, pending: "wip" }, "newest");
    expect(s1).toEqual({ state: { index: 1, pending: "wip" }, text: "older" });
    expect(stepHistory("up", entries, { index: 2, pending: "wip" }, "oldest")).toBeNull();
  });

  test("up with no history does nothing", () => {
    expect(stepHistory("up", [], HISTORY_IDLE, "wip")).toBeNull();
  });

  test("down steps back toward newer entries", () => {
    const step = stepHistory("down", entries, { index: 2, pending: "wip" }, "oldest");
    expect(step).toEqual({ state: { index: 1, pending: "wip" }, text: "older" });
  });

  test("down past the newest entry restores the pending draft and leaves history", () => {
    const step = stepHistory("down", entries, { index: 0, pending: "wip draft" }, "newest");
    expect(step).toEqual({ state: { index: -1, pending: "" }, text: "wip draft" });
  });

  test("down while not navigating does nothing", () => {
    expect(stepHistory("down", entries, HISTORY_IDLE, "wip")).toBeNull();
  });
});
