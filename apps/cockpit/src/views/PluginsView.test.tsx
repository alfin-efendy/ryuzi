import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AddAppInput, AppInfo, CatalogStatus, PluginDetail, PluginInfo, PluginInstallBeginResult } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

function plugin(id: string, categories: string[], over: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id,
    name: id,
    description: "",
    icon: null,
    categories,
    slot: null,
    ownsSlot: false,
    verified: true,
    experimental: false,
    enabled: false,
    source: "catalog",
    capabilities: ["connector"],
    configured: false,
    kind: "integration",
    installed: false,
    family: null,
    pinned: false,
    sourceSpec: null,
    resolvedCommit: null,
    installedAt: null,
    updatedAt: null,
    trustTier: null,
    catalogSource: null,
    catalogVersion: null,
    blockedReason: null,
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
let doctorFindingsFixture: { pluginId: string; severity: string; kind: string; message: string; suggestedAction: string }[] = [];
let catalogStatusFixture: CatalogStatus = { sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 };

const listApps = mock(async () => ({ status: "ok" as const, data: appsFixture }));
const addApp = mock(async (_runnerId: string, _input: AddAppInput) => ({ status: "ok" as const, data: appsFixture }));
const listPlugins = mock(async () => ({ status: "ok" as const, data: pluginsFixture }));
const uninstallPlugin = mock(async (_runnerId: string, id: string) => ({
  status: "ok" as const,
  data: pluginsFixture.filter((p) => p.id !== id),
}));
const pluginsRestartRequired = mock(async () => ({ status: "ok" as const, data: false }));
const catalogStatus = mock(async () => ({ status: "ok" as const, data: catalogStatusFixture }));
// Simulates a real (verified) fetch by default — matches the RPC's own
// `refresh_catalog` behavior of returning the fresh `catalog_status` snapshot.
const refreshCatalog = mock(async () => ({ status: "ok" as const, data: catalogStatusFixture }));
const pluginDoctor = mock(async () => ({ status: "ok" as const, data: doctorFindingsFixture }));
const updatePlugin = mock(async (_runnerId: string, _id: string, _force: boolean) => ({
  status: "ok" as const,
  data: { kind: "updated" as const },
}));
const updateAllPlugins = mock(async (_runnerId: string) => ({
  status: "ok" as const,
  data: [] as { id: string; outcome: { kind: string } }[],
}));
// Mutates the fixture list that `listPlugins` reads from, so `pin()`'s
// internal reload comes back with the real persisted flag — the same
// authoritative-reload behavior being exercised, not a session-only mock.
const setPluginPin = mock(async (_runnerId: string, id: string, pinned: boolean, _reason: string | null) => {
  pluginsFixture = pluginsFixture.map((p) => (p.id === id ? { ...p, pinned } : p));
  return { status: "ok" as const, data: null };
});

const beginSkillInstall = mock(async (_runnerId: string, _source: string) => ({
  status: "ok" as const,
  data: {
    completed: true,
    trust: null,
    plugin: {
      id: "superpowers",
      name: "Superpowers",
      source: "superpowers",
      pluginId: null,
      installedAt: "2026-07-08T10:00:00Z",
      skills: [{ id: "superpowers:brainstorming", name: "brainstorming" }],
    },
  },
}));
const confirmSkillInstall = mock(async (_runnerId: string, _token: string) => ({
  status: "ok" as const,
  data: {
    id: "superpowers",
    name: "Superpowers",
    source: "superpowers",
    pluginId: null,
    installedAt: "2026-07-08T10:00:00Z",
    skills: [] as { id: string; name: string }[],
  },
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

const removeSkill = mock(async (_runnerId: string, _id: string) => ({
  status: "ok" as const,
  data: null,
}));

const refreshSkill = mock(async (_runnerId: string, _id: string) => ({
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
const pluginDetail = mock(async (_runnerId: string, _id: string) => ({ status: "ok" as const, data: wizardDetail }));
const beginPluginInstall = mock(async (_runnerId: string, _pluginId: string) => ({ status: "ok" as const, data: wizardBegin }));
const setPluginOauthClientId = mock(async (_runnerId: string, _pluginId: string, _clientId: string) => ({
  status: "ok" as const,
  data: null,
}));
const cancelPluginInstall = mock(async (_runnerId: string, _pluginId: string, _stateToken: string | null) => ({
  status: "ok" as const,
  data: null,
}));
const completePluginOauth = mock(async (_runnerId: string, _pluginId: string, _code: string, _stateToken: string) => ({
  status: "ok" as const,
  data: null,
}));
const setPluginSetting = mock(async (_runnerId: string, _key: string, _value: string) => ({ status: "ok" as const, data: null }));
const setPluginEnabled = mock(async (_runnerId: string, _id: string, _enabled: boolean) => ({ status: "ok" as const, data: null }));
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
    pluginsRestartRequired,
    catalogStatus,
    refreshCatalog,
    pluginDoctor,
    updatePlugin,
    updateAllPlugins,
    setPluginPin,
    beginSkillInstall,
    confirmSkillInstall,
    listSkills,
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
// `refreshCatalog`'s store action toasts the outcome — mock the boundary
// (matches `DoctorPanel.test.tsx`/`SkillInstallModal.test.tsx`'s convention)
// so tests can assert on it instead of exercising real sonner DOM state.
const toastSuccess = mock((_message: string) => {});
const toastWarning = mock((_message: string) => {});
const toastError = mock((_message: string) => {});
mock.module("sonner", () => ({
  toast: { success: toastSuccess, warning: toastWarning, error: toastError, info: mock(() => {}) },
  Toaster: () => null,
}));

const { useSkills } = await import("../store-skills");
const { useApps } = await import("@/store-apps");
const { usePlugins } = await import("@/store-plugins");
const { useGateways } = await import("@/store-gateways");
const { useNav } = await import("@/store-nav");
const { filterByCategory, PluginsView } = await import("./PluginsView");

const all = [plugin("github", ["vcs", "issues"]), plugin("notion", ["docs", "wiki", "productivity"]), plugin("ollama", ["model-provider"])];

// Render and flush the mount-effect fetches (apps via `hydrate`, plugins via
// `load`, doctor via `loadDoctor`, skills refresh) inside act so their
// setState calls do not fire mid-assertion.
async function renderView() {
  render(<PluginsView />);
  await act(async () => {});
}

function resetPluginsStore() {
  usePlugins.setState({
    plugins: [],
    loaded: false,
    restartRequired: false,
    doctorFindings: [],
    doctorLoaded: false,
    catalogStatus: null,
  });
}

beforeEach(() => {
  appsFixture = [];
  pluginsFixture = [github, notion];
  doctorFindingsFixture = [];
  catalogStatusFixture = { sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 };
  listApps.mockClear();
  addApp.mockClear();
  listPlugins.mockClear();
  uninstallPlugin.mockClear();
  pluginsRestartRequired.mockClear();
  catalogStatus.mockClear();
  refreshCatalog.mockClear();
  pluginDoctor.mockClear();
  updatePlugin.mockClear();
  updateAllPlugins.mockClear();
  setPluginPin.mockClear();
  beginSkillInstall.mockClear();
  confirmSkillInstall.mockClear();
  listSkills.mockClear();
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
  toastSuccess.mockClear();
  toastWarning.mockClear();
  toastError.mockClear();
  useApps.setState({ apps: [], loaded: false, probing: null });
  resetPluginsStore();
  useGateways.setState({ gateways: [], eventsById: {}, loaded: false, probing: false });
  useNav.setState({ history: { back: [], current: { kind: "plugins" }, forward: [] } });
  useSkills.setState({ skills: [], loading: false, error: null });
});

// Reset the shared zustand singletons on the way out too: a later test file
// in the same bun process would otherwise inherit this file's fixtures.
afterEach(() => {
  cleanup();
  useApps.setState({ apps: [], loaded: false, probing: null });
  resetPluginsStore();
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

  expect(await screen.findByText("Install github", { selector: "h2" })).toBeTruthy();
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "github"));
});

test("browse install routes a provider to the connection modal", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("anthropic");

  fireEvent.click(screen.getByRole("button", { name: "Install anthropic" }));

  expect((await screen.findAllByText("Add account")).length).toBeGreaterThan(0);
});

test("browse install routes a skill pack through the two-phase trust flow (beginSkillInstall)", async () => {
  pluginsFixture = [github, anthropic, superpowers];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install superpowers" }));

  await waitFor(() => expect(beginSkillInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "superpowers"));
  // Curated packs resolve `completed: true` — no trust step, no old
  // `install_skill` one-phase command involved.
  await waitFor(() => expect(listPlugins.mock.calls.length).toBeGreaterThan(1));
});

test("Add skill source opens the manual source-entry step of the trust-gated install flow", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Add skill source" }));

  expect(await screen.findByLabelText("Skill source")).toBeTruthy();
  expect(beginSkillInstall).not.toHaveBeenCalled();
});

test("installed aggregates apps and installed plugins with uninstall", async () => {
  appsFixture = [githubApp];
  pluginsFixture = [notion];
  await renderView();

  expect(await screen.findByText("GitHub")).toBeTruthy();
  expect(screen.getByText("notion")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Uninstall notion" }));

  await waitFor(() => expect(uninstallPlugin).toHaveBeenCalledWith(LOCAL_RUNNER, "notion"));
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

test("an installed skill pack shows Update/Pin actions, which call the store's commands", async () => {
  const installedPack = { ...superpowers, installed: true };
  pluginsFixture = [installedPack];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Update superpowers" }));
  await waitFor(() => expect(updatePlugin).toHaveBeenCalledWith(LOCAL_RUNNER, "superpowers", false));

  fireEvent.click(screen.getByRole("button", { name: "Pin superpowers" }));
  await waitFor(() => expect(setPluginPin).toHaveBeenCalledWith(LOCAL_RUNNER, "superpowers", true, "Pinned from Cockpit"));

  expect(await screen.findByText("Pinned")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Unpin superpowers" })).toBeTruthy();
});

test("a pinned skill pack's Pinned pill/button reflects info.pinned straight from listPlugins — no pin() call, so it survives a reload", async () => {
  // Simulates the persisted ledger state coming back on a fresh load (e.g.
  // after an app restart) rather than through this session's `pin()` call.
  const pinnedPack = { ...superpowers, installed: true, pinned: true };
  pluginsFixture = [pinnedPack];
  await renderView();

  expect(await screen.findByText("Pinned")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Unpin superpowers" })).toBeTruthy();
  expect(setPluginPin).not.toHaveBeenCalled();

  // A second reload (`usePlugins.load()`, same fixture) keeps it pinned.
  await act(async () => {
    await usePlugins.getState().load();
  });
  expect(screen.getByText("Pinned")).toBeTruthy();
});

test("a non-skill-pack installed plugin shows no Update/Pin actions", async () => {
  pluginsFixture = [notion];
  await renderView();

  expect(await screen.findByText("notion")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Update notion$/ })).toBeNull();
  expect(screen.queryByRole("button", { name: /^Pin notion$/ })).toBeNull();
});

test("an installed plugin with an attach-failed doctor finding shows the Attach failed pill", async () => {
  pluginsFixture = [notion];
  doctorFindingsFixture = [
    { pluginId: "notion", severity: "warn", kind: "attach-failed", message: "notion failed to attach", suggestedAction: "Check notion" },
  ];
  await renderView();

  expect(await screen.findByText("Attach failed")).toBeTruthy();
});

test("the doctor chip reflects the finding count and opens the doctor panel", async () => {
  doctorFindingsFixture = [
    { pluginId: "notion", severity: "warn", kind: "attach-failed", message: "notion failed to attach", suggestedAction: "Check notion" },
  ];
  await renderView();

  expect(await screen.findByRole("button", { name: "1 issue" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "1 issue" }));
  expect(await screen.findByText("Plugin doctor")).toBeTruthy();
  expect(screen.getAllByText("notion failed to attach").length).toBeGreaterThan(0);
});

test("the doctor chip reads Doctor: OK when there are no findings", async () => {
  await renderView();
  expect(await screen.findByRole("button", { name: "Doctor: OK" })).toBeTruthy();
});

test("Update all is disabled with no installed skill packs, and calls updateAllPlugins when enabled", async () => {
  pluginsFixture = [notion];
  await renderView();

  const updateAllBtn = (await screen.findByRole("button", { name: "Update all" })) as HTMLButtonElement;
  expect(updateAllBtn.disabled).toBe(true);

  pluginsFixture = [notion, { ...superpowers, installed: true }];
  await act(async () => {
    await usePlugins.getState().load();
  });

  const enabled = screen.getByRole("button", { name: "Update all" }) as HTMLButtonElement;
  expect(enabled.disabled).toBe(false);

  fireEvent.click(enabled);
  await waitFor(() => expect(updateAllPlugins).toHaveBeenCalled());
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

test("Browse's Refresh catalog button calls refreshCatalog and toasts the outcome", async () => {
  catalogStatusFixture = { sequence: 4, lastFetchAt: 1_700_000_000_000, outcome: "ok", entries: 12, blocked: 1 };
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("github");

  fireEvent.click(screen.getByRole("button", { name: "Refresh catalog" }));

  await waitFor(() => expect(refreshCatalog).toHaveBeenCalled());
  await waitFor(() => expect(toastSuccess).toHaveBeenCalled());
});

test("Browse shows a subtle catalog status line once catalogStatus has loaded", async () => {
  catalogStatusFixture = { sequence: 4, lastFetchAt: 1_700_000_000_000, outcome: "ok", entries: 12, blocked: 1 };
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  expect(await screen.findByText(/Catalog seq 4/)).toBeTruthy();
});

test("a blocked catalog entry renders the Blocked badge and hides the Install button", async () => {
  const blockedPlugin = plugin("evil-plugin", ["vcs"], { blockedReason: "revoked: known-malicious update" });
  pluginsFixture = [blockedPlugin];
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  expect(await screen.findByText("Blocked")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Install evil-plugin" })).toBeNull();
});
