import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo, AgentRegistryInfo } from "@/bindings";

mock.module("@/bindings", () => ({ commands: {}, events: {} }));

const { AgentAdvancedTab, positiveLimit } = await import("./AgentAdvancedTab");
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
    model: { kind: "route", route: "smart" },
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
  maxTurns: 50,
  maxToolRounds: 100,
  modelInfo: null,
};

const registry: AgentRegistryInfo = {
  agents: [reviewerDetail.summary, { ...reviewerDetail.summary, id: "ryuzi", name: "Ryuzi", isDefault: true }],
  defaultAgentId: "ryuzi",
  recovery: [],
  subagentModel: { kind: "route", route: "fast" },
};

beforeEach(() => {
  updateAgent.mockClear();
  setDefault.mockClear();
  remove.mockClear();
  useAgents.setState({ registry, detail: reviewerDetail, saving: false, update: updateAgent, setDefault, remove });
});
afterEach(cleanup);

test("positiveLimit accepts only positive safe integers", () => {
  expect(positiveLimit(" 75 ")).toBe(75);
  expect(positiveLimit("0")).toBeNull();
  expect(positiveLimit("1.5")).toBeNull();
  expect(positiveLimit("9007199254740992")).toBeNull();
});

test("Advanced validates and saves per-agent loop limits", () => {
  render(<AgentAdvancedTab detail={reviewerDetail} />);
  fireEvent.change(screen.getByRole("textbox", { name: "Max turns" }), { target: { value: "0" } });
  expect(screen.getByText("Max turns must be at least 1.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save limits" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(screen.getByRole("textbox", { name: "Max turns" }), { target: { value: "75" } });
  fireEvent.click(screen.getByRole("button", { name: "Save limits" }));
  expect(updateAgent).toHaveBeenCalledWith("reviewer", expect.objectContaining({ maxTurns: 75, maxToolRounds: 100 }));
});

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
