import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useGateways } from "@/store-gateways";

const { Sidebar } = await import("./Sidebar");

// Sidebar's mount effect hydrates gateways when `loaded` is false, which would
// otherwise reach the unmocked Tauri IPC boundary. Seed `loaded: true` so the
// effect is a no-op — the sidebar's own gateway switcher isn't under test here.
function seedGateways() {
  useGateways.setState({ gateways: [], loaded: true, probing: false });
}

afterEach(cleanup);

function sess(pk: string, lastActive: number) {
  return {
    sessionPk: pk,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: pk,
    status: "idle" as const,
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: false,
    permMode: "default" as const,
  };
}

function project() {
  return {
    projectId: "p",
    name: "proj",
    workdir: "/w",
    source: null,
    model: null,
    effort: null,
    permMode: "default" as const,
    createdAt: 0,
    isGit: true,
  };
}

test("renders an unread dot for an unread, non-focused session", () => {
  seedGateways();
  useUi.setState({ readAt: { s1: 100, s2: 100 }, sessionFilter: { statuses: {}, unreadOnly: false } });
  useStore.setState({
    projects: [project()],
    sessions: [sess("s1", 500), sess("s2", 50)], // s1 unread (500>100), s2 read (50<100)
    focusedSessionPk: null,
    pendingApprovals: [],
  });
  render(<Sidebar />);
  // The unread session's title carries the unread affordance; the read one does not.
  expect(screen.getByTestId("unread-dot-s1")).toBeTruthy();
  expect(screen.queryByTestId("unread-dot-s2")).toBeNull();
});

test("does not show an unread dot for the focused session even if unseen", () => {
  seedGateways();
  useUi.setState({ readAt: { s1: 100 }, sessionFilter: { statuses: {}, unreadOnly: false } });
  useStore.setState({
    projects: [project()],
    sessions: [sess("s1", 500)],
    focusedSessionPk: "s1",
    pendingApprovals: [],
  });
  render(<Sidebar />);
  expect(screen.queryByTestId("unread-dot-s1")).toBeNull();
});
