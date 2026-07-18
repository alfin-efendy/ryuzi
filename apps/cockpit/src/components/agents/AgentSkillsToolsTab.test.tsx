import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo } from "@/bindings";
import { useAgentConfigurationCatalog } from "@/store-agent-catalog";

const getAgentConfigurationCatalog = mock(async () => ({
  status: "ok" as const,
  data: {
    skills: [
      {
        id: "requesting-code-review",
        label: "Requesting code review",
        description: "Review guidance",
        available: true,
        commandScoped: false,
      },
      {
        id: "systematic-debugging",
        label: "Systematic debugging",
        description: "Debugging guidance",
        available: true,
        commandScoped: false,
      },
      { id: "another-skill", label: "Another skill", description: "Another skill", available: true, commandScoped: false },
    ],
    nativeTools: [
      { id: "read", label: "Read", description: "Read files", available: true, commandScoped: false },
      { id: "grep", label: "Grep", description: "Search files", available: true, commandScoped: false },
      { id: "bash", label: "Bash", description: "Run commands", available: true, commandScoped: true },
      { id: "glob", label: "Glob", description: "Find files", available: true, commandScoped: false },
    ],
    pluginTools: [{ id: "github", label: "GitHub", description: "GitHub tools", available: true, commandScoped: false }],
    apps: [{ id: "github", label: "GitHub", description: "GitHub MCP", available: true, commandScoped: false }],
  },
}));
mock.module("@/bindings", () => ({ commands: { getAgentConfigurationCatalog }, events: {} }));

const { AgentSkillsToolsTab } = await import("./AgentSkillsToolsTab");
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
  getAgentConfigurationCatalog.mockClear();
  useAgents.setState({ saving: false, update: updateAgent });
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});
afterEach(() => {
  cleanup();
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});

async function choose(catalogLabel: string, option: string) {
  await waitFor(() => expect(screen.getByRole("combobox", { name: catalogLabel }).hasAttribute("disabled")).toBe(false));
  fireEvent.click(screen.getByRole("combobox", { name: catalogLabel }));
  await screen.findByRole("listbox");
  fireEvent.click(screen.getByRole("option", { name: new RegExp(option) }));
  await waitFor(() => expect(screen.getByRole("combobox", { name: catalogLabel }).getAttribute("aria-expanded")).toBe("false"));
}

test("loads the configuration catalog and resolves configured app IDs", async () => {
  render(<AgentSkillsToolsTab detail={{ ...reviewerDetail, apps: ["github"] }} />);

  await waitFor(() => expect(screen.getByText("GitHub")).toBeTruthy());
  expect(screen.queryByText("Unavailable")).toBeNull();
  expect(getAgentConfigurationCatalog).toHaveBeenCalledTimes(1);
  expect(getAgentConfigurationCatalog).toHaveBeenCalledWith("local");
});

test("catalog selection has no free-text Stable ID placeholders", async () => {
  render(<AgentSkillsToolsTab detail={reviewerDetail} />);

  await waitFor(() => expect(screen.getByRole("combobox", { name: "Skill catalog" })).toBeTruthy());
  expect(screen.queryByPlaceholderText(/Stable .* ID/)).toBeNull();
  await choose("Skill catalog", "Systematic debugging");
  await choose("Native tool catalog", "Glob");
  await choose("Plugin tool catalog", "GitHub");
  await waitFor(() => expect(screen.getByText("systematic-debugging")).toBeTruthy());
  expect(screen.getByText("glob")).toBeTruthy();
  expect(screen.getAllByText("github").length).toBeGreaterThan(0);
});

test("capability selections save a complete mutation with catalog IDs", async () => {
  render(<AgentSkillsToolsTab detail={reviewerDetail} />);
  await choose("Skill catalog", "Systematic debugging");
  await choose("Native tool catalog", "Glob");
  await choose("Plugin tool catalog", "GitHub");
  await choose("App catalog", "GitHub");
  fireEvent.click(screen.getByRole("button", { name: "Save skills and tools" }));

  expect(updateAgent).toHaveBeenCalledWith(
    "reviewer",
    expect.objectContaining({
      skills: ["requesting-code-review", "systematic-debugging"],
      nativeTools: ["read", "grep", "bash", "glob"],
      pluginTools: ["github"],
      apps: ["github"],
    }),
  );
});

test("catalog options exclude selected IDs and removals are explicit", async () => {
  render(<AgentSkillsToolsTab detail={reviewerDetail} />);
  await choose("Skill catalog", "Systematic debugging");
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Skill catalog" }).hasAttribute("disabled")).toBe(false));
  fireEvent.click(screen.getByRole("combobox", { name: "Skill catalog" }));
  await waitFor(() => expect(screen.getByRole("listbox")).toBeTruthy());
  expect(screen.queryByRole("option", { name: /Systematic debugging/ })).toBeNull();
  fireEvent.keyDown(screen.getByRole("listbox"), { key: "Escape" });
  fireEvent.click(screen.getByRole("button", { name: "Remove skill requesting-code-review" }));
  expect(screen.queryByText("requesting-code-review")).toBeNull();
});

test("an unavailable saved native tool remains visible and disables Save", async () => {
  render(<AgentSkillsToolsTab detail={{ ...reviewerDetail, nativeTools: ["read", "retired-native-tool"] }} />);

  await waitFor(() => expect(screen.getByText("retired-native-tool")).toBeTruthy());
  expect(screen.getByText("Unavailable")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save skills and tools" }).hasAttribute("disabled")).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Remove unavailable native tool retired-native-tool" }));
  expect(screen.queryByText("retired-native-tool")).toBeNull();
});

test("missing saved apps remain removable and disable Save", async () => {
  render(<AgentSkillsToolsTab detail={{ ...reviewerDetail, apps: ["retired-app"] }} />);
  await waitFor(() => expect(screen.getByTestId("agent-app-rows").textContent).toContain("retired-app"));
  expect(screen.getByRole("button", { name: "Save skills and tools" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("button", { name: "Remove unavailable app retired-app" })).toBeTruthy();
});
