import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AddAppInput, AppInfo, PluginDetail, PluginInfo, PluginInstallBeginResult } from "@/bindings";

function plugin(id: string, categories: string[]): PluginInfo {
  return {
    id,
    name: id,
    description: "",
    icon: null,
    categories,
    verified: true,
    experimental: false,
    enabled: false,
    source: "catalog",
    capabilities: ["connector"],
    configured: false,
  };
}

const github = plugin("github", ["vcs", "issues"]);
const notion = plugin("notion", ["docs", "wiki", "productivity"]);
const builtin = {
  ...plugin("ollama", ["model-provider"]),
  source: "builtin" as const,
};

const githubApp: AppInfo = {
  id: "github",
  name: "GitHub",
  kind: "MCP server",
  initial: "G",
  color: "#111827",
  desc: "GitHub tools",
  transport: "stdio",
  command: "npx",
  args: ["-y", "@modelcontextprotocol/server-github"],
  url: null,
  scope: "global",
  scopeGateways: [],
  status: "connected",
  statusDetail: null,
  version: "1.0.0",
  publisher: "Acme",
  authKind: "none",
  authDetail: null,
  tools: [],
  agentAccess: [],
};

// Mutable fixture read by the `listApps` mock at call time. PluginsView's
// `hydrate` effect (unlike other views) is unconditional, so it always
// re-fetches on mount — tests set this before rendering instead of seeding
// `useApps` state directly (which hydrate would immediately overwrite).
let appsFixture: AppInfo[] = [];

const listApps = mock(async () => ({ status: "ok" as const, data: appsFixture }));
const addApp = mock(async (_input: AddAppInput) => ({ status: "ok" as const, data: appsFixture }));
const listPlugins = mock(async () => ({ status: "ok" as const, data: [github, notion, builtin] as PluginInfo[] }));

const listSkills = mock(async () => ({
  status: "ok" as const,
  data: [
    {
      id: "superpowers",
      name: "Superpowers",
      source: "superpowers",
      pluginId: null,
      installedAt: "2026-07-08T10:00:00Z",
      skillCount: 12,
    },
  ],
}));

type InstallSkillResponse =
  | {
      status: "ok";
      data: {
        id: string;
        name: string;
        source: string;
        pluginId: null;
        installedAt: string;
        skills: { id: string; name: string }[];
      };
    }
  | {
      status: "error";
      error: string;
    };

const installSkill = mock(
  async (_source: string): Promise<InstallSkillResponse> => ({
    status: "ok" as const,
    data: {
      id: "superpowers",
      name: "Superpowers",
      source: "superpowers",
      pluginId: null,
      installedAt: "2026-07-08T10:00:00Z",
      skills: [{ id: "superpowers:brainstorming", name: "brainstorming" }],
    },
  }),
);

const removeSkill = mock(async (_id: string) => ({
  status: "ok" as const,
  data: null,
}));

const refreshSkill = mock(async (_id: string) => ({
  status: "ok" as const,
  data: {
    id: "superpowers",
    name: "Superpowers",
    source: "superpowers",
    pluginId: null,
    installedAt: "2026-07-08T10:00:00Z",
    skills: [{ id: "superpowers:brainstorming", name: "brainstorming" }],
  },
}));

// The wizard mounts inside PluginsView, so its IPC surface needs benign
// defaults here: begin resolves to authKind "none" with no settings, which
// routes the wizard straight to done.
const wizardDetail: PluginDetail = {
  info: notion,
  auth: null,
  settings: [],
  mcp: [],
  models: [],
  homepage: null,
  publisher: "Notion",
};
const wizardBegin: PluginInstallBeginResult = {
  authKind: "none",
  envVarPresent: false,
  envVarName: null,
  oauthAvailable: false,
  oauthExternal: false,
  needsClientId: false,
  dcrSucceeded: false,
  callbackMode: "auto",
  oauthBegin: null,
  dcrError: null,
};
const pluginDetail = mock(async (_id: string) => ({ status: "ok" as const, data: wizardDetail }));
const beginPluginInstall = mock(async (_pluginId: string) => ({ status: "ok" as const, data: wizardBegin }));
const setPluginOauthClientId = mock(async (_pluginId: string, _clientId: string) => ({ status: "ok" as const, data: null }));
const cancelPluginInstall = mock(async (_pluginId: string, _stateToken: string | null) => ({ status: "ok" as const, data: null }));
const completePluginOauth = mock(async (_pluginId: string, _code: string, _stateToken: string) => ({
  status: "ok" as const,
  data: null,
}));
const setPluginSetting = mock(async (_key: string, _value: string) => ({ status: "ok" as const, data: null }));
const setPluginEnabled = mock(async (_id: string, _enabled: boolean) => ({ status: "ok" as const, data: null }));
const pluginOauthCompletedMsgListen = mock(
  async (_cb: (event: { payload: { pluginId: string; ok: boolean; error: string | null } }) => void) => () => {},
);

// Mock the Tauri IPC boundary before the component (and the real stores it
// pulls in) resolve "@/bindings"; the stores themselves are real zustand
// singletons, seeded/reset around each test below.
mock.module("@/bindings", () => ({
  events: {
    pluginOauthCompletedMsg: { listen: pluginOauthCompletedMsgListen },
  },
  commands: {
    listApps,
    addApp,
    listPlugins,
    listSkills,
    installSkill,
    removeSkill,
    refreshSkill,
    pluginDetail,
    beginPluginInstall,
    setPluginOauthClientId,
    cancelPluginInstall,
    completePluginOauth,
    setPluginSetting,
    setPluginEnabled,
  },
}));

const { useSkills } = await import("../store-skills");
const { useApps } = await import("@/store-apps");
const { usePlugins } = await import("@/store-plugins");
const { useRuntimes } = await import("@/store-runtimes");
const { useGateways } = await import("@/store-gateways");
const { useNav } = await import("@/store-nav");
const { filterByCategory, PluginsView } = await import("./PluginsView");

const all = [github, notion, builtin];

// Render and flush the mount-effect hydrates (apps via `hydrate`, skills
// store reset) inside act so their setState calls do not fire mid-assertion.
async function renderView() {
  render(<PluginsView />);
  await act(async () => {});
}

beforeEach(() => {
  appsFixture = [];
  listApps.mockClear();
  addApp.mockClear();
  listPlugins.mockClear();
  listSkills.mockClear();
  installSkill.mockClear();
  removeSkill.mockClear();
  refreshSkill.mockClear();
  pluginDetail.mockClear();
  beginPluginInstall.mockClear();
  setPluginOauthClientId.mockClear();
  cancelPluginInstall.mockClear();
  completePluginOauth.mockClear();
  setPluginSetting.mockClear();
  setPluginEnabled.mockClear();
  pluginOauthCompletedMsgListen.mockClear();
  useApps.setState({ apps: [], loaded: false, probing: null });
  usePlugins.setState({ plugins: [github, notion, builtin], loaded: true });
  useRuntimes.setState({ runtimes: [], loaded: false, refreshing: false, updating: {}, updateLog: {} });
  useGateways.setState({ gateways: [], eventsById: {}, loaded: false, probing: false });
  useNav.setState({ history: { back: [], current: { kind: "plugins" }, forward: [] } });
  useSkills.setState({ skills: [], loading: false, error: null });
});

// Reset the shared zustand singletons on the way out too: a later test file
// in the same bun process would otherwise inherit this file's fixtures.
afterEach(() => {
  cleanup();
  useApps.setState({ apps: [], loaded: false, probing: null });
  usePlugins.setState({ plugins: [], loaded: false });
  useRuntimes.setState({ runtimes: [], loaded: false, refreshing: false, updating: {}, updateLog: {} });
  useGateways.setState({ gateways: [], eventsById: {}, loaded: false, probing: false });
  useNav.setState({ history: { back: [], current: { kind: "home" }, forward: [] } });
});

test("renders the plugins heading and browse action", async () => {
  await renderView();

  expect(screen.getByRole("heading", { name: "Plugins" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add MCP server" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Browse plugins" })).toBeTruthy();
  expect(screen.getByText("No plugins installed yet. Add an MCP server by hand or browse plugins.")).toBeTruthy();
});

test("access tab uses plugin wording for installed MCP server controls", async () => {
  appsFixture = [githubApp];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Access" }));

  expect(screen.getByText("Plugin")).toBeTruthy();
  expect(screen.getByText("Access here applies before per-tool permissions — a blocked agent never sees the plugin's tools.")).toBeTruthy();
  expect(screen.queryByText("App")).toBeNull();
  expect(screen.queryByRole("button", { name: "Add app" })).toBeNull();
});

test("browse shows a pure catalog grid with no registry UI", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  expect(await screen.findByText("notion")).toBeTruthy();
  expect(screen.getAllByText("Catalog").length).toBeGreaterThan(0);
  expect(screen.queryByText("Registry")).toBeNull();
  expect(screen.queryByRole("textbox", { name: "Search the registry" })).toBeNull();
});

test("browse shows Install for unconfigured disabled catalog plugins and opens the wizard", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("notion");

  expect(screen.queryByRole("button", { name: "Open notion" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Install notion" }));

  expect(await screen.findByText("Install notion", { selector: "span" })).toBeTruthy();
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledWith("notion"));
});

test("single-flight guard: opening the wizard blocks background Install clicks on another plugin", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("notion");

  // Open the wizard for plugin A (github) via its Install button.
  fireEvent.click(screen.getByRole("button", { name: "Install github" }));
  await screen.findByText("Install github", { selector: "span" });
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledWith("github"));
  beginPluginInstall.mockClear();

  // The Modal scrim blocks mouse clicks on background content, but jsdom
  // has no focus trap and dispatches this click regardless — exactly
  // simulating a keyboard user tabbing to plugin B's card and pressing
  // Enter on its Install button while the wizard is open.
  fireEvent.click(screen.getByRole("button", { name: "Install notion" }));

  // installingPlugin must be unchanged: the wizard still shows plugin A's
  // header, and no begin call was made for plugin B.
  expect(screen.getByText("Install github", { selector: "span" })).toBeTruthy();
  expect(beginPluginInstall).not.toHaveBeenCalledWith("notion");
});

test("browse shows Open plus the enable switch for configured plugins", async () => {
  usePlugins.setState({ plugins: [{ ...github, configured: true }, notion, builtin], loaded: true });
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  expect(await screen.findByRole("button", { name: "Open github" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Install github" })).toBeNull();
  expect(screen.getByRole("switch", { name: "github enabled" })).toBeTruthy();
});

test("skills tab renders installed skills from listSkills", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));

  await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(1));
  expect((await screen.findAllByText("Superpowers")).length).toBeGreaterThan(0);
  expect(screen.getByText("superpowers")).toBeTruthy();
  expect(screen.getByText("12 skills")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Refresh Superpowers" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Remove Superpowers" })).toBeTruthy();
});

test("skills tab installs Superpowers from the curated action", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install Superpowers" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("superpowers"));
});

test("manual skill install preserves the typed source after a failed attempt", async () => {
  installSkill.mockImplementationOnce(async () => ({
    status: "error" as const,
    error: "network down",
  }));

  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  const input = screen.getByRole("textbox", { name: "Skill source" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install source" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("obra/superpowers"));
  expect(input.value).toBe("obra/superpowers");
});

test("manual skill install clears the typed source after a successful attempt", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  const input = screen.getByRole("textbox", { name: "Skill source" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install source" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("obra/superpowers"));
  await waitFor(() => expect(input.value).toBe(""));
});

test("filterByCategory passes every plugin through for the default all category", () => {
  expect(filterByCategory(all, "all").map((item) => item.id)).toEqual(["github", "notion", "ollama"]);
});

test("filterByCategory keeps only plugins whose categories include the picked one", () => {
  expect(filterByCategory(all, "docs").map((p) => p.id)).toEqual(["notion"]);
});

test("filterByCategory matches a plugin tagged with several categories from any one of them", () => {
  expect(filterByCategory(all, "issues").map((p) => p.id)).toEqual(["github"]);
  expect(filterByCategory(all, "wiki").map((p) => p.id)).toEqual(["notion"]);
});

test("filterByCategory returns an empty list when nothing matches", () => {
  expect(filterByCategory(all, "sandbox")).toEqual([]);
});
