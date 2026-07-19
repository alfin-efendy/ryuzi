import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo } from "@/bindings";
import { useAgentConfigurationCatalog } from "@/store-agent-catalog";

const getAgentConfigurationCatalog = mock(async () => ({
  status: "ok" as const,
  data: {
    skills: [],
    nativeTools: [
      { id: "read", label: "Read", description: "Read files", available: true, commandScoped: false },
      { id: "bash", label: "Bash", description: "Run commands", available: true, commandScoped: true },
    ],
    pluginTools: [{ id: "github", label: "GitHub", description: "GitHub tools", available: true, commandScoped: false }],
    apps: [],
  },
}));
mock.module("@/bindings", () => ({ commands: { getAgentConfigurationCatalog }, events: {} }));

const { AgentPermissionsTab } = await import("./AgentPermissionsTab");
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
    skillCount: 0,
    toolCount: 2,
    knowledgeCount: 0,
    executable: true,
    validation: [],
    isDefault: false,
  },
  permissionRules: [],
  skills: [],
  nativeTools: ["read", "bash"],
  pluginTools: [],
  apps: [],
  modelInfo: null,
  personality: { preset: "helpful", custom: null },
};

beforeEach(() => {
  getAgentConfigurationCatalog.mockClear();
  updateAgent.mockClear();
  useAgents.setState({ saving: false, update: updateAgent });
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});
afterEach(() => {
  cleanup();
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});

test("unavailable permission rules block save and tool IDs are not free-text inputs", async () => {
  render(
    <AgentPermissionsTab
      detail={{
        ...reviewerDetail,
        permissionRules: [{ id: "retired-rule", tool: "retired-tool", decision: "deny", commandPrefix: null }],
      }}
    />,
  );

  await waitFor(() => expect(screen.getByText("Unavailable")).toBeTruthy());
  expect(screen.queryByRole("textbox", { name: "Rule tool ID" })).toBeNull();
  expect(screen.getByRole("button", { name: "Save permissions" }).hasAttribute("disabled")).toBe(true);

  fireEvent.click(screen.getByRole("button", { name: "Remove unavailable rule retired-tool" }));
  await waitFor(() => expect(screen.queryByText("Unavailable")).toBeNull());
  expect(screen.getByRole("button", { name: "Save permissions" }).hasAttribute("disabled")).toBe(false);
});

test("catalog tools expose all decisions and command prefix only for command-scoped tools", async () => {
  render(<AgentPermissionsTab detail={reviewerDetail} />);
  fireEvent.click(screen.getByRole("button", { name: "Add rule" }));

  await waitFor(() => expect(screen.getByRole("combobox", { name: "Rule tool" }).hasAttribute("disabled")).toBe(false));
  expect(screen.queryByRole("textbox", { name: "Command prefix" })).toBeNull();
  fireEvent.click(screen.getByRole("combobox", { name: "Rule tool" }));
  await screen.findByRole("listbox");
  await waitFor(() => expect(screen.getByRole("listbox").textContent).toContain("Bash"));
  fireEvent.click(screen.getByRole("option", { name: /^Bash/ }));
  expect(screen.getByRole("textbox", { name: "Command prefix" })).toBeTruthy();

  fireEvent.click(screen.getByRole("combobox", { name: "Rule decision" }));
  expect(await screen.findByRole("option", { name: "Allow" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Ask" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Deny" })).toBeTruthy();
});

test("duplicate tool and command prefix combinations block save", async () => {
  render(
    <AgentPermissionsTab
      detail={{
        ...reviewerDetail,
        permissionRules: [
          { id: "first", tool: "bash", decision: "allow", commandPrefix: "cargo test" },
          { id: "second", tool: "bash", decision: "deny", commandPrefix: "cargo test" },
        ],
      }}
    />,
  );

  await waitFor(() => expect(screen.getAllByText("Duplicate permission rule.").length).toBe(2));
  expect(screen.getByRole("button", { name: "Save permissions" }).hasAttribute("disabled")).toBe(true);
});
