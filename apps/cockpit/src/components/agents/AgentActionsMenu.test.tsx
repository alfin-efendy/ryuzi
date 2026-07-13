import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { AgentDetailInfo, AgentModelInfo, AgentRegistryInfo, AgentSummaryInfo } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const route = (r: string): AgentModelInfo => ({ kind: "route", route: r });

function summary(id: string, name: string, overrides: Partial<AgentSummaryInfo> = {}): AgentSummaryInfo {
  return {
    id,
    name,
    description: "",
    avatarColor: "violet",
    model: route("smart"),
    permissionMode: "ask",
    skillCount: 0,
    toolCount: 0,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: id === "ryuzi",
    ...overrides,
  };
}

const ryuziSummary = () => summary("ryuzi", "Ryuzi");
const reviewerSummary = () => summary("reviewer", "Reviewer");

function registry(): AgentRegistryInfo {
  return {
    agents: [ryuziSummary(), reviewerSummary()],
    defaultAgentId: "ryuzi",
    recovery: [],
    subagentModel: route("fast"),
  };
}

function detailOf(s: AgentSummaryInfo): AgentDetailInfo {
  return {
    summary: s,
    permissionRules: [],
    skills: [],
    nativeTools: [],
    pluginTools: [],
    apps: [],
    maxTurns: 50,
    maxToolRounds: 100,
    modelInfo: null,
  };
}

const duplicateAgent = mock(async (_runnerId: string | null, _agentId: string) => ({
  status: "ok" as const,
  data: detailOf(summary("reviewer-copy", "Reviewer copy")),
}));
const deleteAgent = mock(async (_runnerId: string | null, _agentId: string) => ({
  status: "ok" as const,
  data: { ...registry(), agents: [ryuziSummary()] },
}));

mock.module("@/bindings", () => ({
  commands: { duplicateAgent, deleteAgent },
  events: {},
}));

const { AgentActionsMenu } = await import("./AgentActionsMenu");
const { useAgents } = await import("@/store-agents");
const { useNav } = await import("@/store-nav");

beforeEach(() => {
  duplicateAgent.mockClear();
  deleteAgent.mockClear();
  useAgents.setState({
    registry: registry(),
    detail: null,
    models: [],
    loaded: true,
    loading: false,
    saving: false,
  });
  useNav.setState({
    history: { back: [], current: { kind: "agents" }, forward: [] },
    pendingPrimaryAgentId: null,
  });
});

afterEach(cleanup);

test("menu contains exactly Start chat, Duplicate, and Delete", () => {
  render(<AgentActionsMenu agent={reviewerSummary()} />);
  // No standalone Start chat Button anywhere — it exists only inside the menu.
  expect(screen.queryByRole("button", { name: "Start chat" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  const panel = screen.getByTestId("agent-actions-panel");
  expect(
    within(panel)
      .getAllByRole("button")
      .map((item) => item.textContent),
  ).toEqual(["Start chat", "Duplicate", "Delete"]);
});

test("Start chat preselects the agent through navigation state", () => {
  render(<AgentActionsMenu agent={reviewerSummary()} />);
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  fireEvent.click(screen.getByRole("button", { name: "Start chat" }));
  expect(useNav.getState().pendingPrimaryAgentId).toBe("reviewer");
  expect(useNav.getState().history.current).toEqual({ kind: "home" });
});

test("Duplicate creates a copy and navigates to its detail", async () => {
  render(<AgentActionsMenu agent={reviewerSummary()} />);
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  fireEvent.click(screen.getByRole("button", { name: "Duplicate" }));
  await waitFor(() => expect(duplicateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer"));
  await waitFor(() => expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "reviewer-copy" }));
});

test("Delete confirms with the exact copy and stays on the hub after success", async () => {
  render(<AgentActionsMenu agent={reviewerSummary()} />);
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));

  const dialog = await screen.findByRole("dialog", { name: "Delete Reviewer?" });
  expect(dialog.textContent).toContain("Delete Reviewer?");
  expect(dialog.textContent).toContain(
    "Configuration and isolated knowledge will be permanently removed. Historical sessions remain readable.",
  );

  fireEvent.click(screen.getByRole("button", { name: "Delete agent" }));
  await waitFor(() => expect(deleteAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer"));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delete Reviewer?" })).toBeNull());
  // Deletion from the hub stays on the hub.
  expect(useNav.getState().history.current).toEqual({ kind: "agents" });
  expect(useAgents.getState().registry?.agents.map((a) => a.id)).toEqual(["ryuzi"]);
});

test("Delete confirmation is disabled when only one agent remains", async () => {
  useAgents.setState({ registry: { ...registry(), agents: [reviewerSummary()], defaultAgentId: "reviewer" } });
  render(<AgentActionsMenu agent={reviewerSummary()} />);
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));

  await screen.findByRole("dialog", { name: "Delete Reviewer?" });
  expect((screen.getByRole("button", { name: "Delete agent" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Delete agent" }));
  expect(deleteAgent).not.toHaveBeenCalled();
});
