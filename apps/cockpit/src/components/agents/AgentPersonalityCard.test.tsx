import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo } from "@/bindings";
import { useAgentConfigurationCatalog } from "@/store-agent-catalog";

mock.module("@/bindings", () => ({ commands: {}, events: {} }));

const { AgentPersonalityCard } = await import("./AgentPersonalityCard");
const { useAgents } = await import("@/store-agents");

const updateAgent = mock(async (_agentId: string, _input: AgentMutationInfo) => true);

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
  maxTurns: 50,
  maxToolRounds: 100,
  modelInfo: null,
  personality: { preset: "helpful", custom: null },
};

beforeEach(() => {
  updateAgent.mockClear();
  useAgents.setState({ saving: false, update: updateAgent });
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});
afterEach(() => {
  cleanup();
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});

test("switching to Custom reveals the textarea and disables save while blank", async () => {
  render(<AgentPersonalityCard detail={reviewerDetail} />);

  expect(screen.queryByRole("textbox", { name: "Custom personality" })).toBeNull();

  fireEvent.click(screen.getByRole("combobox", { name: "Personality preset" }));
  fireEvent.click(await screen.findByRole("option", { name: /^Custom/ }));

  const textarea = screen.getByRole("textbox", { name: "Custom personality" });
  expect(textarea).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save personality" }).hasAttribute("disabled")).toBe(true);

  fireEvent.change(textarea, { target: { value: "  " } });
  expect(screen.getByRole("button", { name: "Save personality" }).hasAttribute("disabled")).toBe(true);

  fireEvent.change(textarea, { target: { value: "Speak like a stern librarian." } });
  expect(screen.getByRole("button", { name: "Save personality" }).hasAttribute("disabled")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Save personality" }));
  expect(updateAgent).toHaveBeenCalledWith(
    "reviewer",
    expect.objectContaining({ personality: { preset: "custom", custom: "Speak like a stern librarian." } }),
  );
});

test("a non-custom preset hides the textarea and shows its description", () => {
  render(<AgentPersonalityCard detail={reviewerDetail} />);

  expect(screen.queryByRole("textbox", { name: "Custom personality" })).toBeNull();
  expect(screen.getByText(/You are a helpful, direct assistant\./)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save personality" }).hasAttribute("disabled")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Save personality" }));
  expect(updateAgent).toHaveBeenCalledWith("reviewer", expect.objectContaining({ personality: { preset: "helpful", custom: null } }));
});
