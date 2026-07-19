import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { Project } from "@/bindings";
import { LOCAL_RUNNER, type UiSession } from "@/lib/session-key";

// ProjectsView reads projects straight from the store and never calls a
// command in these tests (the New-project modal stays closed), so the real
// bindings module is left intact — mocking it here would leak into sibling
// test files that share the process.

const { ProjectsView } = await import("./ProjectsView");
const { useStore } = await import("@/store");
const { useUi } = await import("@/store-ui");
const { useNav } = await import("@/store-nav");

function project(projectId: string, name: string, createdAt: number | null = 1): Project {
  return {
    projectId,
    name,
    workdir: `C:\\code\\${name}`,
    source: null,
    model: null,
    effort: null,
    permMode: "default",
    createdAt,
    isGit: true,
  };
}

function session(pk: string, projectId: string, lastActive: number): UiSession {
  return {
    runnerId: LOCAL_RUNNER,
    sessionPk: pk,
    primaryAgentId: null,
    primaryAgentSnapshot: null,
    projectId,
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: "t",
    status: "idle",
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: true,
    permMode: "default",
    kind: "project",
    speaker: null,
    agent: null,
    parentSessionPk: null,
  };
}

afterEach(cleanup);

test("lists every project and filters by the search box", () => {
  useUi.setState({ projectOrdering: "name", projectOrder: [] });
  useStore.setState({ projects: [project("p1", "sentinel"), project("p2", "ryuzi")], sessions: [] });
  render(<ProjectsView />);
  expect(screen.getByText("sentinel")).toBeTruthy();
  expect(screen.getByText("ryuzi")).toBeTruthy();

  fireEvent.change(screen.getByPlaceholderText("Search projects"), { target: { value: "sent" } });
  expect(screen.getByText("sentinel")).toBeTruthy();
  expect(screen.queryByText("ryuzi")).toBeNull();
});

test("opening a project routes to its settings", () => {
  const setProjectSettingsFor = mock(() => {});
  useNav.setState({ setProjectSettingsFor });
  useUi.setState({ projectOrdering: "name", projectOrder: [] });
  useStore.setState({ projects: [project("p1", "sentinel")], sessions: [session("s1", "p1", 5)] });
  render(<ProjectsView />);
  fireEvent.click(screen.getByText("sentinel"));
  expect(setProjectSettingsFor).toHaveBeenCalledWith("p1");
});
