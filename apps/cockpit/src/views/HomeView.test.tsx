import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type {
  BranchList,
  CatalogEntry,
  ChatContextArg,
  CmdError,
  CommandInfo,
  ConnectionInfo,
  Project,
  ProjectRuntimeInfo,
  Result,
  Session,
} from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const branchListData: BranchList = { branches: ["main", "develop"], current: "main", detached: false };
const listBranches = mock(
  (_runnerId: string, _projectId: string): Promise<Result<BranchList, CmdError>> => Promise.resolve({ status: "ok", data: branchListData }),
);
const nativeCommands = mock(
  (_runnerId: string, _projectId: string): Promise<Result<CommandInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }),
);
const searchFiles = mock((): Promise<Result<string[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const runtimeInfo: ProjectRuntimeInfo = {
  projectId: "p1",
  model: null,
  storedEffort: null,
  effectiveEffort: null,
  effectiveEffortLabel: null,
  effectiveSource: "none",
  storedEffortStatus: "valid",
  modelInfo: null,
};
const projectRuntimeInfo = mock(() => Promise.resolve({ status: "ok" as const, data: runtimeInfo }));
// start() (via useStore.start) calls these three IPC commands — mocked here
// so the permission-picker test can drive the real store's start() and
// inspect what startSession actually received.
const startSession = mock(
  (
    _runnerId: string,
    _projectId: string,
    _primaryAgentId: string,
    _turn: { text: string; context: ChatContextArg | null },
  ): Promise<Result<Session, CmdError>> =>
    Promise.resolve({
      status: "ok",
      data: {
        sessionPk: "s1",
        primaryAgentId: null,
        primaryAgentSnapshot: null,
        projectId: "p1",
        agentSessionId: null,
        worktreePath: null,
        branch: null,
        title: "ship it",
        status: "running",
        permMode: "bypassPermissions",
        startedBy: "cockpit",
        createdAt: 1,
        lastActive: 1,
        resumeAttempts: 0,
        branchOwned: false,
        kind: "project",
        speaker: null,
        agent: null,
        parentSessionPk: null,
      },
    }),
);
const startChatSession = mock(
  (
    _runnerId: string,
    _primaryAgentId: string,
    _turn: { text: string; context: ChatContextArg | null },
  ): Promise<Result<Session, CmdError>> =>
    Promise.resolve({
      status: "ok",
      data: {
        sessionPk: "chat-1",
        primaryAgentId: "ryuzi",
        primaryAgentSnapshot: { id: "ryuzi", name: "Ryuzi", avatarColor: "violet" },
        projectId: null,
        agentSessionId: null,
        worktreePath: null,
        branch: null,
        title: "chat",
        status: "running",
        permMode: "default",
        startedBy: "cockpit",
        createdAt: 1,
        lastActive: 1,
        resumeAttempts: 0,
        branchOwned: false,
        kind: "chat",
        speaker: null,
        agent: null,
        parentSessionPk: null,
      },
    }),
);

const listProjects = mock((): Promise<Result<Project[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const listSessions = mock((): Promise<Result<Session[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
// refresh() (fire-and-forget after a successful start()) always fans out to
// listGateways too — unmocked it rejects.
const listGateways = mock((): Promise<Result<never[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));

mock.module("@/bindings", () => ({
  commands: {
    listBranches,
    nativeCommands,
    searchFiles,
    projectRuntimeInfo,
    startChatSession,
    startSession,
    listProjects,
    listSessions,
    listGateways,
  },
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
const defaultHydrateConnections = useConnections.getState().hydrate;
const { useAgents } = await import("@/store-agents");
const { useModelStatuses } = await import("@/store-model-statuses");
const { useUi } = await import("@/store-ui");
const { useNative } = await import("@/store-native");

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

const selectable = (requestValue: string) => ({
  kind: "concrete" as const,
  requestValue,
  displayName: requestValue.split("/").pop() ?? requestValue,
  preferenceKey: null,
  supported: [],
  configuredDefault: null,
  resolvedDefault: null,
  defaultSource: "none" as const,
});

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
  quotaCapability: null,
  models: ["claude-opus-4", "claude-sonnet-4"],
  needsRelogin: false,
};

beforeEach(() => {
  useStore.setState({ projects: [project()], selectedProjectId: "p1", projectRuntimeById: { p1: runtimeInfo } });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({
    catalog: catalogEntries,
    connections: [anthropicConnection],
    loaded: true,
    hydrate: defaultHydrateConnections,
  });
  useAgents.setState({
    registry: {
      agents: [
        {
          id: "ryuzi",
          name: "Ryuzi",
          description: "",
          avatarColor: "violet",
          model: { kind: "route", route: "smart" },
          permissionMode: "ask",
          skillCount: 0,
          toolCount: 0,
          knowledgeCount: 0,
          executable: true,
          validation: [],
          isDefault: true,
        },
      ],
      defaultAgentId: "ryuzi",
      recovery: [],
      subagentModel: { kind: "route", route: "fast" },
    },
    models: [selectable("anthropic/claude-opus-4"), selectable("anthropic/claude-sonnet-4")],
  });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
  useNative.setState({ commandsByProject: {} });
  useNav.setState({ composerBranch: null });
  listBranches.mockClear();
  nativeCommands.mockClear();
  searchFiles.mockClear();
  startSession.mockClear();
  startChatSession.mockClear();
  listProjects.mockClear();
  listSessions.mockClear();
  listGateways.mockClear();
});

// Reset the shared zustand singletons so later test files in the same bun
// process don't inherit this file's fixtures (mirrors ModelPicker.test.tsx).
afterEach(() => {
  cleanup();
  useStore.setState({ projects: [], selectedProjectId: null, projectRuntimeById: {} });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useAgents.setState({ registry: null, models: [] });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
  useNative.setState({ commandsByProject: {} });
});

test("git project: branch pill shows and branches are fetched", async () => {
  render(<HomeView />);
  // The Combobox trigger renders the resolved current branch. The trigger's
  // ARIA role is "combobox" (Base UI) with its accessible name taken from
  // aria-label="Branch"; the visible branch name lives in its text content.
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Branch" }).textContent).toContain("main"));
  expect(listBranches).toHaveBeenCalledWith(LOCAL_RUNNER, "p1");
});

test("hydrates provider connections when they have not loaded", async () => {
  const hydrate = mock(async () => {});
  useConnections.setState({ loaded: false, hydrate });

  render(<HomeView />);

  await waitFor(() => expect(hydrate).toHaveBeenCalledTimes(1));
});

test("non-git project: no branch pill, no worktree toggle, no list_branches call", async () => {
  useStore.setState({ projects: [project({ isGit: false })] });
  render(<HomeView />);
  // The whole branch Combobox — trigger pill AND its worktree-Switch footer — is gone.
  expect(screen.queryByRole("combobox", { name: "Branch" })).toBeNull();
  // Let the other mount effect flush so a stray branch fetch would have fired by now.
  await waitFor(() => expect(nativeCommands).toHaveBeenCalledWith(LOCAL_RUNNER, "p1"));
  expect(listBranches).not.toHaveBeenCalled();
});

test("only suggests each effective slash command from the catalog", async () => {
  nativeCommands.mockResolvedValueOnce({
    status: "ok",
    data: [
      {
        name: "ship",
        description: "Project ship",
        agent: null,
        model: null,
        subtask: false,
        origin: "project",
        effective: true,
        shadowsGlobal: true,
      },
      {
        name: "ship",
        description: "Global ship",
        agent: null,
        model: null,
        subtask: false,
        origin: "global",
        effective: false,
        shadowsGlobal: false,
      },
      {
        name: "init",
        description: "Project init",
        agent: null,
        model: null,
        subtask: false,
        origin: "project",
        effective: false,
        shadowsGlobal: true,
      },
      {
        name: "init",
        description: "Global init",
        agent: null,
        model: null,
        subtask: false,
        origin: "global",
        effective: false,
        shadowsGlobal: false,
      },
      {
        name: "init",
        description: "Built-in init",
        agent: null,
        model: null,
        subtask: false,
        origin: "builtin",
        effective: true,
        shadowsGlobal: false,
      },
    ],
  });
  render(<HomeView />);

  fireEvent.change(screen.getByPlaceholderText("Do anything"), { target: { value: "/" } });

  expect(await screen.findByText("Project ship")).toBeTruthy();
  expect(screen.getAllByText("/ship")).toHaveLength(1);
  expect(screen.queryByText("Global ship")).toBeNull();
  expect(await screen.findByText("Built-in init")).toBeTruthy();
  expect(screen.getAllByText("/init")).toHaveLength(1);
  expect(screen.queryByText("Project init")).toBeNull();
  expect(screen.queryByText("Global init")).toBeNull();
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

test("sends raw leading whitespace and its structured mention span", async () => {
  useAgents.setState({
    registry: {
      ...useAgents.getState().registry!,
      agents: [
        ...useAgents.getState().registry!.agents,
        {
          ...useAgents.getState().registry!.agents[0],
          id: "ada",
          name: "Ada",
          description: "Accessibility reviewer",
          isDefault: false,
        },
      ],
    },
  });
  render(<HomeView />);
  await waitFor(() => expect(useNav.getState().composerBranch).toBe("main"));

  const composer = screen.getByPlaceholderText("Do anything") as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "  @a review", selectionStart: 4 } });
  expect(screen.getByRole("menu")).toBeTruthy();
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("  @Ada  review");

  fireEvent.keyDown(composer, { key: "Enter" });
  await waitFor(() =>
    expect(startSession).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "p1",
      "ryuzi",
      expect.objectContaining({
        text: "  @Ada  review",
        mentions: [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 2, endUtf16: 6 }],
      }),
    ),
  );
});

test("keeps structured mentions with their home draft when projects switch", async () => {
  useStore.setState({ projects: [project(), project({ projectId: "p2", name: "other" })] });
  useAgents.setState({
    registry: {
      ...useAgents.getState().registry!,
      agents: [
        ...useAgents.getState().registry!.agents,
        { ...useAgents.getState().registry!.agents[0], id: "ada", name: "Ada", description: "Accessibility reviewer", isDefault: false },
      ],
    },
  });
  render(<HomeView />);

  const composer = screen.getByPlaceholderText("Do anything") as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "@a keep", selectionStart: 2 } });
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("@Ada  keep");

  act(() => useStore.setState({ selectedProjectId: "p2" }));
  fireEvent.change(composer, { target: { value: "plain p2" } });
  act(() => useStore.setState({ selectedProjectId: "p1" }));
  expect(composer.value).toBe("@Ada  keep");

  fireEvent.keyDown(composer, { key: "Enter" });
  await waitFor(() =>
    expect(startSession).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "p1",
      "ryuzi",
      expect.objectContaining({ mentions: [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 0, endUtf16: 4 }] }),
    ),
  );
});

test("textarea Escape closes the agent mention popup", () => {
  useStore.setState({ selectedProjectId: null });
  localStorage.removeItem("cockpit.lastPrimaryAgentId");
  useAgents.setState({
    registry: {
      ...useAgents.getState().registry!,
      agents: [
        ...useAgents.getState().registry!.agents,
        { ...useAgents.getState().registry!.agents[0], id: "ada", name: "Ada", description: "Accessibility reviewer", isDefault: false },
      ],
    },
  });
  render(<HomeView />);

  const composer = screen.getByPlaceholderText("Do anything");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });
  expect(screen.getByRole("menu")).toBeTruthy();
  fireEvent.keyDown(composer, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
});

test("plain agent @ mentions open the agent menu instead of searching context", () => {
  useAgents.setState({
    registry: {
      ...useAgents.getState().registry!,
      agents: [
        ...useAgents.getState().registry!.agents,
        { ...useAgents.getState().registry!.agents[0], id: "ada", name: "Ada", description: "Accessibility reviewer", isDefault: false },
      ],
    },
  });
  render(<HomeView />);

  const composer = screen.getByPlaceholderText("Do anything");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });

  expect(screen.getByRole("menu").textContent).toContain("Agents");
  expect(searchFiles).not.toHaveBeenCalled();
});

test("selects the default primary and forwards a complete first project TurnInput", async () => {
  render(<HomeView />);
  await waitFor(() => expect(useNav.getState().composerBranch).toBe("main"));

  const box = screen.getByPlaceholderText("Do anything") as HTMLTextAreaElement;
  fireEvent.change(box, { target: { value: "ship it" } });
  fireEvent.keyDown(box, { key: "Enter" });

  await waitFor(() =>
    expect(startSession).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "ryuzi", {
      text: "ship it",
      mentions: [],
      context: { branch: "main", voiceTranscript: null, references: [] },
      attachments: [],
      git: { useWorktree: true, createBranch: true, branchName: null, baseBranch: null },
    }),
  );
  expect(localStorage.getItem("cockpit.lastPrimaryAgentId")).toBe("ryuzi");
});

test("prefers a pending primary over persisted and default choices", async () => {
  useNav.setState({ pendingPrimaryAgentId: "reviewer" });
  localStorage.setItem("cockpit.lastPrimaryAgentId", "stored");
  useAgents.setState({
    registry: {
      agents: [
        { ...useAgents.getState().registry!.agents[0], id: "ryuzi", isDefault: true },
        { ...useAgents.getState().registry!.agents[0], id: "stored", isDefault: false },
        { ...useAgents.getState().registry!.agents[0], id: "reviewer", isDefault: false },
      ],
      defaultAgentId: "ryuzi",
      recovery: [],
      subagentModel: { kind: "route", route: "fast" },
    },
  });
  render(<HomeView />);
  await waitFor(() => expect(useNav.getState().composerBranch).toBe("main"));
  fireEvent.change(screen.getByPlaceholderText("Do anything"), { target: { value: "assigned" } });
  fireEvent.click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() => expect(startSession).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "reviewer", expect.anything()));
  expect(useNav.getState().pendingPrimaryAgentId).toBeNull();
  expect(localStorage.getItem("cockpit.lastPrimaryAgentId")).toBe("reviewer");
});

test("uses a persisted executable primary when no pending choice exists", async () => {
  localStorage.setItem("cockpit.lastPrimaryAgentId", "stored");
  useAgents.setState({
    registry: {
      agents: [
        { ...useAgents.getState().registry!.agents[0], id: "ryuzi", isDefault: true },
        { ...useAgents.getState().registry!.agents[0], id: "stored", isDefault: false },
      ],
      defaultAgentId: "ryuzi",
      recovery: [],
      subagentModel: { kind: "route", route: "fast" },
    },
  });
  render(<HomeView />);
  await waitFor(() => expect(useNav.getState().composerBranch).toBe("main"));
  fireEvent.change(screen.getByPlaceholderText("Do anything"), { target: { value: "resume" } });
  fireEvent.click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() => expect(startSession).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "stored", expect.anything()));
});

test("disables the composer and navigates to agent repair when no executable primary exists", () => {
  useAgents.setState({
    registry: {
      agents: [{ ...useAgents.getState().registry!.agents[0], executable: false }],
      defaultAgentId: "ryuzi",
      recovery: [],
      subagentModel: { kind: "route", route: "fast" },
    },
  });
  useNav.setState({ drafts: { "home:p1": "keep this" } });
  render(<HomeView />);

  const box = screen.getByPlaceholderText("Do anything") as HTMLTextAreaElement;
  expect(box.disabled).toBe(true);
  expect(box.value).toBe("keep this");
  expect(screen.getByRole("button", { name: "Start session" }).hasAttribute("disabled")).toBe(true);

  fireEvent.click(screen.getByRole("button", { name: "Repair agents" }));
  expect(useNav.getState().view()).toEqual({ kind: "agents" });
  expect(startSession).not.toHaveBeenCalled();
});

test("starts a chat without a project using the selected primary", async () => {
  useStore.setState({ selectedProjectId: null });
  render(<HomeView />);
  fireEvent.change(screen.getByPlaceholderText("Do anything"), { target: { value: "chat" } });
  fireEvent.click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() =>
    expect(startChatSession).toHaveBeenCalledWith(LOCAL_RUNNER, "ryuzi", {
      text: "chat",
      mentions: [],
      context: { branch: null, voiceTranscript: null, references: [] },
      attachments: [],
      git: null,
    }),
  );
});

test("preserves project, branch, context, voice, and attachment controls while model controls stay removed", () => {
  render(<HomeView />);
  expect(screen.getByRole("combobox", { name: "Project" })).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Branch" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Attach" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Voice" })).toBeTruthy();
  expect(screen.queryByRole("combobox", { name: "Permission mode" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Model and effort" })).toBeNull();
});
