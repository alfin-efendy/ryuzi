import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentSummaryInfo, CmdError, OpenTarget, Project, Result, Session } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

// --- @/bindings: only the commands actually reachable from the mount paths
// exercised below need a real implementation; everything else stays absent
// so an accidental new call fails loudly instead of silently no-op'ing.
const openInTargets: OpenTarget[] = [{ id: "vscode", name: "VS Code" }];
const listOpenTargets = mock(() => Promise.resolve(openInTargets));
const openIn = mock((): Promise<Result<null, CmdError>> => Promise.resolve({ status: "ok", data: null }));
const sessionWorkdir = mock(
  (_runnerId: string, _sessionPk: string): Promise<Result<string, CmdError>> => Promise.resolve({ status: "ok", data: "C:\\code\\demo" }),
);
const nativeCommands = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
// TodoPanel (always mounted by SessionView) fires this on mount — stubbed ok:[]
// so its effect resolves cleanly; TodoPanel itself renders null for an empty list.
const sessionTodos = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
// loadProjectRuntime (always fired on mount for a project-bound session) fires
// this too — stubbed with a minimal valid ProjectRuntimeInfo so its effect
// resolves cleanly; the model/effort UI isn't under test here.
const projectRuntimeInfo = mock(() =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      projectId: "p1",
      model: null,
      storedEffort: null,
      effectiveEffort: null,
      effectiveEffortLabel: null,
      effectiveSource: "none" as const,
      storedEffortStatus: "valid" as const,
      modelInfo: null,
    },
  }),
);

// `commands.fetchAttachment` isn't reachable from any mount path exercised
// below (no test here renders an image attachment) — it's stubbed anyway
// because `mock.module("@/bindings", ...)` replaces the module for the whole
// `bun test` process, not just this file: the real (unmocked) `Transcript`
// component other test files render (e.g. ModalShells.test.tsx) resolves
// `commands` through this same live binding, so an absent `fetchAttachment`
// here would break ITS attachment-preview test instead of ours.
const fetchAttachment = mock(() => Promise.resolve({ status: "ok" as const, data: { dataBase64: "", contentType: null } }));
const continueSession = mock(() => Promise.resolve({ status: "ok" as const, data: null }));
const listProjects = mock(() => Promise.resolve({ status: "ok" as const, data: [] as Project[] }));
const listSessions = mock(() => Promise.resolve({ status: "ok" as const, data: [] as Session[] }));
const listGateways = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
const searchFiles = mock(() => Promise.resolve({ status: "ok" as const, data: [] as string[] }));

mock.module("@/bindings", () => ({
  commands: {
    listOpenTargets,
    openIn,
    sessionWorkdir,
    nativeCommands,
    sessionTodos,
    projectRuntimeInfo,
    fetchAttachment,
    continueSession,
    listProjects,
    listSessions,
    listGateways,
    searchFiles,
    // SessionView's orch task-strip effect (Phase 5) calls this on a chat
    // session; stub an empty result so no strip mounts and it doesn't throw.
    orchListRoots: async () => ({ status: "ok" as const, data: [] }),
  },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
// useComposerAttachments registers a Tauri drag-drop listener on mount (see HomeView.test.tsx).
mock.module("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: () => Promise.resolve(() => {}) }),
}));

// Deliberately NOT stubbing Transcript/RightPanel/TodoPanel here: bun's
// mock.module() replaces a module for the whole test run (all files share one
// process), and all three now have their own dedicated *.test.tsx that import
// the real component (Transcript's is ModalShells.test.tsx) — stubbing them
// here would silently break those other files. RightPanel is safe to leave
// real because nav.rightOpen defaults false, so it never mounts in these
// tests anyway; TodoPanel is handled via the sessionTodos stub above;
// Transcript never renders any rows in these fixtures (`transcripts: {}`), so
// its markdown-rendering cost never actually pays off here either.

// Stand-in for the real drawer (which pulls in xterm + store-terms) — a spy so
// the test can assert whether the PTY drawer mounts at all, which is the
// load-bearing behavior for this task (P4-4). No other test file imports the
// real BottomTerminalDrawer, so mocking it here is safe.
const drawerMounts: Array<{ runnerId: string; sessionPk: string }> = [];
mock.module("@/components/session/BottomTerminalDrawer", () => ({
  BottomTerminalDrawer: (props: { runnerId: string; sessionPk: string }) => {
    drawerMounts.push({ runnerId: props.runnerId, sessionPk: props.sessionPk });
    return <div data-testid="bottom-terminal-drawer" />;
  },
}));

const { SessionView } = await import("./SessionView");
const { useStore } = await import("@/store");
const { useNav } = await import("@/store-nav");
const { useAgents } = await import("@/store-agents");
const { useConnections } = await import("@/store-connections");

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

function session(runnerId: string, overrides: Partial<Session> = {}): Session & { runnerId: string } {
  return {
    runnerId,
    sessionPk: "s1",
    primaryAgentId: null,
    primaryAgentSnapshot: null,
    projectId: "p1",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: "demo session",
    status: "idle",
    permMode: "default",
    startedBy: "cockpit",
    createdAt: 1,
    lastActive: 1,
    resumeAttempts: 0,
    branchOwned: false,
    kind: "project",
    speaker: null,
    agent: null,
    parentSessionPk: null,
    ...overrides,
  };
}

function primary(id: string, executable = true): AgentSummaryInfo {
  return {
    id,
    name: "Renamed profile",
    description: "",
    avatarColor: "blue",
    model: { kind: "route", route: "smart" },
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable,
    validation: [],
    isDefault: false,
  };
}

function seed(runnerId: string, sessionOverrides: Partial<Session> = {}, agents: AgentSummaryInfo[] = []) {
  useStore.setState({
    sessions: [session(runnerId, sessionOverrides)],
    projects: [project()],
    focusedSession: { runnerId, pk: "s1" },
    transcripts: {},
    pendingApprovals: [],
  });
  useAgents.setState({ registry: { agents, defaultAgentId: agents[0]?.id ?? "none", recovery: [], subagentModel: { kind: "route", route: "fast" } } });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({ loaded: true });
}

beforeEach(() => {
  drawerMounts.length = 0;
  listOpenTargets.mockClear();
  continueSession.mockClear();
  listProjects.mockClear();
  listSessions.mockClear();
  listGateways.mockClear();
  searchFiles.mockClear();
});

afterEach(() => {
  cleanup();
  useNav.setState({ drafts: {}, bottomOpen: false, rightOpen: false });
  useStore.setState({ sessions: [], projects: [], focusedSession: null, transcripts: {}, pendingApprovals: [] });
  useAgents.setState({ registry: null, models: [] });
  useConnections.setState({ loaded: false, catalog: [], connections: [] });
});

test("normal sessions render their passed approval card only once", async () => {
  seed(LOCAL_RUNNER);
  useStore.setState({
    pendingApprovals: [
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s1",
        runId: "main-run",
        requestId: "main-approval",
        tool: "bash",
        summary: "run the main command",
        kind: "tool",
        input: { command: "printf normal-session-approval" },
        principal: null,
      },
    ],
  });

  render(<SessionView />);

  expect(await screen.findAllByText("printf normal-session-approval")).toHaveLength(1);
});

test("immutable primary snapshot labels the session despite profile edits", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "reviewer", primaryAgentSnapshot: { id: "reviewer", name: "Original reviewer", avatarColor: "violet" } },
    [primary("reviewer")],
  );
  render(<SessionView />);

  expect(await screen.findByText("Original reviewer")).toBeTruthy();
  expect(screen.queryByText("Renamed profile")).toBeNull();
  expect((screen.getByPlaceholderText("Ask for follow-up changes") as HTMLTextAreaElement).disabled).toBe(false);
});

test("session composer sends raw leading whitespace and its structured mention span", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } },
    [primary("primary"), { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" }],
  );
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "  @a review", selectionStart: 4 } });
  expect(screen.getByRole("menu")).toBeTruthy();
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("  @Ada  review");
  fireEvent.keyDown(composer, { key: "Enter" });

  await waitFor(() =>
    expect(continueSession).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "s1",
      expect.objectContaining({
        text: "  @Ada  review",
        mentions: [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 2, endUtf16: 6 }],
      }),
    ),
  );
});

test("session mention metadata stays with its draft when switching sessions", async () => {
  const s2 = session(LOCAL_RUNNER, {
    sessionPk: "s2",
    primaryAgentId: "primary",
    primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" },
  });
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } },
    [primary("primary"), { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" }],
  );
  useStore.setState({ sessions: [...useStore.getState().sessions, s2] });
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "@a keep", selectionStart: 2 } });
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("@Ada  keep");

  act(() => useStore.setState({ focusedSession: { runnerId: LOCAL_RUNNER, pk: "s2" } }));
  fireEvent.change(composer, { target: { value: "plain s2" } });
  act(() => useStore.setState({ focusedSession: { runnerId: LOCAL_RUNNER, pk: "s1" } }));
  expect(composer.value).toBe("@Ada  keep");
  fireEvent.keyDown(composer, { key: "Enter" });

  await waitFor(() =>
    expect(continueSession).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "s1",
      expect.objectContaining({ mentions: [{ agentId: "ada", labelSnapshot: "Ada", startUtf16: 0, endUtf16: 4 }] }),
    ),
  );
});

test("session textarea Escape closes the agent mention popup", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } },
    [primary("primary"), { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" }],
  );
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });
  expect(screen.getByRole("menu")).toBeTruthy();
  fireEvent.keyDown(composer, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
});

test("session plain agent @ mentions open the agent menu instead of searching context", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } },
    [primary("primary"), { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" }],
  );
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });

  expect(screen.getByRole("menu").textContent).toContain("Agents");
  expect(searchFiles).not.toHaveBeenCalled();
});

test("session composer selects an agent mention from its keyboard menu", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } },
    [primary("primary"), { ...primary("ada"), name: "Ada" }],
  );
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "ask @a", selectionStart: 6 } });
  expect(screen.getByRole("menu")).toBeTruthy();

  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("ask @Ada ");
});

test("legacy sessions stay read-only without a repair destination", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: null, primaryAgentSnapshot: null });
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Legacy sessions are read-only.")) as HTMLTextAreaElement;
  expect(composer.disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.queryByRole("button", { name: "Repair agent" })).toBeNull();
});

test("a deleted primary makes a captured session read-only without repair", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "deleted", primaryAgentSnapshot: { id: "deleted", name: "Deleted", avatarColor: "rose" } });
  render(<SessionView />);

  expect(await screen.findByText("The session’s primary agent was deleted, so this session is read-only.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Repair agent" })).toBeNull();
});

test("a nonexecutable primary offers repair navigation", async () => {
  seed(
    LOCAL_RUNNER,
    { primaryAgentId: "reviewer", primaryAgentSnapshot: { id: "reviewer", name: "Reviewer", avatarColor: "violet" } },
    [primary("reviewer", false)],
  );
  render(<SessionView />);

  fireEvent.click(await screen.findByRole("button", { name: "Repair agent" }));
  expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "reviewer" });
});

test("local session with the bottom panel open: terminal drawer mounts and both controls are enabled", async () => {
  useNav.setState({ bottomOpen: true });
  seed(LOCAL_RUNNER);
  render(<SessionView />);

  expect(await screen.findByTestId("bottom-terminal-drawer")).toBeTruthy();
  // SessionView re-renders a few times as its mount effects settle (workdir,
  // native commands, ...) — each re-render re-invokes the spy, so assert on
  // the props it was given rather than an exact call count.
  expect(drawerMounts.length).toBeGreaterThan(0);
  expect(drawerMounts.every((m) => m.runnerId === LOCAL_RUNNER && m.sessionPk === "s1")).toBe(true);

  const toggleBtn = screen.getByRole("button", { name: "Toggle bottom panel" }) as HTMLButtonElement;
  expect(toggleBtn.hasAttribute("disabled")).toBe(false);

  const openInBtn = await screen.findByRole("button", { name: "Open in…" });
  expect(openInBtn.hasAttribute("disabled")).toBe(false);
  expect(listOpenTargets).toHaveBeenCalledTimes(1);
});

// This is the load-bearing case (see SessionView.tsx's render-guard comment):
// nav.bottomOpen is a single global/persisted flag also toggled from
// TitleBar, so it can very well already be true when the user switches INTO
// a remote session — the render guard (not just disabling the toggle button)
// is what stops a remote PTY from auto-spawning in that situation.
test("remote session, even with the bottom panel already globally open: terminal drawer never mounts and both controls are disabled", async () => {
  useNav.setState({ bottomOpen: true });
  seed("gw-1");
  render(<SessionView />);

  // findByRole (not getByRole) so pending mount-effect state updates (workdir,
  // native commands, ...) settle under act() before we assert, avoiding
  // "not wrapped in act" console noise from updates landing after this test.
  const toggleBtn = (await screen.findByRole("button", { name: "Toggle bottom panel" })) as HTMLButtonElement;
  expect(screen.queryByTestId("bottom-terminal-drawer")).toBeNull();
  expect(drawerMounts).toEqual([]);

  expect(toggleBtn.hasAttribute("disabled")).toBe(true);
  expect(toggleBtn.closest("span")?.getAttribute("title")).toBe("Not available for sessions on a remote runner");

  const openInBtn = screen.getByRole("button", { name: "Open in…" }) as HTMLButtonElement;
  expect(openInBtn.hasAttribute("disabled")).toBe(true);
  expect(openInBtn.closest("span")?.getAttribute("title")).toBe("Not available for sessions on a remote runner");
  expect(listOpenTargets).not.toHaveBeenCalled();
});

test("remote session with the bottom panel closed: toggling it stays a no-op (disabled) instead of opening the drawer", async () => {
  useNav.setState({ bottomOpen: false });
  seed("gw-1");
  render(<SessionView />);

  const toggleBtn = await screen.findByRole("button", { name: "Toggle bottom panel" });
  toggleBtn.click();

  expect(useNav.getState().bottomOpen).toBe(false);
  expect(screen.queryByTestId("bottom-terminal-drawer")).toBeNull();
});
