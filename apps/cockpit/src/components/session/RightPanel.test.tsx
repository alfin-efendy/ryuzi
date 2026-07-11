import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
const { useDiff } = await import("@/store-diff");
const { useUi } = await import("@/store-ui");

const APP_DIFF = ["diff --git a/src/app.ts b/src/app.ts", "--- a/src/app.ts", "+++ b/src/app.ts", "@@ -1 +1 @@", "-old", "+new"].join(
  "\n",
);

beforeEach(() => {
  useNav.setState({ rightOpen: true, rightTab: "review", rightMaximized: false });
  useDiff.setState({ bySession: {}, pendingReview: null });
  useUi.setState({ tabs: [], activeTabId: null });
  gitDiff.mockClear();
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: "" }));
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

test("completed diff selects and clears a pending transcript review target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(screen.getByText("src/app.ts")).toBeTruthy());
  expect(useDiff.getState().pendingReview).toBeNull();
});

test("completed diff clears an unmatched pending target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "src/missing.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().bySession.s1?.loading).toBe(false));
  expect(useDiff.getState().pendingReview).toBeNull();
  expect(screen.getByText("app.ts")).toBeTruthy();
});

test("refreshing to fewer files clamps the selected review file", async () => {
  useDiff.setState({
    bySession: {
      s1: {
        loading: false,
        error: null,
        files: [
          { dir: "src/", name: "first.ts", add: 1, del: 0, lines: [] },
          { dir: "src/", name: "second.ts", add: 1, del: 0, lines: [] },
        ],
      },
    },
  });
  const view = render(<RightPanel sessionPk="s1" branch="main" running isGit />);
  fireEvent.click(screen.getByTitle("src/second.ts"));

  useDiff.setState({
    bySession: { s1: { loading: false, error: null, files: [{ dir: "src/", name: "first.ts", add: 1, del: 0, lines: [] }] } },
  });
  view.rerender(<RightPanel sessionPk="s1" branch="main" running isGit />);

  expect(screen.getByText("first.ts")).toBeTruthy();
});

test("Review error keeps Refresh available and retries", async () => {
  gitDiff.mockImplementationOnce(() => Promise.resolve({ status: "error", error: { message: "diff failed" } }));
  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(screen.getByText("diff failed")).toBeTruthy());
  gitDiff.mockImplementationOnce(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  fireEvent.click(screen.getByTitle("Refresh diff"));

  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(2));
  await waitFor(() => expect(screen.queryByText("diff failed")).toBeNull());
});

test("many file tabs do not move the expand action out of the fixed header", () => {
  useNav.setState({ rightTab: "file" });
  useUi.setState({
    tabs: Array.from({ length: 12 }, (_, index) => ({
      id: `/work/file-${index}.ts`,
      kind: "file" as const,
      path: `/work/file-${index}.ts`,
      title: `file-${index}.ts`,
    })),
    activeTabId: "/work/file-0.ts",
  });

  render(<RightPanel sessionPk="s1" branch="main" running isGit />);

  const header = screen.getByTestId("right-panel-header");
  const expand = screen.getByTitle("Expand panel");
  expect(header.contains(expand)).toBe(true);
  expect(expand.parentElement?.className).toContain("shrink-0");
});
