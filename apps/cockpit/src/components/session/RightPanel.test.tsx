import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

const APP_DIFF = ["diff --git a/src/app.ts b/src/app.ts", "--- a/src/app.ts", "+++ b/src/app.ts", "@@ -1 +1 @@", "-old", "+new"].join("\n");
const FRESH_DIFF = [
  "diff --git a/src/fresh.ts b/src/fresh.ts",
  "--- a/src/fresh.ts",
  "+++ b/src/fresh.ts",
  "@@ -1 +1 @@",
  "+const fresh = true;",
].join("\n");

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

test("pending review target waits for its fresh fetch instead of consuming a stale diff", async () => {
  let resolveDiff!: (result: Result<string, CmdError>) => void;
  gitDiff.mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveDiff = resolve;
      }),
  );
  useDiff.setState({
    bySession: {
      s1: {
        loading: false,
        error: null,
        files: [{ dir: "src/", name: "stale.ts", add: 1, del: 0, lines: [["add", 1, "const stale = true;"]] }],
      },
    },
    pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\fresh.ts" },
  });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(gitDiff).toHaveBeenCalledWith("s1"));
  expect(useDiff.getState().pendingReview).toEqual({ sessionPk: "s1", path: "C:\\code\\demo\\src\\fresh.ts" });

  await act(async () => {
    resolveDiff({ status: "ok", data: FRESH_DIFF });
  });

  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  await act(async () => {
    await Promise.resolve();
  });
  expect(gitDiff).toHaveBeenCalledTimes(1);
  expect(screen.getByTitle("src/fresh.ts").className).toContain("bg-accent");
  expect(screen.getByText("const fresh = true;")).toBeTruthy();
});

test("result-error target fetch clears without selecting a preserved stale match", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "error", error: { message: "diff failed" } }));
  useDiff.setState({
    bySession: {
      s1: {
        loading: false,
        error: null,
        files: [
          { dir: "src/", name: "first.ts", add: 1, del: 0, lines: [["add", 1, "const first = true;"]] },
          { dir: "src/", name: "second.ts", add: 1, del: 0, lines: [["add", 1, "const second = true;"]] },
        ],
      },
    },
    pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\second.ts" },
  });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  expect(screen.getByTitle("src/first.ts").className).toContain("bg-accent");
  expect(screen.getByTitle("src/second.ts").className).not.toContain("bg-accent");
  expect(screen.getByText("diff failed")).toBeTruthy();
});

test("rejected target fetch clears the pending review target", async () => {
  gitDiff.mockImplementation(() => Promise.reject(new Error("diff failed")));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
});

test("other-session pending review target remains unchanged", async () => {
  const pending = { sessionPk: "s2", path: "C:\\code\\demo\\src\\app.ts" };
  useDiff.setState({ pendingReview: pending });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(gitDiff).toHaveBeenCalledWith("s1"));
  expect(useDiff.getState().pendingReview).toEqual(pending);
});

test("completed diff selects and clears a pending transcript review target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(screen.getByText("src/app.ts")).toBeTruthy());
  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
});

test("completed diff clears an unmatched pending target", async () => {
  gitDiff.mockImplementation(() => Promise.resolve({ status: "ok", data: APP_DIFF }));
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "src/missing.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().bySession.s1?.loading).toBe(false));
  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  expect(screen.getByText("app.ts")).toBeTruthy();
});

test("pending review target stays pending while its fresh diff fetch is still loading", async () => {
  let resolveDiff!: (result: Result<string, CmdError>) => void;
  gitDiff.mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveDiff = resolve;
      }),
  );
  useDiff.setState({ pendingReview: { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" } });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  await waitFor(() => expect(useDiff.getState().bySession.s1?.loading).toBe(true));
  expect(useDiff.getState().pendingReview).toEqual({ sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" });

  await act(async () => {
    resolveDiff({ status: "ok", data: APP_DIFF });
  });

  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  expect(screen.getByText("app.ts")).toBeTruthy();
});

test("A -> B -> A target fetches do not let stale completions settle the latest A", async () => {
  const deferred: Array<{ resolve: (result: Result<string, CmdError>) => void }> = [];
  gitDiff.mockImplementation(
    () =>
      new Promise((resolve) => {
        deferred.push({ resolve });
      }),
  );
  const targetA = { sessionPk: "s1", path: "C:\\code\\demo\\src\\fresh.ts" };
  const targetB = { sessionPk: "s1", path: "C:\\code\\demo\\src\\app.ts" };
  useDiff.setState({ pendingReview: targetA });

  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);
  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(1));

  await act(async () => {
    useDiff.setState({ pendingReview: targetB });
  });
  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(2));

  await act(async () => {
    useDiff.setState({ pendingReview: targetA });
  });
  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(3));

  await act(async () => {
    deferred[0]?.resolve({ status: "ok", data: APP_DIFF });
    deferred[1]?.resolve({ status: "ok", data: APP_DIFF });
  });
  expect(useDiff.getState().pendingReview).toEqual(targetA);

  await act(async () => {
    deferred[2]?.resolve({ status: "ok", data: FRESH_DIFF });
  });
  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  expect(gitDiff).toHaveBeenCalledTimes(3);
  expect(screen.getByTitle("src/fresh.ts").className).toContain("bg-accent");
  expect(screen.getByText("const fresh = true;")).toBeTruthy();
});

test("remount attaches to an unresolved target fetch instead of fetching again", async () => {
  let resolveDiff!: (result: Result<string, CmdError>) => void;
  gitDiff.mockImplementation(
    () =>
      new Promise((resolve) => {
        resolveDiff = resolve;
      }),
  );
  const target = { sessionPk: "s1", path: "C:\\code\\demo\\src\\fresh.ts" };
  useDiff.setState({ pendingReview: target });

  const firstPanel = render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);
  await waitFor(() => expect(gitDiff).toHaveBeenCalledTimes(1));

  await act(async () => {
    firstPanel.unmount();
  });
  render(<RightPanel sessionPk="s1" branch="main" running={false} isGit />);

  expect(gitDiff).toHaveBeenCalledTimes(1);
  expect(useDiff.getState().pendingReview).toEqual(target);

  await act(async () => {
    resolveDiff({ status: "ok", data: FRESH_DIFF });
  });
  await waitFor(() => expect(useDiff.getState().pendingReview).toBeNull());
  expect(screen.getByTitle("src/fresh.ts").className).toContain("bg-accent");
  expect(screen.getByText("const fresh = true;")).toBeTruthy();
});

test("refreshing to fewer files clamps the selected review file", async () => {
  useDiff.setState({
    bySession: {
      s1: {
        loading: false,
        error: null,
        files: [
          { dir: "src/", name: "first.ts", add: 1, del: 0, lines: [["add", 1, "const first = true;"]] },
          { dir: "src/", name: "second.ts", add: 1, del: 0, lines: [["add", 1, "const second = true;"]] },
        ],
      },
    },
  });
  const view = render(<RightPanel sessionPk="s1" branch="main" running isGit />);
  act(() => {
    fireEvent.click(screen.getByTitle("src/second.ts"));
  });
  // Selecting second.ts moved the highlight and content pane off first.ts,
  // proving the clamp assertions below aren't trivially true beforehand.
  expect(screen.getByTitle("src/second.ts").className).toContain("bg-accent");
  expect(screen.getByText("const second = true;")).toBeTruthy();

  act(() => {
    useDiff.setState({
      bySession: {
        s1: {
          loading: false,
          error: null,
          files: [{ dir: "src/", name: "first.ts", add: 1, del: 0, lines: [["add", 1, "const first = true;"]] }],
        },
      },
    });
    view.rerender(<RightPanel sessionPk="s1" branch="main" running isGit />);
  });

  // The clamped index now points at the surviving first.ts — its row is
  // highlighted and its diff content (not stale second.ts content) is shown.
  expect(screen.getByTitle("src/first.ts").className).toContain("bg-accent");
  expect(screen.getByText("const first = true;")).toBeTruthy();
  expect(screen.queryByText("const second = true;")).toBeNull();
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
