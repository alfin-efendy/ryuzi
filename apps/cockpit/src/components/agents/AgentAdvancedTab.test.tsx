import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo, AgentRegistryInfo } from "@/bindings";

mock.module("@/bindings", () => ({ commands: {}, events: {} }));

const { AgentAdvancedTab } = await import("./AgentAdvancedTab");
const { useAgents } = await import("@/store-agents");

const updateAgent = mock(async (_agentId: string, _input: AgentMutationInfo) => true);
const setDefault = mock(async (_agentId: string) => true);
const remove = mock(async (_agentId: string) => true);

const reviewerDetail: AgentDetailInfo = {
  summary: {
    id: "reviewer",
    name: "Reviewer",
    description: "Reviews implementation quality.",
    avatarColor: "violet",
    model: { kind: "route", route: "free" },
    permissionMode: "ask",
    skillCount: 1,
    toolCount: 3,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: false,
  },
  permissionRules: [],
  skills: ["requesting-code-review"],
  nativeTools: ["read", "grep", "bash"],
  pluginTools: [],
  apps: [],
  modelInfo: null,
  personality: { preset: "helpful", custom: null },
};

const registry: AgentRegistryInfo = {
  agents: [reviewerDetail.summary, { ...reviewerDetail.summary, id: "ryuzi", name: "Ryuzi", isDefault: true }],
  defaultAgentId: "ryuzi",
  recovery: [],
  subagentModel: { kind: "route", route: "free" },
};

beforeEach(() => {
  updateAgent.mockClear();
  setDefault.mockClear();
  remove.mockClear();
  useAgents.setState({ registry, detail: reviewerDetail, saving: false, update: updateAgent, setDefault, remove });
});
afterEach(cleanup);

test("default and danger-zone controls use registry operations", async () => {
  render(<AgentAdvancedTab detail={reviewerDetail} />);
  fireEvent.click(screen.getByRole("button", { name: "Make default" }));
  expect(setDefault).toHaveBeenCalledWith("reviewer");
  fireEvent.click(screen.getByRole("button", { name: "Delete Reviewer" }));
  expect(await screen.findByRole("dialog", { name: "Delete Reviewer?" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Delete agent" }));
  await waitFor(() => expect(remove).toHaveBeenCalledWith("reviewer"));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delete Reviewer?" })).toBeNull());
});

test("current default cannot be made default again", () => {
  render(<AgentAdvancedTab detail={{ ...reviewerDetail, summary: { ...reviewerDetail.summary, isDefault: true } }} />);
  expect(screen.queryByRole("button", { name: "Make default" })).toBeNull();
  expect(screen.getByText("This is the default agent.")).toBeTruthy();
});
