import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { BranchList, CatalogEntry, CmdError, CommandInfo, ConnectionInfo, Project, Result } from "@/bindings";

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
const { useAgent } = await import("@/store-agent");
const { useModelStatuses, statusKey } = await import("@/store-model-statuses");
const { useUi } = await import("@/store-ui");

function project(overrides: Partial<Project> = {}): Project {
  return {
    projectId: "p1",
    name: "demo",
    workdir: "C:\\code\\demo",
    source: null,
    model: null,
    effort: null,
    permMode: "default",
    createdAt: 1,
    isGit: true,
    ...overrides,
  };
}

const catalogEntries: CatalogEntry[] = [
  {
    id: "anthropic",
    name: "Anthropic",
    family: "anthropic",
    color: "#D97757",
    initial: "A",
    category: "api_key",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-opus-4", "claude-sonnet-4"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
];

const anthropicConnection: ConnectionInfo = {
  id: "conn-1",
  provider: "anthropic",
  providerName: "Anthropic",
  color: "#D97757",
  initial: "A",
  authType: "apiKey",
  label: "Anthropic",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["claude-opus-4", "claude-sonnet-4"],
  keyMasked: "sk-…3fk9",
  needsRelogin: false,
  claudeCloaking: false,
};

beforeEach(() => {
  useStore.setState({ projects: [project()], selectedProjectId: "p1" });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({ catalog: catalogEntries, connections: [anthropicConnection], loaded: true });
  useAgent.setState({ models: ["anthropic/claude-opus-4", "anthropic/claude-sonnet-4"], model: null, permMode: "ask" });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
  useNav.setState({ composerBranch: null, composerModel: null });
  listBranches.mockClear();
  nativeCommands.mockClear();
});

// Reset the shared zustand singletons so later test files in the same bun
// process don't inherit this file's fixtures (mirrors ModelPicker.test.tsx).
afterEach(() => {
  cleanup();
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useAgent.setState({ models: [], model: null, permMode: null });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
});

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
  // clearDraft mutates the shared useNav store synchronously while HomeView
  // (and the branch Combobox, which also reads useNav) are still mounted;
  // without act() that update is applied outside any act scope and React
  // warns for every subscriber it re-renders.
  act(() => {
    useNav.getState().clearDraft("home:p1");
  });
});

test("composer model chip: shared ModelPicker look — no Ryuzi suffix, search always available", async () => {
  render(<HomeView />);
  const chip = screen.getByRole("combobox", { name: "Model" });
  expect(chip.textContent).toContain("Default model");
  expect(chip.textContent).not.toContain("Ryuzi");
  fireEvent.click(chip);
  expect(await screen.findByPlaceholderText("Search…")).toBeTruthy();
  expect(screen.getByRole("option", { name: "claude-opus-4" })).toBeTruthy();
});

test("hide-invalid filters the composer model list, keeping untested models", async () => {
  useModelStatuses.setState({ byKey: { [statusKey("anthropic", "claude-sonnet-4")]: "invalid" } });
  useUi.setState({ hideInvalidModels: true });
  render(<HomeView />);
  fireEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(await screen.findByRole("option", { name: "claude-opus-4" })).toBeTruthy();
  expect(screen.queryByRole("option", { name: "claude-sonnet-4" })).toBeNull();
});
