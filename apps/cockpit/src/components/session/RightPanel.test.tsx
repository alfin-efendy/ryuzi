import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";

const gitDiff = mock((): Promise<Result<string, CmdError>> => Promise.resolve({ status: "ok", data: "" }));
const sessionWorkdir = mock((): Promise<Result<string, CmdError>> => Promise.resolve({ status: "ok", data: "C:\\code\\demo" }));

mock.module("@/bindings", () => ({
  commands: { gitDiff, sessionWorkdir },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
// The File tab pulls in CodeMirror and file IPC — irrelevant to the Review-tab
// cases below, so stub both panes.
mock.module("@/components/FileViewer", () => ({ FileViewer: () => null }));
mock.module("@/components/FileTreePane", () => ({ FileTreePane: () => null }));

const { RightPanel } = await import("./RightPanel");
const { useNav } = await import("@/store-nav");

beforeEach(() => {
  useNav.setState({ rightOpen: true, rightTab: "review", rightMaximized: false });
  gitDiff.mockClear();
});

afterEach(cleanup);

test("non-git session: Review shows the empty state and never fetches a diff", () => {
  render(<RightPanel sessionPk="s1" branch={null} running={false} isGit={false} />);
  expect(screen.getByText(/Not a git repository/)).toBeTruthy();
  expect(screen.queryByText("No changes yet.")).toBeNull();
  // Mount effects already ran synchronously under act() — no fetch fired.
  expect(gitDiff).not.toHaveBeenCalled();
});

test("git session: Review fetches the diff as before", async () => {
  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);
  await waitFor(() => expect(gitDiff).toHaveBeenCalledWith("s1"));
  expect(screen.queryByText(/Not a git repository/)).toBeNull();
});
