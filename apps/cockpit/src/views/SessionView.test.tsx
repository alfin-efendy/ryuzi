import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, CommandInfo, OpenTarget, Project, Result, Session } from "@/bindings";
import { LOCAL_RUNNER, refKey } from "@/lib/session-key";
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
const { useConnections } = await import("@/store-connections");
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

function seed(runnerId: string, status: Session["status"] = "idle") {
  useStore.setState({
    sessions: [session(runnerId, { status })],
    projects: [project()],
    focusedSession: { runnerId, pk: "s1" },
    transcripts: {},
    pendingApprovals: [],
  });
  // loaded: true keeps the mount effect from hydrating connections over IPC.
  useConnections.setState({ loaded: true });
  useNative.setState({ commandsByProject: {} });
}

beforeEach(() => {
  drawerMounts.length = 0;
  listOpenTargets.mockClear();
  sessionQueue.mockClear();
  continueSession.mockClear();
  continueSession.mockResolvedValue({ status: "ok", data: null });
  enqueueSessionMessage.mockClear();
  enqueueSessionMessage.mockResolvedValue({ status: "ok", data: { id: "q1", text: "queued" } });
  useNative.setState({ queuedBySession: {} });
  useStore.setState({ send: realSend });
  useNav.setState({ drafts: {} });
});

afterEach(() => {
  cleanup();
  useNav.setState({ bottomOpen: false });
  useConnections.setState({ loaded: false, catalog: [], connections: [] });
  useNative.setState({ commandsByProject: {} });
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
  seed(runnerId, "running");
  useNav.setState({ drafts: { [draftKey]: "queue this" } });
  enqueueSessionMessage.mockImplementationOnce(() => queued.promise);

  render(<SessionView />);
  const composer = screen.getByPlaceholderText("Enter to queue");
  fireEvent.keyDown(composer, { key: "Enter" });
  fireEvent.keyDown(composer, { key: "Enter" });

  await waitFor(() => expect(enqueueSessionMessage).toHaveBeenCalledTimes(1));
  expect((screen.getByRole("button", { name: "Stop" }) as HTMLButtonElement).disabled).toBe(false);

  queued.resolve({ status: "ok", data: { id: "q1", text: "queue this" } });
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
  seed(runnerId, "running");
  useNav.setState({ drafts: { [draftKey]: "queue this", s1: "other session" } });

  render(<SessionView />);
  fireEvent.keyDown(screen.getByPlaceholderText("Enter to queue"), { key: "Enter" });

  await waitFor(() => expect(enqueueSessionMessage).toHaveBeenCalledWith(runnerId, "s1", "queue this", expect.anything()));
  expect(useNav.getState().drafts[draftKey]).toBeUndefined();
  expect(useNav.getState().drafts.s1).toBe("other session");
});

test("running queue failure leaves the runner-qualified draft", async () => {
  const runnerId = "remote-1";
  const draftKey = refKey({ runnerId, pk: "s1" });
  seed(runnerId, "running");
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

  await waitFor(() => expect(continueSession).toHaveBeenCalledWith(runnerId, "s1", "retry this", expect.anything()));
  expect(useNav.getState().drafts[draftKey]).toBe("retry this");
});
