import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import type { BranchList, CmdError, CommandInfo, Project, Result } from "@/bindings";

const branchListData: BranchList = { branches: ["main", "develop"], current: "main", detached: false };
const listBranches = mock((): Promise<Result<BranchList, CmdError>> => Promise.resolve({ status: "ok", data: branchListData }));
const nativeCommands = mock((): Promise<Result<CommandInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const searchFiles = mock((): Promise<Result<string[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));

mock.module("@/bindings", () => ({
  commands: { listBranches, nativeCommands, searchFiles },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
// useComposerAttachments registers a Tauri drag-drop listener on mount.
mock.module("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: () => Promise.resolve(() => {}) }),
}));

const { HomeView } = await import("./HomeView");
const { useStore } = await import("@/store");
const { useNav } = await import("@/store-nav");
const { useConnections } = await import("@/store-connections");

function project(overrides: Partial<Project> = {}): Project {
  return {
    projectId: "p1",
    name: "demo",
    workdir: "C:\\code\\demo",
    source: null,
    harness: "native",
    model: null,
    effort: null,
    permMode: "default",
    createdAt: 1,
    isGit: true,
    ...overrides,
  };
}

beforeEach(() => {
  useStore.setState({ projects: [project()], selectedProjectId: "p1" });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({ catalog: [], connections: [], loaded: true });
  useNav.setState({ composerBranch: null, composerModel: null });
  listBranches.mockClear();
  nativeCommands.mockClear();
});

afterEach(cleanup);

test("git project: branch pill shows and branches are fetched", async () => {
  render(<HomeView />);
  // The Combobox trigger renders the resolved current branch. The trigger's
  // ARIA role is "combobox" (Base UI) with its accessible name taken from
  // aria-label="Branch"; the visible branch name lives in its text content.
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Branch" }).textContent).toContain("main"));
  expect(listBranches).toHaveBeenCalledWith("p1");
});

test("non-git project: no branch pill, no worktree toggle, no list_branches call", async () => {
  useStore.setState({ projects: [project({ isGit: false })] });
  render(<HomeView />);
  // The whole branch Combobox — trigger pill AND its worktree-Switch footer — is gone.
  expect(screen.queryByRole("combobox", { name: "Branch" })).toBeNull();
  // Let the other mount effect flush so a stray branch fetch would have fired by now.
  await waitFor(() => expect(nativeCommands).toHaveBeenCalledWith("p1"));
  expect(listBranches).not.toHaveBeenCalled();
});

test("composer text is read from the persisted draft map (key home:{projectId})", async () => {
  useNav.getState().setDraft("home:p1", "half-typed prompt");
  render(<HomeView />);
  await waitFor(() => {
    const box = screen.getByPlaceholderText("Do anything") as HTMLTextAreaElement;
    expect(box.value).toBe("half-typed prompt");
  });
  useNav.getState().clearDraft("home:p1");
});
