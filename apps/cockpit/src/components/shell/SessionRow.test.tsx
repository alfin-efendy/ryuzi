import { afterEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const { SessionRow } = await import("./SessionRow");

afterEach(cleanup);

const base = {
  session: {
    runnerId: "local",
    sessionPk: "s1",
    primaryAgentId: null,
    primaryAgentSnapshot: null,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: "My session",
    status: "idle" as const,
    startedBy: null,
    createdAt: 0,
    lastActive: 1,
    resumeAttempts: 0,
    branchOwned: false,
    permMode: "default" as const,
    kind: "project" as const,
    speaker: null,
    agent: null,
    parentSessionPk: null,
  },
  isActive: false,
  isPinned: false,
  unread: false,
  isArchived: false,
  hasTail: false,
  archiveDisabled: false,
  onOpen: () => {},
  onTogglePin: () => {},
  onToggleArchive: () => {},
};

test("renders the title and, when unread, the unread dot", () => {
  const { rerender } = render(<SessionRow {...base} />);
  expect(screen.getByText("My session")).toBeTruthy();
  expect(screen.queryByTestId("unread-dot-s1")).toBeNull();
  rerender(<SessionRow {...base} unread />);
  expect(screen.getByTestId("unread-dot-s1")).toBeTruthy();
});

test("pin button reflects isPinned and calls onTogglePin", () => {
  let toggled = 0;
  render(<SessionRow {...base} isPinned onTogglePin={() => (toggled += 1)} />);
  fireEvent.click(screen.getByRole("button", { name: /unpin/i }));
  expect(toggled).toBe(1);
});

test("dragHandle renders when provided", () => {
  render(<SessionRow {...base} dragHandle={<span data-testid="grip" />} />);
  expect(screen.getByTestId("grip")).toBeTruthy();
});
