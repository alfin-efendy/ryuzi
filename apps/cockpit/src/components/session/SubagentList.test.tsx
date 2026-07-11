import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { useStore } from "@/store";
import type { Row } from "@/lib/transcript";

const { SubagentList } = await import("./SubagentList");

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

afterEach(cleanup);
beforeEach(() => {
  useStore.setState({ transcripts: {} });
});

test("renders a roster row per sub-agent with a count label", () => {
  useStore.setState({
    transcripts: {
      s1: [
        row({ toolSubagent: "explore", createdAt: 2 }),
        row({ toolSubagent: "explore", createdAt: 3 }),
        row({ toolSubagent: "plan", createdAt: 1 }),
      ],
    },
  });
  render(<SubagentList sessionPk="s1" />);
  expect(screen.getByText("explore")).toBeTruthy();
  expect(screen.getByText("2 calls")).toBeTruthy();
  expect(screen.getByText("plan")).toBeTruthy();
  expect(screen.getByText("1 call")).toBeTruthy();
});

test("empty transcript shows the empty-state line", () => {
  render(<SubagentList sessionPk="s1" />);
  expect(screen.getByText(/no sub-agents/i)).toBeTruthy();
});
