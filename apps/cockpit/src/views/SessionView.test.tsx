import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentSummaryInfo, CmdError, CommandInfo, OpenTarget, Project, Result, SearchEntryInfo, Session } from "@/bindings";
import { LOCAL_RUNNER, refKey, sessKey } from "@/lib/session-key";
import { useNative } from "@/store-native";

// --- @/bindings: only the commands actually reachable from the mount paths
// exercised below need a real implementation; everything else stays absent
// so an accidental new call fails loudly instead of silently no-op'ing.
const openInTargets: OpenTarget[] = [{ id: "vscode", name: "VS Code" }];
const listOpenTargets = mock(() => Promise.resolve(openInTargets));
const openIn = mock((): Promise<Result<null, CmdError>> => Promise.resolve({ status: "ok", data: null }));
const sessionWorkdir = mock(
  (_runnerId: string, _sessionPk: string): Promise<Result<string, CmdError>> => Promise.resolve({ status: "ok", data: "C:\\code\\demo" }),
);
const nativeCommands = mock((): Promise<Result<CommandInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
// TodoPanel (always mounted by SessionView) fires this on mount — stubbed ok:[]
// so its effect resolves cleanly; TodoPanel itself renders null for an empty list.
const sessionTodos = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
const sessionQueue = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
const continueSession = mock(() => Promise.resolve({ status: "ok" as const, data: null }));
const enqueueSessionMessage = mock<
  (
    ...args: Parameters<typeof import("@/bindings").commands["enqueueSessionMessage"]>
  ) => ReturnType<typeof import("@/bindings").commands["enqueueSessionMessage"]>
>(() => Promise.resolve({ status: "ok" as const, data: { id: "q1", text: "queued" } }));
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
const listProjects = mock(() => Promise.resolve({ status: "ok" as const, data: [] as Project[] }));
const listSessions = mock(() => Promise.resolve({ status: "ok" as const, data: [] as Session[] }));
const listGateways = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
// Default fixture for the unified `@` context picker's debounced workspace
// search: one matching folder, one matching file, both containing "session"
// so `@session` queries below exercise a real (non-empty) Folders/Files
// match without colliding with the seeded project name ("demo") or agent
// ("Ada"). `beforeEach` reinstates this default implementation after every
// test via `mockReset` + `mockImplementation` so a test that overrides it
// (e.g. with `mockResolvedValueOnce`) never leaks into a later one.
const DEFAULT_SEARCH_ENTRIES: SearchEntryInfo[] = [
  { path: "src/session", dir: true },
  { path: "src/views/SessionView.tsx", dir: false },
];
const searchFiles = mock<
  (
    ...args: Parameters<typeof import("@/bindings").commands["searchFiles"]>
  ) => ReturnType<typeof import("@/bindings").commands["searchFiles"]>
>(() => Promise.resolve({ status: "ok" as const, data: DEFAULT_SEARCH_ENTRIES }));
const getChildRuns = mock(() =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      rootRunId: "root-run",
      runs: [
        {
          runId: "child-run",
          sessionPk: "s1",
          parentRunId: "root-run",
          retryOf: null,
          sourceToolCallId: "dispatch-1",
          dispatchIndex: 0,
          primaryAgentId: "primary",
          executingAgentId: "worker",
          executingAgentNameSnapshot: "Researcher",
          agentKind: "subagent" as const,
          task: "Inspect hydrated dispatch",
          status: "completed" as const,
          startedAt: 1,
          finishedAt: 2,
          toolCount: 1,
          resolvedModel: null,
          resolvedEffort: null,
          result: "Hydrated result",
          error: null,
        },
      ],
    },
  }),
);
const getChildTranscript = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));

mock.module("@/bindings", () => ({
  commands: {
    listOpenTargets,
    openIn,
    sessionWorkdir,
    nativeCommands,
    sessionTodos,
    sessionQueue,
    continueSession,
    enqueueSessionMessage,
    removeSessionMessage: async () => ({ status: "ok" as const, data: true }),
    projectRuntimeInfo,
    fetchAttachment,
    listProjects,
    listSessions,
    listGateways,
    searchFiles,
    getChildRuns,
    getChildTranscript,
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
const { useDelegation } = await import("@/store-delegation");
const realSend = useStore.getState().send;

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

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
    // Default to an executable owner: most tests exercise the normal (owned)
    // composer/panel/queue path. Explicit legacy/deleted/nonexecutable tests
    // override `primaryAgentId`/`primaryAgentSnapshot` themselves.
    primaryAgentId: "primary",
    primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" },
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
    model: { kind: "route", route: "free" },
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable,
    validation: [],
    isDefault: false,
  };
}

function seed(runnerId: string, sessionOverrides: Partial<Session> = {}, agents: AgentSummaryInfo[] = [primary("primary")]) {
  useStore.setState({
    sessions: [session(runnerId, sessionOverrides)],
    projects: [project()],
    focusedSession: { runnerId, pk: "s1" },
    transcripts: {},
    pendingApprovals: [],
  });
  useAgents.setState({
    registry: { agents, defaultAgentId: agents[0]?.id ?? "none", recovery: [], subagentModel: { kind: "route", route: "free" } },
  });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({ loaded: true });
  useNative.setState({ commandsByProject: {}, queuedBySession: {} });
}

beforeEach(() => {
  drawerMounts.length = 0;
  listOpenTargets.mockClear();
  // `mockClear()` only resets call history; a prior test's
  // `mockRejectedValueOnce`/`mockResolvedValue` override otherwise leaks
  // into later tests sharing this same module-level mock.
  continueSession.mockReset();
  continueSession.mockImplementation(() => Promise.resolve({ status: "ok" as const, data: null }));
  enqueueSessionMessage.mockReset();
  enqueueSessionMessage.mockImplementation(() => Promise.resolve({ status: "ok" as const, data: { id: "q1", text: "queued" } }));
  listProjects.mockClear();
  listSessions.mockClear();
  listGateways.mockClear();
  searchFiles.mockReset();
  searchFiles.mockImplementation(() => Promise.resolve({ status: "ok" as const, data: DEFAULT_SEARCH_ENTRIES }));
  getChildRuns.mockClear();
  getChildTranscript.mockClear();
});

afterEach(() => {
  cleanup();
  useNav.setState({ drafts: {}, bottomOpen: false, rightOpen: false });
  useStore.setState({
    sessions: [],
    projects: [],
    focusedSession: null,
    transcripts: {},
    pendingApprovals: [],
    // A test that stubs `send` via `useStore.setState({ send })` otherwise
    // leaks that mock into every later test in this shared store singleton.
    send: realSend,
  });
  useAgents.setState({ registry: null, models: [] });
  useConnections.setState({ loaded: false, catalog: [], connections: [] });
  useNative.setState({ commandsByProject: {}, queuedBySession: {} });
  useDelegation.setState({
    bySession: {},
    rootRunBySession: {},
    rosterStateBySession: {},
    transcriptByRun: {},
    transcriptStateByRun: {},
    seenRunsByDispatch: {},
    selectedBySession: {},
  });
});

test("hydrates the delegation roster on session mount and gives main transcript cards the durable root owner", async () => {
  seed(LOCAL_RUNNER);
  useStore.setState({
    transcripts: {
      [sessKey(LOCAL_RUNNER, "s1")]: [
        {
          seq: 1,
          role: "assistant",
          blockType: "tool_call",
          text: "",
          toolCallId: "dispatch-1",
          toolStatus: "completed",
          toolKind: "task",
          toolName: "task",
          toolOutput: "ordinary output",
          createdAt: 1,
          attachments: [],
          toolPath: null,
          toolInput: { prompt: "Inspect" },
          toolDurationMs: null,
          toolExitCode: null,
          toolSummary: null,
          toolSubagent: null,
        },
      ],
    },
  });

  render(<SessionView />);

  await waitFor(() => expect(getChildRuns).toHaveBeenCalledWith(LOCAL_RUNNER, "s1"));
  expect(await screen.findByRole("button", { name: /Open Researcher agent run/i })).toBeTruthy();
  expect(screen.getByText("Hydrated result")).toBeTruthy();
});

test("resolves each primary turn dispatch against that row's durable owner", async () => {
  seed(LOCAL_RUNNER);
  getChildRuns.mockResolvedValueOnce({
    status: "ok" as const,
    data: {
      rootRunId: "first-primary",
      runs: [
        {
          runId: "first-child",
          sessionPk: "s1",
          parentRunId: "first-primary",
          retryOf: null,
          sourceToolCallId: "first-dispatch",
          dispatchIndex: 0,
          primaryAgentId: "primary",
          executingAgentId: "researcher",
          executingAgentNameSnapshot: "Researcher",
          agentKind: "subagent" as const,
          task: "Inspect the first turn",
          status: "completed" as const,
          startedAt: 1,
          finishedAt: 2,
          toolCount: 1,
          resolvedModel: null,
          resolvedEffort: null,
          result: "First turn result",
          error: null,
        },
        {
          runId: "second-child",
          sessionPk: "s1",
          parentRunId: "second-primary",
          retryOf: null,
          sourceToolCallId: "second-dispatch",
          dispatchIndex: 0,
          primaryAgentId: "primary",
          executingAgentId: "verifier",
          executingAgentNameSnapshot: "Verifier",
          agentKind: "subagent" as const,
          task: "Verify the later turn",
          status: "completed" as const,
          startedAt: 3,
          finishedAt: 4,
          toolCount: 1,
          resolvedModel: null,
          resolvedEffort: null,
          result: "Second turn result",
          error: null,
        },
      ],
    },
  });
  useStore.setState({
    transcripts: {
      [sessKey(LOCAL_RUNNER, "s1")]: [
        {
          seq: 1,
          role: "assistant",
          blockType: "tool_call",
          text: "",
          ownerRunId: "first-primary",
          toolCallId: "first-dispatch",
          toolStatus: "completed",
          toolKind: "task",
          toolName: "task",
          toolOutput: "first ordinary output",
          createdAt: 1,
          attachments: [],
          toolPath: null,
          toolInput: { prompt: "Inspect" },
          toolDurationMs: null,
          toolExitCode: null,
          toolSummary: null,
          toolSubagent: null,
          toolDispatchFailures: [],
        },
        {
          seq: 2,
          role: "assistant",
          blockType: "tool_call",
          text: "",
          ownerRunId: "second-primary",
          toolCallId: "second-dispatch",
          toolStatus: "completed",
          toolKind: "other",
          toolName: "delegate_agent",
          toolOutput: "second ordinary output",
          createdAt: 3,
          attachments: [],
          toolPath: null,
          toolInput: { task: "Verify" },
          toolDurationMs: null,
          toolExitCode: null,
          toolSummary: null,
          toolSubagent: null,
          toolDispatchFailures: [],
        },
      ],
    },
  });

  render(<SessionView />);

  expect(await screen.findByRole("button", { name: /Open Researcher agent run/i })).toBeTruthy();
  expect(screen.getByRole("button", { name: /Open Verifier agent run/i })).toBeTruthy();
  expect(screen.getByText("First turn result")).toBeTruthy();
  expect(screen.getByText("Second turn result")).toBeTruthy();
  expect(screen.queryByText("second ordinary output")).toBeNull();
});

test("only suggests each effective slash command from the catalog", async () => {
  const catalog: CommandInfo[] = [
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
  ];
  seed(LOCAL_RUNNER);
  nativeCommands.mockResolvedValueOnce({ status: "ok", data: catalog });
  render(<SessionView />);

  fireEvent.change(screen.getByPlaceholderText("Ask for follow-up changes"), { target: { value: "/" } });

  expect(await screen.findByText("Project ship")).toBeTruthy();
  expect(screen.getAllByText("/ship")).toHaveLength(1);
  expect(screen.queryByText("Global ship")).toBeNull();
  expect(await screen.findByText("Built-in init")).toBeTruthy();
  expect(screen.getAllByText("/init")).toHaveLength(1);
  expect(screen.queryByText("Project init")).toBeNull();
  expect(screen.queryByText("Global init")).toBeNull();
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

test("session composer has no orchestration, model/effort, or permission-mode controls", async () => {
  seed(LOCAL_RUNNER);
  render(<SessionView />);

  await screen.findByPlaceholderText("Ask for follow-up changes");
  expect(screen.queryByRole("button", { name: /Orchestrate/i })).toBeNull();
  expect(screen.queryByRole("button", { name: "Model and effort" })).toBeNull();
  expect(screen.queryByRole("combobox", { name: /permission/i })).toBeNull();
});

test("session composer sends raw leading whitespace and its structured mention span", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
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
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
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

test("session `@` opens the unified picker with Project/Agents immediately, then Folders/Files once the debounced search resolves", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@", selectionStart: 1 } });

  // Project and Agents come from local state (project/registry props already
  // in memory) and render on the very first pass, before the 120ms debounced
  // `searchFiles` call has any chance to resolve.
  const menu = screen.getByRole("menu");
  expect(menu.textContent).toContain("Project");
  expect(menu.textContent).toContain("Agents");
  expect(screen.queryByText("Folders")).toBeNull();
  expect(screen.queryByText("Files")).toBeNull();

  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", ""));
  await waitFor(() => expect(screen.getByText("Folders")).toBeTruthy());
  expect(screen.getByText("Files")).toBeTruthy();
});

test("session `@session` matches only Folders/Files (project/agent don't match) and Enter selects the folder token", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "@session", selectionStart: 8 } });

  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "session"));
  await waitFor(() => expect(screen.getByText("Folders")).toBeTruthy());
  expect(screen.getByText("Files")).toBeTruthy();
  // Neither the project ("demo") nor Ada match "session" — their sections
  // never appear in the flattened menu.
  expect(screen.queryByText("Project")).toBeNull();
  expect(screen.queryByText("Agents")).toBeNull();
  expect(screen.queryByText("demo")).toBeNull();
  expect(screen.queryByText("Ada")).toBeNull();

  // Folders sort ahead of Files (`contextPickerGroups`), so the flattened
  // index 0 that a bare Enter picks is the "src/session" folder, not the
  // "SessionView.tsx" file.
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("@src/session ");
});

test("session `@` then Enter selects the Project item (flattened index 0) and its reference doesn't leak into context.references", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "@", selectionStart: 1 } });
  expect(screen.getByRole("menu")).toBeTruthy();

  // Project sorts first (`contextPickerGroups`), so a bare Enter at the
  // default pickerIndex (0) picks it, not an Agent or workspace entry.
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("@demo ");

  // Picking an item replaces the composer's value, but jsdom/happy-dom
  // doesn't synthesize the browser's own caret-to-end move that follows a
  // programmatic value change — a real browser would fire a `select` event
  // there. Without it, the stale caret (still inside the now-replaced `@`
  // token) would make `activeContextQuery` see the old token as still
  // active and reopen the picker instead of letting the next Enter submit.
  fireEvent.select(composer, { target: { selectionStart: composer.value.length } });
  fireEvent.keyDown(composer, { key: "Enter" });

  await waitFor(() =>
    expect(continueSession).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "s1",
      expect.objectContaining({ context: expect.objectContaining({ references: [] }) }),
    ),
  );
});

test("session unified context search error preserves Project/Agents but drops Folders/Files", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  searchFiles.mockImplementation(() => Promise.resolve({ status: "error" as const, error: { message: "search failed" } }));
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@", selectionStart: 1 } });

  const menu = screen.getByRole("menu");
  expect(menu.textContent).toContain("Project");
  expect(menu.textContent).toContain("Agents");

  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", ""));
  expect(screen.queryByText("Folders")).toBeNull();
  expect(screen.queryByText("Files")).toBeNull();
  expect(screen.getByRole("menu").textContent).toContain("Project");
  expect(screen.getByRole("menu").textContent).toContain("Agents");
});

test("session `@` query with no matches shows a compact no-results panel and Enter still submits (not intercepted)", async () => {
  const send = mock(async () => true);
  useStore.setState({ send });
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  searchFiles.mockImplementation(() => Promise.resolve({ status: "ok" as const, data: [] }));
  render(<SessionView />);

  // "@zzz" matches neither the project ("demo") nor Ada, and the debounced
  // workspace search below is stubbed empty — the flattened picker has no
  // items, so the full ContextPickerMenu never renders, only the compact
  // no-results panel.
  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "@zzz", selectionStart: 4 } });

  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "zzz"));
  await waitFor(() => expect(screen.getByText("No matches.")).toBeTruthy());
  expect(screen.queryByRole("menu")).toBeNull();

  // Enter must not be swallowed by the no-results panel — with no pickable
  // items it falls through to the normal submit path.
  fireEvent.keyDown(composer, { key: "Enter" });
  await waitFor(() => expect(send).toHaveBeenCalled());
});

test("session Escape closes the unified context menu and it stays closed once the debounced search resolves", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });
  expect(screen.getByRole("menu")).toBeTruthy();

  fireEvent.keyDown(composer, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();

  // The debounced `searchFiles` call still fires and resolves for the
  // already-dismissed query — the dismissal (keyed by draft + token start)
  // must survive that resolution instead of popping the menu back open.
  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "a"));
  expect(screen.queryByRole("menu")).toBeNull();
});

test("session textarea Escape closes the agent mention popup", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });
  expect(screen.getByRole("menu")).toBeTruthy();
  fireEvent.keyDown(composer, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
});

test("session plain agent @ mentions open the agent menu immediately and still trigger the unified context search", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada", description: "Accessibility reviewer" },
  ]);
  render(<SessionView />);

  const composer = await screen.findByPlaceholderText("Ask for follow-up changes");
  fireEvent.change(composer, { target: { value: "@a", selectionStart: 2 } });

  // Project/Agents render synchronously off local state — no need to wait.
  expect(screen.getByRole("menu").textContent).toContain("Agents");
  // The unified picker's `@` handling no longer special-cases a plain agent
  // query: the debounced workspace search still fires for it (query "a"),
  // it just doesn't have any matching folders/files to add once it resolves.
  await waitFor(() => expect(searchFiles).toHaveBeenCalledWith(LOCAL_RUNNER, "p1", "a"));
});

test("session composer selects an agent mention from its keyboard menu", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "primary", primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "violet" } }, [
    primary("primary"),
    { ...primary("ada"), name: "Ada" },
  ]);
  render(<SessionView />);

  const composer = (await screen.findByPlaceholderText("Ask for follow-up changes")) as HTMLTextAreaElement;
  fireEvent.change(composer, { target: { value: "ask @a", selectionStart: 6 } });
  expect(screen.getByRole("menu")).toBeTruthy();

  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer.value).toBe("ask @Ada ");
});

test("a deleted primary labels the header and transcript with its preserved identity", async () => {
  seed(LOCAL_RUNNER, { primaryAgentId: "deleted", primaryAgentSnapshot: { id: "deleted", name: "Deleted", avatarColor: "rose" } });
  useStore.setState({
    transcripts: {
      [sessKey(LOCAL_RUNNER, "s1")]: [
        {
          seq: 1,
          role: "assistant",
          blockType: "text",
          text: "Preserved response",
          toolCallId: null,
          toolStatus: null,
          toolKind: null,
          toolName: null,
          toolOutput: null,
          createdAt: 1,
          attachments: [],
          toolPath: null,
          toolInput: null,
          toolDurationMs: null,
          toolExitCode: null,
          toolSummary: null,
          toolSubagent: null,
        },
      ],
    },
  });
  render(<SessionView />);

  expect(await screen.findAllByText("Deleted (Deleted)")).toHaveLength(2);
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
  seed(LOCAL_RUNNER, { primaryAgentId: "reviewer", primaryAgentSnapshot: { id: "reviewer", name: "Reviewer", avatarColor: "violet" } }, [
    primary("reviewer", false),
  ]);
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

test("running queue accepts one rapid Enter submission and clears after durable enqueue", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  const queued = deferred<{ status: "ok"; data: { id: string; text: string } }>();
  seed(
    runnerId,
    {
      status: "running",
      primaryAgentId: "primary",
      primaryAgentSnapshot: { id: "primary", name: "Primary", avatarColor: "blue" },
    },
    [primary("primary")],
  );
  useNav.setState({ drafts: { [draftKey]: "queue this" } });
  enqueueSessionMessage.mockImplementationOnce(() => queued.promise);

  render(<SessionView />);
  const composer = screen.getByPlaceholderText("Enter to queue");
  fireEvent.keyDown(composer, { key: "Enter" });
  fireEvent.keyDown(composer, { key: "Enter" });

  await waitFor(() => expect(enqueueSessionMessage).toHaveBeenCalledTimes(1));
  expect((screen.getByRole("button", { name: "Stop" }) as HTMLButtonElement).disabled).toBe(false);

  queued.resolve({ status: "ok", data: { id: "q1", text: "queue this" } });
  await waitFor(() => expect(useNative.getState().queuedBySession[sessKey(runnerId, "s1")]).toEqual([{ id: "q1", text: "queue this" }]));
  await waitFor(() => expect(useNav.getState().drafts[draftKey]).toBeUndefined());
});

test("idle composer accepts one Enter and click submission while send is pending", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  const sent = deferred<boolean>();
  const send = mock(() => sent.promise);
  seed(runnerId);
  useNav.setState({ drafts: { [draftKey]: "send this" } });
  useStore.setState({ send });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Ask for follow-up changes"), { key: "Enter" });
  const sendButton = screen.getByRole("button", { name: "Send" });
  fireEvent.click(sendButton);

  await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
  expect((sendButton as HTMLButtonElement).disabled).toBe(true);

  sent.resolve(true);
  await waitFor(() => expect(useNav.getState().drafts[draftKey]).toBeUndefined());
  await waitFor(() => expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(false));
});

test("a failed submission retains the draft and allows a retry", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  const send = mock(() => Promise.resolve(false));
  seed(runnerId);
  useNav.setState({ drafts: { [draftKey]: "retry this" } });
  useStore.setState({ send });

  render(<SessionView />);
  const composer = screen.getByPlaceholderText("Ask for follow-up changes");
  fireEvent.keyDown(composer, { key: "Enter" });
  await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
  expect(useNav.getState().drafts[draftKey]).toBe("retry this");

  fireEvent.keyDown(composer, { key: "Enter" });
  await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
  expect(useNav.getState().drafts[draftKey]).toBe("retry this");
});

test("running queue success clears the runner-qualified draft", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId, { status: "running" });
  useNav.setState({ drafts: { [draftKey]: "queue this", s1: "other session" } });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Enter to queue"), { key: "Enter" });

  await waitFor(() =>
    expect(enqueueSessionMessage).toHaveBeenCalledWith(
      runnerId,
      "s1",
      "queue this",
      expect.objectContaining({ context: expect.anything() }),
    ),
  );
  expect(useNav.getState().drafts[draftKey]).toBeUndefined();
  expect(useNav.getState().drafts.s1).toBe("other session");
});

test("running queue failure leaves the runner-qualified draft", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId, { status: "running" });
  useNav.setState({ drafts: { [draftKey]: "keep this", s1: "other session" } });
  enqueueSessionMessage.mockResolvedValue({ status: "error", error: { message: "nope" } });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Enter to queue"), { key: "Enter" });

  await waitFor(() => expect(enqueueSessionMessage).toHaveBeenCalled());
  expect(useNav.getState().drafts[draftKey]).toBe("keep this");
  expect(useNav.getState().drafts.s1).toBe("other session");
});

test("idle send success clears the runner-qualified draft", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId);
  useNav.setState({ drafts: { [draftKey]: "send this", s1: "other session" } });
  const send = mock(async () => true);
  useStore.setState({ send });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Ask for follow-up changes"), { key: "Enter" });

  await waitFor(() => expect(send).toHaveBeenCalled());
  expect(useNav.getState().drafts[draftKey]).toBeUndefined();
  expect(useNav.getState().drafts.s1).toBe("other session");
});

test("idle send failure leaves the runner-qualified draft", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId);
  useNav.setState({ drafts: { [draftKey]: "retry this", s1: "other session" } });
  const send = mock(async () => false);
  useStore.setState({ send });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Ask for follow-up changes"), { key: "Enter" });

  await waitFor(() => expect(send).toHaveBeenCalled());
  expect(useNav.getState().drafts[draftKey]).toBe("retry this");
  expect(useNav.getState().drafts.s1).toBe("other session");
});

test("a rejected idle send keeps the draft without an unhandled rejection", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId);
  useNav.setState({ drafts: { [draftKey]: "retry this" } });
  continueSession.mockRejectedValueOnce(new Error("IPC unavailable"));

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Ask for follow-up changes"), { key: "Enter" });

  await waitFor(() => expect(continueSession).toHaveBeenCalledWith(runnerId, "s1", expect.objectContaining({ text: "retry this" })));
  expect(useNav.getState().drafts[draftKey]).toBe("retry this");
});
