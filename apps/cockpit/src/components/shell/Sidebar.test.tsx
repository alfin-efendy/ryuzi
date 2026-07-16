import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { Project } from "@/bindings";
import { LOCAL_RUNNER, sessKey, type UiSession } from "@/lib/session-key";

const worktreeDirty = mock(async () => ({ status: "ok" as const, data: { dirty: true, unmergedCommits: 0 } }));
const termCloseSession = mock(async () => ({ status: "ok" as const, data: null }));

mock.module("@/bindings", () => ({
  commands: { worktreeDirty, termCloseSession },
  events: {},
}));

const { Sidebar } = await import("./Sidebar");
const { useStore } = await import("@/store");
const { useUi } = await import("@/store-ui");
const { useNav } = await import("@/store-nav");
const { useGateways } = await import("@/store-gateways");

const project: Project = {
  projectId: "p1",
  name: "Ryuzi",
  workdir: "C:\\code\\ryuzi",
  source: null,
  model: null,
  effort: null,
  permMode: "default",
  createdAt: 1,
  isGit: true,
};

const session: UiSession = {
  runnerId: LOCAL_RUNNER,
  sessionPk: "s1",
  primaryAgentId: null,
  primaryAgentSnapshot: null,
  projectId: "p1",
  agentSessionId: null,
  worktreePath: "C:\\code\\ryuzi-worktree",
  branch: "feat/modal-safety",
  title: "Preserve modal safety",
  status: "idle",
  startedBy: null,
  createdAt: 1,
  lastActive: 2,
  resumeAttempts: 0,
  branchOwned: true,
  permMode: "default",
  kind: "project",
  speaker: null,
  agent: null,
  parentSessionPk: null,
};

const endSession = mock((_runnerId: string, _sessionPk: string): Promise<boolean> => Promise.resolve(true));

beforeEach(() => {
  worktreeDirty.mockClear();
  termCloseSession.mockClear();
  endSession.mockClear();
  useStore.setState({
    projects: [project],
    sessions: [session],
    transcripts: {},
    pendingApprovals: [],
    focusedSession: null,
    selectedProjectId: null,
    end: endSession,
  });
  useUi.setState({
    pinned: {},
    archived: {},
    sessionFilter: { statuses: {}, unreadOnly: false },
    organizeBy: "project",
    collapsed: {},
    taskOrder: {},
    projectOrder: [],
  });
  useNav.setState({
    history: { back: [], current: { kind: "home" }, forward: [] },
    sidebarOpen: true,
    searchQuery: "",
  });
  useGateways.setState({ gateways: [], eventsById: {}, activeGateway: "local", loaded: true, probing: false });
});

afterEach(cleanup);

async function openArchiveConfirmation() {
  render(<Sidebar />);
  fireEvent.click(screen.getByTitle("Archive — ends the session and removes its worktree"));
  return await screen.findByRole("dialog", { name: "Archive session?" });
}

test("sidebar exposes Agents without a top-level Learning route", () => {
  render(<Sidebar />);
  expect(screen.getByRole("button", { name: "Agents" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Learning" })).toBeNull();
});

test("sidebar opens Automations and keeps it active for legacy scheduler jobs", () => {
  useNav.setState({ history: { back: [], current: { kind: "scheduler" }, forward: [] } });
  render(<Sidebar />);

  const automations = screen.getByRole("button", { name: "Automations" });
  expect(automations.className).toContain("bg-sidebar-accent");
  fireEvent.click(automations);
  expect(useNav.getState().history.current).toEqual({ kind: "automations" });
});

test("archive confirmation preserves the consequences and initially focuses Cancel", async () => {
  const dialog = await openArchiveConfirmation();
  const cancel = screen.getByRole("button", { name: "Cancel" });

  await waitFor(() => expect(document.activeElement).toBe(cancel));
  expect(dialog.textContent).toContain("Archiving ends the session and deletes the worktree and its");
  expect(dialog.textContent).toContain("branch — that work is discarded and unrecoverable. The transcript stays available.");
});

test("busy archive locks every dismissal path until teardown settles", async () => {
  let resolveClose: ((result: { status: "ok"; data: null }) => void) | undefined;
  termCloseSession.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        resolveClose = resolve;
      }),
  );
  await openArchiveConfirmation();
  fireEvent.click(screen.getByRole("button", { name: "Archive & discard work" }));

  const close = screen.getByRole("button", { name: "Close" }) as HTMLButtonElement;
  const cancel = screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
  const archive = screen.getByRole("button", { name: "Archiving…" }) as HTMLButtonElement;
  await waitFor(() => expect(screen.getByRole("dialog", { name: "Archive session?" }).getAttribute("aria-busy")).toBe("true"));
  expect(close.disabled).toBe(true);
  expect(cancel.disabled).toBe(true);
  expect(archive.disabled).toBe(true);

  fireEvent.click(close);
  fireEvent.click(cancel);
  fireEvent.click(archive);
  fireEvent.keyDown(document, { key: "Escape" });
  fireEvent.click(document.querySelector('[data-slot="modal-backdrop"]') as HTMLElement);
  expect(screen.getByRole("dialog", { name: "Archive session?" })).toBeTruthy();
  expect(termCloseSession).toHaveBeenCalledTimes(1);

  await act(async () => resolveClose?.({ status: "ok", data: null }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Archive session?" })).toBeNull());
  expect(endSession).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
});

function sessionFixture(pk: string, lastActive: number): UiSession {
  return {
    ...session,
    sessionPk: pk,
    primaryAgentId: null,
    primaryAgentSnapshot: null,
    projectId: "p1",
    title: pk,
    lastActive,
    worktreePath: null,
    branch: null,
    branchOwned: false,
  };
}

const k1 = sessKey(LOCAL_RUNNER, "s1");
const k2 = sessKey(LOCAL_RUNNER, "s2");

test("renders an unread dot for an unread, non-focused session", () => {
  useUi.setState({ readAt: { [k1]: 100, [k2]: 100 }, sessionFilter: { statuses: {}, unreadOnly: false } });
  useStore.setState({
    projects: [project],
    sessions: [sessionFixture("s1", 500), sessionFixture("s2", 50)],
    focusedSession: null,
    pendingApprovals: [],
  });
  render(<Sidebar />);
  expect(screen.getByTestId("unread-dot-s1")).toBeTruthy();
  expect(screen.queryByTestId("unread-dot-s2")).toBeNull();
});

test("does not show an unread dot for the focused session even if unseen", () => {
  useUi.setState({ readAt: { [k1]: 100 }, sessionFilter: { statuses: {}, unreadOnly: false } });
  useStore.setState({
    projects: [project],
    sessions: [sessionFixture("s1", 500)],
    focusedSession: { runnerId: LOCAL_RUNNER, pk: "s1" },
    pendingApprovals: [],
  });
  render(<Sidebar />);
  expect(screen.queryByTestId("unread-dot-s1")).toBeNull();
});

test("renders New Task nav and a Tasks section header, not the old labels", () => {
  useStore.setState({
    projects: [project],
    sessions: [{ ...session, projectId: null, kind: "chat" }],
    pendingApprovals: [],
    focusedSession: null,
  });
  render(<Sidebar />);
  expect(screen.getByRole("button", { name: "New Task" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "New session" })).toBeNull();
  expect(screen.getByText("Tasks")).toBeTruthy();
  expect(screen.queryByText("Chat")).toBeNull();
});

test("Organize menu switches By Project / By Task and offers Manual Order; no Status filter", () => {
  render(<Sidebar />);
  // open the Projects-header menu
  fireEvent.click(screen.getAllByTitle("Sort and organize")[0]);
  expect(screen.getByText("Organize")).toBeTruthy();
  expect(screen.queryByText("Status")).toBeNull();
  expect(screen.queryByText("Collapse all")).toBeNull();
  expect(screen.queryByText("Mark all as read")).toBeNull();
});

test("By Task mode shows all tasks flat and a Projects section without nested tasks", () => {
  useUi.setState({ organizeBy: "task", collapsed: {} });
  useStore.setState({
    projects: [project],
    sessions: [
      { ...session, sessionPk: "cx", projectId: null, kind: "chat", title: "chat task" },
      { ...session, sessionPk: "px", projectId: "p1", kind: "project", title: "project task" },
    ],
    pendingApprovals: [],
    focusedSession: null,
  });
  render(<Sidebar />);
  // both tasks appear in the flat Tasks list
  expect(screen.getByText("chat task")).toBeTruthy();
  expect(screen.getByText("project task")).toBeTruthy();
  // Projects section still lists the project for navigation
  expect(screen.getByText("Ryuzi")).toBeTruthy();
  // …but flat (no nested per-project tree, so no per-project settings gear).
  expect(screen.queryByTitle("Project settings")).toBeNull();
});

test("By Project mode keeps project tasks nested and chat tasks in the Tasks section", () => {
  useUi.setState({ organizeBy: "project", collapsed: {} });
  useStore.setState({
    projects: [project],
    sessions: [
      { ...session, sessionPk: "cx", projectId: null, kind: "chat", title: "chat task" },
      { ...session, sessionPk: "px", projectId: "p1", kind: "project", title: "nested task" },
    ],
    pendingApprovals: [],
    focusedSession: null,
  });
  render(<Sidebar />);
  expect(screen.getByText("chat task")).toBeTruthy();
  expect(screen.getByText("nested task")).toBeTruthy();
});

test("collapsing the Tasks section hides its rows", () => {
  useStore.setState({
    projects: [project],
    sessions: [{ ...session, projectId: null, kind: "chat", title: "chatty" }],
    pendingApprovals: [],
    focusedSession: null,
  });
  useUi.setState({ collapsed: {} });
  render(<Sidebar />);
  expect(screen.getByText("chatty")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /collapse tasks/i }));
  expect(screen.queryByText("chatty")).toBeNull();
});
