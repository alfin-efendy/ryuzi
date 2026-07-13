import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo, AgentRegistryInfo, SelectableModelInfo } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const getAgent = mock(async (_runner: string | null, id: string) => ({
  status: "ok" as const,
  data: detail({ summary: { ...detail().summary, id, name: id === "ryuzi" ? "Ryuzi" : "Reviewer", isDefault: id === "ryuzi" } }),
}));
const listApps = mock(async () => ({ status: "ok" as const, data: [] }));
const updateAgent = mock(async (_runner: string | null, _id: string, input: AgentMutationInfo) => ({
  status: "ok" as const,
  data: detail({ ...input, modelInfo: null }),
}));
const duplicateAgent = mock(async (_runner: string | null, _id: string) => ({
  status: "ok" as const,
  data: detail({ summary: { ...detail().summary, id: "reviewer-copy", name: "Reviewer Copy" } }),
}));
const deleteAgent = mock(async (_runner: string | null, _id: string) => ({
  status: "ok" as const,
  data: { ...registry, agents: registry.agents.filter((agent) => agent.id !== "reviewer-copy") },
}));

mock.module("@/bindings", () => ({ commands: { deleteAgent, duplicateAgent, getAgent, listApps, updateAgent }, events: {} }));
mock.module("@/components/agents/AgentLearningTab", () => ({
  AgentLearningTab: ({ agentId }: { agentId: string }) => <div>Learning for {agentId}</div>,
}));

const { AgentDetailView } = await import("./AgentDetailView");
const { useAgents } = await import("@/store-agents");
const { useApps } = await import("@/store-apps");
const { useNav } = await import("@/store-nav");

const routeInfo: SelectableModelInfo = {
  kind: "namedRoute",
  requestValue: "smart",
  displayName: "Smart",
  preferenceKey: null,
  supported: [],
  configuredDefault: null,
  resolvedDefault: null,
  defaultSource: "none",
};
const opusInfo: SelectableModelInfo = {
  kind: "concrete",
  requestValue: "anthropic/claude-opus-4-8",
  displayName: "Claude Opus",
  preferenceKey: null,
  supported: ["low", "medium", "high", "max", "xhigh"].map((value) => ({
    value,
    label: value === "xhigh" ? "XHigh" : value[0].toUpperCase() + value.slice(1),
    description: null,
  })),
  configuredDefault: null,
  resolvedDefault: "high",
  defaultSource: "provider",
};

const miniInfo: SelectableModelInfo = {
  ...opusInfo,
  requestValue: "anthropic/claude-haiku-4-5",
  displayName: "Claude Haiku",
  supported: [{ value: "low", label: "Low", description: null }],
  resolvedDefault: "low",
};

function detail(overrides: Partial<AgentDetailInfo> = {}): AgentDetailInfo {
  return {
    summary: {
      id: "reviewer",
      name: "Reviewer",
      description: "Reviews implementation quality.",
      avatarColor: "violet",
      model: { kind: "route", route: "smart" },
      permissionMode: "ask",
      skillCount: 1,
      toolCount: 3,
      knowledgeCount: 12,
      executable: true,
      validation: [],
      isDefault: false,
    },
    permissionRules: [],
    skills: ["requesting-code-review"],
    nativeTools: ["read", "grep", "bash"],
    pluginTools: [],
    apps: [],
    maxTurns: 50,
    maxToolRounds: 100,
    modelInfo: routeInfo,
    ...overrides,
  };
}

const registry: AgentRegistryInfo = {
  agents: [detail().summary, { ...detail().summary, id: "ryuzi", name: "Ryuzi", isDefault: true }],
  defaultAgentId: "ryuzi",
  recovery: [],
  subagentModel: { kind: "route", route: "fast" },
};

function seed(value = detail()) {
  useAgents.setState({ registry, detail: value, models: [routeInfo, opusInfo, miniInfo], loaded: true, loading: false, saving: false });
  useNav.setState({ history: { back: [], current: { kind: "agentDetail", agentId: "reviewer" }, forward: [] } });
}

beforeEach(() => {
  deleteAgent.mockClear();
  duplicateAgent.mockClear();
  listApps.mockClear();
  updateAgent.mockClear();
  useApps.setState({ apps: [], loaded: false, hydrating: false, probing: null });
  seed();
});
afterEach(cleanup);

test("management flow inspects, duplicates, starts chat with, and deletes through the generated command store", async () => {
  const { unmount } = render(<AgentDetailView agentId="reviewer" />);
  expect(screen.getByRole("heading", { name: "Reviewer" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer" }));
  fireEvent.click(screen.getByRole("button", { name: "Duplicate" }));
  await waitFor(() => expect(duplicateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer"));
  await waitFor(() => expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "reviewer-copy" }));

  unmount();
  seed(detail({ summary: { ...detail().summary, id: "reviewer-copy", name: "Reviewer Copy" } }));
  render(<AgentDetailView agentId="reviewer-copy" />);
  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer Copy" }));
  fireEvent.click(screen.getByRole("button", { name: "Start chat" }));
  expect(useNav.getState().pendingPrimaryAgentId).toBe("reviewer-copy");
  expect(useNav.getState().history.current).toEqual({ kind: "home" });

  fireEvent.click(screen.getByRole("button", { name: "Actions for Reviewer Copy" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  await screen.findByRole("dialog", { name: "Delete Reviewer Copy?" });
  fireEvent.click(screen.getByRole("button", { name: "Delete agent" }));
  await waitFor(() => expect(deleteAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer-copy"));
});

test("detail has Back, identity, actions, six tabs, and overview metrics", () => {
  render(<AgentDetailView agentId="reviewer" />);
  expect(screen.getByRole("button", { name: "Back to Agents" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Reviewer" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Actions for Reviewer" })).toBeTruthy();
  const tabs = screen.getByTestId("agent-detail-tabs");
  expect(
    within(tabs)
      .getAllByRole("button")
      .map((button) => button.textContent),
  ).toEqual(["Overview", "Model", "Permissions", "Skills & Tools", "Learning", "Advanced"]);
  expect(screen.getByText("12 readable concepts")).toBeTruthy();
  expect(screen.getByText("1 enabled skill")).toBeTruthy();
  expect(screen.getByText("3 enabled tools")).toBeTruthy();
  expect(screen.getByText("No owned sessions yet.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Start chat" })).toBeNull();
});

test("Back uses navigation history", () => {
  useNav.setState({ history: { back: [{ kind: "models" }], current: { kind: "agentDetail", agentId: "reviewer" }, forward: [] } });
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Back to Agents" }));
  expect(useNav.getState().history.current).toEqual({ kind: "models" });
});

test("concrete model renders resolver-supported effort values and route has no effort", async () => {
  const concrete = detail({
    summary: { ...detail().summary, model: { kind: "concrete", name: opusInfo.requestValue, effort: "high" } },
    modelInfo: opusInfo,
  });
  seed(concrete);
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Model" }));
  fireEvent.click(screen.getByRole("combobox", { name: "Agent effort" }));
  for (const label of ["Model default", "Low", "Medium", "High", "Max", "XHigh"]) {
    expect(await screen.findByRole("option", { name: label })).toBeTruthy();
  }
  cleanup();
  seed();
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Model" }));
  expect(screen.queryByRole("combobox", { name: "Agent effort" })).toBeNull();
});

test("explicit permission rule editing persists typed rules", async () => {
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Permissions" }));
  fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Rule tool ID" }), { target: { value: "bash" } });
  fireEvent.click(screen.getByRole("combobox", { name: "Rule decision" }));
  fireEvent.click(await screen.findByRole("option", { name: "Allow" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Command prefix" }), { target: { value: "cargo test" } });
  fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  await waitFor(() =>
    expect(updateAgent).toHaveBeenCalledWith(
      LOCAL_RUNNER,
      "reviewer",
      expect.objectContaining({
        permissionRules: [expect.objectContaining({ tool: "bash", decision: "allow", commandPrefix: "cargo test" })],
      }),
    ),
  );
});

test("permission rules preserve unknown stable tool IDs and allow editing them", async () => {
  seed(detail({ permissionRules: [{ id: "custom-rule", tool: "plugin__acme__deploy", decision: "deny", commandPrefix: null }] }));
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Permissions" }));
  const toolId = screen.getByRole("textbox", { name: "Rule tool ID" });
  expect((toolId as HTMLInputElement).value).toBe("plugin__acme__deploy");
  fireEvent.change(toolId, { target: { value: "mcp__github__create_issue" } });
  fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  await waitFor(() =>
    expect(updateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer", {
      name: "Reviewer",
      description: "Reviews implementation quality.",
      avatarColor: "violet",
      model: { kind: "route", route: "smart" },
      permissionMode: "ask",
      permissionRules: [{ id: "custom-rule", tool: "mcp__github__create_issue", decision: "deny", commandPrefix: null }],
      skills: ["requesting-code-review"],
      nativeTools: ["read", "grep", "bash"],
      pluginTools: [],
      apps: [],
      maxTurns: 50,
      maxToolRounds: 100,
    }),
  );
});

test("model transitions preserve supported effort, clear unsupported effort, and save a complete mutation", async () => {
  const concrete = detail({
    summary: { ...detail().summary, model: { kind: "concrete", name: opusInfo.requestValue, effort: "high" } },
    modelInfo: opusInfo,
  });
  seed(concrete);
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Model" }));

  fireEvent.click(screen.getByRole("combobox", { name: "Agent model" }));
  fireEvent.click(await screen.findByRole("option", { name: miniInfo.requestValue }));
  expect((screen.getByRole("combobox", { name: "Agent effort" }) as HTMLButtonElement).textContent).toContain("Model default");

  fireEvent.click(screen.getByRole("button", { name: "Save model" }));
  await waitFor(() =>
    expect(updateAgent).toHaveBeenCalledWith(LOCAL_RUNNER, "reviewer", {
      name: "Reviewer",
      description: "Reviews implementation quality.",
      avatarColor: "violet",
      model: { kind: "concrete", name: miniInfo.requestValue, effort: null },
      permissionMode: "ask",
      permissionRules: [],
      skills: ["requesting-code-review"],
      nativeTools: ["read", "grep", "bash"],
      pluginTools: [],
      apps: [],
      maxTurns: 50,
      maxToolRounds: 100,
    }),
  );
});

test("changing agent resets the local tab to Overview", async () => {
  const { rerender } = render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Permissions" }));
  expect(screen.getByText("Explicit rules")).toBeTruthy();

  const ryuzi = detail({ summary: { ...detail().summary, id: "ryuzi", name: "Ryuzi", isDefault: true } });
  act(() => {
    useAgents.setState({ detail: ryuzi });
    rerender(<AgentDetailView agentId="ryuzi" />);
  });
  await waitFor(() => expect(screen.getByText("Recent sessions")).toBeTruthy());
  expect(screen.queryByText("Explicit rules")).toBeNull();
});

test("Skills & Tools and Advanced tabs render their owned settings", () => {
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Skills & Tools" }));
  expect(screen.getByRole("textbox", { name: "Skill ID" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save skills and tools" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
  expect(screen.getByRole("textbox", { name: "Max turns" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Delete Reviewer" })).toBeTruthy();
});

test("Learning renders the selected agent's Learning tab", () => {
  render(<AgentDetailView agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Learning" }));
  expect(screen.getByText("Learning for reviewer")).toBeTruthy();
});
