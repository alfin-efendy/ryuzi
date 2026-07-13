import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentDetailInfo, AgentMutationInfo, AppInfo } from "@/bindings";

mock.module("@/bindings", () => ({ commands: {}, events: {} }));

const { AgentSkillsToolsTab } = await import("./AgentSkillsToolsTab");
const { useAgents } = await import("@/store-agents");
const { useApps } = await import("@/store-apps");
const { usePlugins } = await import("@/store-plugins");

const updateAgent = mock(async (_agentId: string, _input: AgentMutationInfo) => true);

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

const github = {
  id: "github",
  name: "GitHub",
  desc: "GitHub MCP",
  kind: "mcp",
  initial: "G",
  color: "blue",
  transport: "stdio",
  command: null,
  args: [],
  url: null,
  scope: "global",
  scopeGateways: [],
  status: "ready",
  statusDetail: null,
  version: null,
  publisher: null,
  authKind: "none",
  authDetail: null,
  tools: [],
  agentAccess: [],
} satisfies AppInfo;

beforeEach(() => {
  updateAgent.mockClear();
  useAgents.setState({ saving: false, update: updateAgent });
  useApps.setState({ apps: [github], loaded: true });
  usePlugins.setState({ plugins: [], loaded: true });
});
afterEach(cleanup);

test("capability switches save stable IDs in separate lists", () => {
  render(<AgentSkillsToolsTab detail={reviewerDetail} />);
  fireEvent.change(screen.getByRole("textbox", { name: "Skill ID" }), { target: { value: " systematic-debugging " } });
  fireEvent.click(screen.getByRole("button", { name: "Add skill" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Native tool ID" }), { target: { value: "glob" } });
  fireEvent.click(screen.getByRole("button", { name: "Add native tool" }));
  fireEvent.click(screen.getByRole("switch", { name: "Enable app github" }));
  fireEvent.click(screen.getByRole("button", { name: "Save skills and tools" }));
  expect(updateAgent).toHaveBeenCalledWith(
    "reviewer",
    expect.objectContaining({
      skills: ["requesting-code-review", "systematic-debugging"],
      nativeTools: ["read", "grep", "bash", "glob"],
      pluginTools: [],
      apps: ["github"],
    }),
  );
});

test("blank and duplicate IDs are rejected while removals are explicit", () => {
  render(<AgentSkillsToolsTab detail={reviewerDetail} />);
  const input = screen.getByRole("textbox", { name: "Skill ID" });
  fireEvent.change(input, { target: { value: "   " } });
  expect(screen.getByRole("button", { name: "Add skill" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(input, { target: { value: "requesting-code-review" } });
  expect(screen.getByRole("button", { name: "Add skill" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByText("requesting-code-review")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Remove skill requesting-code-review" }));
  expect(screen.queryByText("requesting-code-review")).toBeNull();
});

test("apps use the catalog and preserve unavailable YAML IDs in order", () => {
  render(<AgentSkillsToolsTab detail={{ ...reviewerDetail, apps: ["retired-app", "github"] }} />);
  expect(screen.getByRole("combobox", { name: "App catalog" })).toBeTruthy();
  const rows = screen.getByTestId("agent-app-rows");
  expect(rows.textContent).toContain("retired-appUnavailableGitHub");
  expect(screen.getByRole("button", { name: "Remove unavailable app retired-app" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Save skills and tools" }));
  expect(updateAgent).toHaveBeenCalledWith("reviewer", expect.objectContaining({ apps: ["retired-app", "github"] }));
});
