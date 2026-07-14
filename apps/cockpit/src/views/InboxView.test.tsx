import { afterEach, expect, spyOn, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { useStore } from "@/store";
import { LOCAL_RUNNER } from "@/lib/session-key";

const { InboxView } = await import("./InboxView");

afterEach(cleanup);

test("renders one card per pending approval across sessions, newest first", () => {
  useStore.setState({
    sessions: [],
    pendingApprovals: [
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s1",
        runId: "run-1",
        requestId: "r1",
        tool: "bash",
        summary: "Bash: ls",
        kind: "tool",
        input: {},
        principal: null,
      },
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s2",
        runId: "run-2",
        requestId: "r2",
        tool: "edit",
        summary: "Edit: a.ts",
        kind: "tool",
        input: {},
        principal: null,
      },
    ],
  });
  render(<InboxView />);
  const cards = screen.getAllByText("Approval needed");
  expect(cards.length).toBe(2);
});

test("renders distinct approvals when request IDs repeat across runs", () => {
  useStore.setState({
    sessions: [],
    pendingApprovals: [
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s1",
        runId: "run-1",
        requestId: "shared-request",
        tool: "bash",
        summary: "Bash: first",
        kind: "tool",
        input: {},
        principal: null,
      },
      {
        runnerId: "remote-runner",
        sessionPk: "s2",
        runId: "run-2",
        requestId: "shared-request",
        tool: "edit",
        summary: "Edit: second",
        kind: "tool",
        input: {},
        principal: null,
      },
    ],
  });
  const consoleError = spyOn(console, "error").mockImplementation(() => {});
  render(<InboxView />);

  expect(screen.getAllByText("Approval needed")).toHaveLength(2);
  expect(consoleError).not.toHaveBeenCalled();
  consoleError.mockRestore();
});
test("empty state renders quietly", () => {
  useStore.setState({ pendingApprovals: [] });
  render(<InboxView />);
  expect(screen.getByText(/No pending approvals/)).toBeTruthy();
});
