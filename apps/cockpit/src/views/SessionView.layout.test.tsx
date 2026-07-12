import { afterAll, afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

// Bun's `mock.module(...)` registers a module override for the whole
// `bun test` process, not just this file. Several of the modules SessionView
// pulls in (RightPanel, TranscriptFileContext, QueuedMessages,
// SessionCostPanel, Transcript, TodoPanel, ...) are real, shared components
// that other test files render unmocked and depend on for real behavior
// (see SessionView.test.tsx's remote-runner gating suite, which deliberately
// leaves most of these real). To keep this file's heavier mocks from leaking
// into the rest of the suite, we snapshot each module's real exports *before*
// installing the mock (a plain object spread, not a re-import — re-importing
// the same specifier later only sees the mocked registry entry), then
// restore that snapshot in `afterAll` so later test files in the same
// process see the genuine module again.
const realBindings = { ...(await import("@/bindings")) };
const realTranscriptFileContext = { ...(await import("@/components/transcript/TranscriptFileContext")) };
const realTranscript = { ...(await import("@/components/transcript/Transcript")) };
const realRightPanel = { ...(await import("@/components/session/RightPanel")) };
const realBottomTerminalDrawer = { ...(await import("@/components/session/BottomTerminalDrawer")) };
const realTodoPanel = { ...(await import("@/components/session/TodoPanel")) };
const realQueuedMessages = { ...(await import("@/components/session/QueuedMessages")) };
const realSessionCostPanel = { ...(await import("@/components/session/SessionCostPanel")) };
const realOpenInMenu = { ...(await import("@/components/session/OpenInMenu")) };
const realComposerModelEffortMenu = { ...(await import("@/components/ComposerModelEffortMenu")) };
const realUseComposerAttachments = { ...(await import("@/components/composer/useComposerAttachments")) };
const realAttachmentChips = { ...(await import("@/components/composer/AttachmentChips")) };
const realVoice = { ...(await import("@/lib/voice")) };

mock.module("@/bindings", () => ({
  commands: {
    sessionWorkdir: async () => ({ status: "ok" as const, data: "/work/demo" }),
    searchFiles: async () => ({ status: "ok" as const, data: [] }),
    sessionRuntimeInfo: async () => ({ status: "ok" as const, data: null }),
    listProviderCatalog: async () => [],
    listConnections: async () => [],
    // Not reached from any mount path here (Transcript is mocked away below),
    // but `mock.module` replaces "@/bindings" process-wide: the real
    // Transcript other test files render (e.g. ModalShells.test.tsx) resolves
    // `commands` through this same live binding while it's active.
    fetchAttachment: async () => ({ status: "ok" as const, data: { dataBase64: "", contentType: null } }),
  },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
mock.module("@/components/transcript/TranscriptFileContext", () => ({
  TranscriptFileContext: {
    Provider: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  },
  useOpenWorkspaceFile: () => null,
  WorkspacePathCode: ({ text }: { text: string }) => <>{text}</>,
}));
mock.module("@/components/composer/useComposerAttachments", () => ({
  useComposerAttachments: () => ({
    attachments: [],
    dragOver: false,
    onPaste: () => undefined,
    remove: () => undefined,
  }),
}));
mock.module("@/components/composer/AttachmentChips", () => ({ AttachmentChips: () => null }));
mock.module("@/lib/voice", () => ({
  startVoiceDictation: () => ({ ok: false as const, message: "Voice unavailable in test" }),
}));
mock.module("@/components/transcript/Transcript", () => ({
  Transcript: ({ children }: { children?: React.ReactNode }) => <div data-testid="transcript">{children}</div>,
}));
mock.module("@/components/session/RightPanel", () => ({
  RightPanel: () => <div data-testid="right-panel">right</div>,
}));
mock.module("@/components/session/BottomTerminalDrawer", () => ({
  BottomTerminalDrawer: () => <div data-testid="bottom-terminal">terminal</div>,
}));
mock.module("@/components/session/TodoPanel", () => ({ TodoPanel: () => null }));
mock.module("@/components/session/QueuedMessages", () => ({ QueuedMessages: () => null }));
mock.module("@/components/session/SessionCostPanel", () => ({ SessionCostPanel: () => null }));
mock.module("@/components/session/OpenInMenu", () => ({ OpenInMenu: () => null }));
mock.module("@/components/ComposerModelEffortMenu", () => ({ ComposerModelEffortMenu: () => null }));

const { SessionView } = await import("./SessionView");
const { useNav } = await import("@/store-nav");
const { useStore } = await import("@/store");

beforeEach(() => {
  useNav.setState({ bottomOpen: true, rightOpen: true, rightMaximized: false, rightTab: "review", drafts: {} });
  useStore.setState({
    focusedSession: { runnerId: LOCAL_RUNNER, pk: "s1" },
    sessions: [
      {
        runnerId: LOCAL_RUNNER,
        sessionPk: "s1",
        projectId: null,
        agentSessionId: null,
        worktreePath: null,
        branch: null,
        title: "Review layout",
        status: "idle",
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
    ],
    projects: [],
    transcripts: { [sessKey(LOCAL_RUNNER, "s1")]: [] },
    pendingApprovals: [],
  });
});

afterEach(cleanup);

afterAll(() => {
  // Restore every globally-mocked module from its pre-mock snapshot so later
  // test files in this same `bun test` process see the real implementations
  // again, instead of the overrides installed above.
  mock.module("@/bindings", () => realBindings);
  mock.module("@/components/transcript/TranscriptFileContext", () => realTranscriptFileContext);
  mock.module("@/components/composer/useComposerAttachments", () => realUseComposerAttachments);
  mock.module("@/components/composer/AttachmentChips", () => realAttachmentChips);
  mock.module("@/lib/voice", () => realVoice);
  mock.module("@/components/transcript/Transcript", () => realTranscript);
  mock.module("@/components/session/RightPanel", () => realRightPanel);
  mock.module("@/components/session/BottomTerminalDrawer", () => realBottomTerminalDrawer);
  mock.module("@/components/session/TodoPanel", () => realTodoPanel);
  mock.module("@/components/session/QueuedMessages", () => realQueuedMessages);
  mock.module("@/components/session/SessionCostPanel", () => realSessionCostPanel);
  mock.module("@/components/session/OpenInMenu", () => realOpenInMenu);
  mock.module("@/components/ComposerModelEffortMenu", () => realComposerModelEffortMenu);
});

test("panel controls live at workspace scope and expose pressed state", async () => {
  render(<SessionView />);
  await act(async () => {});
  const controls = screen.getByTestId("session-panel-controls");
  const chatHeader = screen.getByTestId("session-chat-header");
  const bottomToggle = screen.getByTitle("Toggle bottom panel");
  const rightToggle = screen.getByTitle("Toggle right panel");

  expect(controls.contains(bottomToggle)).toBe(true);
  expect(controls.contains(rightToggle)).toBe(true);
  expect(chatHeader.contains(bottomToggle)).toBe(false);
  expect(bottomToggle.getAttribute("aria-pressed")).toBe("true");
  expect(rightToggle.getAttribute("aria-pressed")).toBe("true");
});

test("bottom terminal is outside the horizontal main row", async () => {
  render(<SessionView />);
  await act(async () => {});
  const mainRow = screen.getByTestId("session-main-row");
  const bottomRow = screen.getByTestId("session-bottom-row");
  const terminal = screen.getByTestId("bottom-terminal");

  expect(mainRow.contains(screen.getByTestId("right-panel"))).toBe(true);
  expect(mainRow.contains(terminal)).toBe(false);
  expect(bottomRow.contains(terminal)).toBe(true);
  expect(mainRow.parentElement).toBe(bottomRow.parentElement);
});

test("workspace toggles remain rendered and update panel state when panels close", async () => {
  render(<SessionView />);
  await act(async () => {});
  fireEvent.click(screen.getByTitle("Toggle right panel"));
  fireEvent.click(screen.getByTitle("Toggle bottom panel"));

  expect(useNav.getState().rightOpen).toBe(false);
  expect(useNav.getState().bottomOpen).toBe(false);
  expect(screen.getByTitle("Toggle right panel")).toBeTruthy();
  expect(screen.getByTitle("Toggle bottom panel")).toBeTruthy();
});
