import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AddAppInput, AppInfo, PluginDetail, PluginInfo, PluginInstallBeginResult } from "@/bindings";

function plugin(id: string, categories: string[], over: Partial<PluginInfo> = {}): PluginInfo {
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
    kind: "integration",
    installed: false,
    family: null,
    ...over,
  };
}

const github = plugin("github", ["vcs", "issues"]);
const notion = plugin("notion", ["docs"], { installed: true });
const anthropic = plugin("anthropic", ["model-provider"], { kind: "provider", family: "anthropic", source: "builtin" });
const superpowers = plugin("superpowers", ["skills"], { kind: "skill-pack", source: "skill-pack" });

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

// Mutable fixtures read by the mocks at call time. PluginsView's `hydrate`
// (apps) and `load` (plugins) effects re-fetch on mount, so tests set these
// before rendering instead of seeding store state directly.
let appsFixture: AppInfo[] = [];
let pluginsFixture: PluginInfo[] = [github, notion];

const listApps = mock(async () => ({ status: "ok" as const, data: appsFixture }));
const addApp = mock(async (_input: AddAppInput) => ({ status: "ok" as const, data: appsFixture }));
const listPlugins = mock(async () => ({ status: "ok" as const, data: pluginsFixture }));
const uninstallPlugin = mock(async (id: string) => ({
  status: "ok" as const,
  data: pluginsFixture.filter((p) => p.id !== id),
}));

const listSkills = mock(async () => ({
  status: "ok" as const,
  data: [] as {
    id: string;
    name: string;
    source: string;
    pluginId: string | null;
    installedAt: string;
    skillCount: number;
  }[],
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
// AddConnectionModal (mounted for provider installs) subscribes to the OAuth
// authorize-url stream on open; a no-op listener keeps that effect happy.
const oauthAuthorizeUrlMsgListen = mock(async (_cb: (event: unknown) => void) => () => {});

// Mock the Tauri IPC boundary before the component (and the real stores it
// pulls in) resolve "@/bindings"; the stores themselves are real zustand
// singletons, seeded/reset around each test below.
mock.module("@/bindings", () => ({
  events: {
    pluginOauthCompletedMsg: { listen: pluginOauthCompletedMsgListen },
    oauthAuthorizeUrlMsg: { listen: oauthAuthorizeUrlMsgListen },
  },
  commands: {
    listApps,
    addApp,
    listPlugins,
    uninstallPlugin,
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
const { useGateways } = await import("@/store-gateways");
const { useNav } = await import("@/store-nav");
const { filterByCategory, PluginsView } = await import("./PluginsView");

const all = [plugin("github", ["vcs", "issues"]), plugin("notion", ["docs", "wiki", "productivity"]), plugin("ollama", ["model-provider"])];

// Render and flush the mount-effect fetches (apps via `hydrate`, plugins via
// `load`, skills refresh) inside act so their setState calls do not fire
// mid-assertion.
async function renderView() {
  render(<PluginsView />);
  await act(async () => {});
}

beforeEach(() => {
  appsFixture = [];
  pluginsFixture = [github, notion];
  listApps.mockClear();
  addApp.mockClear();
  listPlugins.mockClear();
  uninstallPlugin.mockClear();
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
  oauthAuthorizeUrlMsgListen.mockClear();
  useApps.setState({ apps: [], loaded: false, probing: null });
  usePlugins.setState({ plugins: [], loaded: false });
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
  useGateways.setState({ gateways: [], eventsById: {}, loaded: false, probing: false });
  useNav.setState({ history: { back: [], current: { kind: "home" }, forward: [] } });
  useSkills.setState({ skills: [], loading: false, error: null });
});

test("has exactly two tabs and no Access/Skills", async () => {
  await renderView();

  expect(screen.getByRole("button", { name: "Installed" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Browse" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Access" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Skills" })).toBeNull();
});

test("header shows Add MCP server and Add skill source, not Browse plugins", async () => {
  await renderView();

  expect(screen.getByRole("heading", { name: "Plugins" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add MCP server" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add skill source" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Browse plugins" })).toBeNull();
});

test("browse lists only not-installed entries", async () => {
  pluginsFixture = [github, notion];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  expect(await screen.findByText("github")).toBeTruthy();
  expect(screen.queryByText("notion")).toBeNull();
});

test("browse install routes an integration to the install wizard", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("github");

  fireEvent.click(screen.getByRole("button", { name: "Install github" }));

  expect(await screen.findByText("Install github", { selector: "span" })).toBeTruthy();
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledWith("github"));
});

test("browse install routes a provider to the connection modal", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("anthropic");

  fireEvent.click(screen.getByRole("button", { name: "Install anthropic" }));

  expect((await screen.findAllByText("Add account")).length).toBeGreaterThan(0);
});

test("browse install routes a skill pack to installSource", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install superpowers" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("superpowers"));
});

test("browse skill-pack install is guarded while skills are loading", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  // A skills install is already in flight: installSource sets loading:true
  // synchronously at the start, so a rapid second click must be a no-op.
  act(() => {
    useSkills.setState({ loading: true });
  });

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install superpowers" }));

  await act(async () => {});
  expect(installSkill).not.toHaveBeenCalled();
});

test("browse skill-pack install fires when skills are not loading", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  act(() => {
    useSkills.setState({ loading: false });
  });

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install superpowers" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("superpowers"));
});

test("installed aggregates apps and installed plugins with uninstall", async () => {
  appsFixture = [githubApp];
  pluginsFixture = [notion];
  await renderView();

  expect(await screen.findByText("GitHub")).toBeTruthy();
  expect(screen.getByText("notion")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Uninstall notion" }));

  await waitFor(() => expect(uninstallPlugin).toHaveBeenCalledWith("notion"));
});

test("an installed curated pack renders as exactly one card, not a duplicate manual row", async () => {
  // The skills store and the plugins list both know about superpowers once
  // it's installed (same id). The Installed tab must show it once — as the
  // plugin card with an Uninstall button — and NOT also as a manual "Skill
  // sources" row (whose id/pluginId matches a listed plugin, so it's filtered
  // out).
  const installedPack = { ...superpowers, installed: true };
  pluginsFixture = [installedPack];
  useSkills.setState({
    skills: [
      {
        id: "superpowers",
        name: "superpowers",
        source: "superpowers",
        pluginId: null,
        installedAt: "2026-07-08T10:00:00Z",
        skillCount: 12,
      },
    ],
    loading: false,
    error: null,
  });
  await renderView();

  expect(await screen.findByText("superpowers")).toBeTruthy();
  // Exactly one node bears the pack name (the InstalledPluginCard), and it
  // carries an Uninstall action — the manual "Skill sources" card would add a
  // Remove button and a second name node.
  expect(screen.getAllByText("superpowers")).toHaveLength(1);
  expect(screen.getByRole("button", { name: "Uninstall superpowers" })).toBeTruthy();
  expect(screen.queryByText("Skill sources")).toBeNull();
  expect(screen.queryByRole("button", { name: "Remove superpowers" })).toBeNull();
});

test("empty installed state points at browse", async () => {
  appsFixture = [];
  pluginsFixture = [];
  await renderView();

  expect(await screen.findByText("Nothing installed yet. Browse plugins or add an MCP server by hand.")).toBeTruthy();
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
