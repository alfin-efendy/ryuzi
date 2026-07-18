import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type {
  AgentDetailInfo,
  AgentModelInfo,
  AgentMutationInfo,
  AgentRegistryInfo,
  AgentSummaryInfo,
  SelectableModelInfo,
} from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const route = (r: string): AgentModelInfo => ({ kind: "route", route: r });

function summary(id: string, name: string, overrides: Partial<AgentSummaryInfo> = {}): AgentSummaryInfo {
  return {
    id,
    name,
    description: "",
    avatarColor: "violet",
    model: route("free"),
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

const reviewer = summary("reviewer", "Reviewer", {
  description: "Reviews implementation quality and regressions.",
  skillCount: 1,
  toolCount: 3,
});
const ryuzi = summary("ryuzi", "Ryuzi");

function registry(): AgentRegistryInfo {
  return { agents: [ryuzi, reviewer], defaultAgentId: "ryuzi", recovery: [], subagentModel: route("free") };
}

const selectable: SelectableModelInfo = {
  kind: "concrete",
  requestValue: "anthropic/claude-opus-4",
  displayName: "Claude Opus",
  preferenceKey: null,
  supported: [
    { value: "low", label: "Low", description: null },
    { value: "high", label: "High", description: null },
  ],
  configuredDefault: null,
  resolvedDefault: "high",
  defaultSource: "provider",
};

function detail(input: AgentMutationInfo): AgentDetailInfo {
  return {
    summary: summary(input.name.trim().toLowerCase().replace(/\s+/g, "-"), input.name, {
      description: input.description,
      avatarColor: input.avatarColor,
      model: input.model,
      permissionMode: input.permissionMode,
    }),
    permissionRules: input.permissionRules,
    skills: input.skills,
    nativeTools: input.nativeTools,
    pluginTools: input.pluginTools,
    apps: input.apps,
    maxTurns: input.maxTurns,
    maxToolRounds: input.maxToolRounds,
    modelInfo: null,
    personality: { preset: "helpful", custom: null },
  };
}

const createAgent = mock(async (_runnerId: string | null, input: AgentMutationInfo) => ({ status: "ok" as const, data: detail(input) }));
const updateSubagentModel = mock(async (_runnerId: string | null, model: AgentModelInfo) => ({
  status: "ok" as const,
  data: { ...registry(), subagentModel: model },
}));

mock.module("@/bindings", () => ({
  commands: { createAgent, updateSubagentModel },
  events: {},
}));

const { AgentsView } = await import("./AgentsView");
const { useAgents } = await import("@/store-agents");
const { useNav } = await import("@/store-nav");

function seedAgents() {
  useAgents.setState({
    registry: registry(),
    detail: null,
    models: [selectable],
    loaded: true,
    loading: false,
    saving: false,
  });
  useNav.setState({
    history: { back: [], current: { kind: "agents" }, forward: [] },
    pendingPrimaryAgentId: null,
  });
}

beforeEach(() => {
  createAgent.mockClear();
  updateSubagentModel.mockClear();
  seedAgents();
});

afterEach(cleanup);

test("management flow creates through the generated command store and opens detail", async () => {
  useAgents.setState({ registry: { ...registry(), agents: [ryuzi] } });
  render(<AgentsView />);
  fireEvent.click(screen.getByRole("button", { name: "New agent" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Reviewer" } });
  fireEvent.change(screen.getByRole("textbox", { name: "Description" }), { target: { value: "Reviews changes" } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await waitFor(() =>
    expect(createAgent).toHaveBeenCalledWith(LOCAL_RUNNER, expect.objectContaining({ name: "Reviewer", description: "Reviews changes" })),
  );
  await waitFor(() => expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "reviewer" }));
});

test("Main Agent tab renders roster metadata and opens dedicated detail", () => {
  render(<AgentsView />);
  expect(screen.getByRole("button", { name: "Main Agent" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sub Agent" })).toBeTruthy();
  expect(screen.getByText("Reviews implementation quality and regressions.")).toBeTruthy();
  expect(screen.getAllByText("free").length).toBeGreaterThan(0);
  expect(screen.getAllByText("Ask").length).toBeGreaterThan(0);
  expect(screen.getByText("1 skill · 3 tools")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open Reviewer" }));
  expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "reviewer" });
});

test("Sub Agent tab exposes one shared model and no create affordance", () => {
  render(<AgentsView />);
  fireEvent.click(screen.getByRole("button", { name: "Sub Agent" }));
  expect(screen.getByText(/ephemeral, memoryless runtime workers/)).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Shared subagent model" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "New agent" })).toBeNull();
});

test("shared concrete subagent model shows only its supported effort options", async () => {
  useAgents.setState({ registry: { ...registry(), subagentModel: { kind: "concrete", name: selectable.requestValue, effort: "high" } } });
  render(<AgentsView />);
  fireEvent.click(screen.getByRole("button", { name: "Sub Agent" }));
  const effort = screen.getByRole("combobox", { name: "Shared subagent effort" });
  fireEvent.click(effort);
  expect(await screen.findByRole("option", { name: "Model default" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Low" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "High" })).toBeTruthy();
});

test("create modal sends the complete initial mutation and opens the new detail", async () => {
  render(<AgentsView />);
  fireEvent.click(screen.getByRole("button", { name: "New agent" }));
  expect((screen.getByRole("button", { name: "Create" }) as HTMLButtonElement).disabled).toBe(true);

  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "  Architect  " } });
  fireEvent.change(screen.getByRole("textbox", { name: "Description" }), { target: { value: "  Designs system boundaries.  " } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await waitFor(() =>
    expect(createAgent).toHaveBeenCalledWith(LOCAL_RUNNER, {
      name: "Architect",
      description: "Designs system boundaries.",
      avatarColor: "violet",
      model: route("free"),
      permissionMode: "ask",
      permissionRules: [],
      skills: [],
      nativeTools: [],
      pluginTools: [],
      apps: [],
      maxTurns: 50,
      maxToolRounds: 100,
    }),
  );
  await waitFor(() => expect(useNav.getState().history.current).toEqual({ kind: "agentDetail", agentId: "architect" }));
});
