import { test, expect, spyOn } from "bun:test";
import { usePlugins, browsePlugins, installedPlugins, summarizeUpdateAll, componentPluginIds } from "./store-plugins";
import { commands, type CatalogStatus, type ComponentReleaseDetail, type DoctorFinding, type PluginInfo } from "./bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

function reset() {
  usePlugins.setState({
    plugins: [],
    loaded: false,
    restartRequired: false,
    doctorFindings: [],
    doctorLoaded: false,
    catalogStatus: null,
    componentBootstrapStatus: null,
    componentPlugins: [],
    componentPluginsLoaded: false,
  });
}

function componentReleaseDetail(over: Partial<ComponentReleaseDetail> = {}): ComponentReleaseDetail {
  return {
    pluginId: "mimo",
    releases: [],
    activeVersion: null,
    activeManifest: null,
    ...over,
  };
}

// `load()` also calls `pluginsRestartRequired` — every test that exercises it
// (directly or via `setEnabled`/`update`/`pin`) needs this stubbed too, or the
// real Tauri IPC call throws outside a webview.
function stubRestartRequired(value = false) {
  return spyOn(commands, "pluginsRestartRequired").mockResolvedValue({ status: "ok", data: value });
}

// Same reasoning as `stubRestartRequired`: `load()` also best-effort fetches
// `catalog_status` for the Browse tab's status line.
function stubCatalogStatus(status: CatalogStatus | null = null) {
  return spyOn(commands, "catalogStatus").mockResolvedValue({
    status: "ok",
    data: status ?? { sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 },
  });
}

const builtin: PluginInfo = {
  id: "native",
  name: "Native",
  description: "Built-in native harness",
  icon: "cpu",
  categories: ["runtime"],
  slot: null,
  ownsSlot: false,
  verified: true,
  experimental: false,
  enabled: true,
  source: "builtin",
  capabilities: ["runtime"],
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
  catalogVersion: null,
  componentBacked: false,
  blockedReason: null,
};

const github: PluginInfo = {
  id: "github",
  name: "GitHub",
  description: "Repos, issues, PRs",
  icon: "github",
  categories: ["vcs"],
  slot: null,
  ownsSlot: false,
  verified: true,
  experimental: false,
  enabled: true,
  source: "component",
  capabilities: ["connector"],
  configured: false,
  kind: "integration",
  installed: true,
  family: null,
  pinned: false,
  sourceSpec: null,
  resolvedCommit: null,
  installedAt: null,
  updatedAt: null,
  trustTier: null,
  catalogVersion: null,
  componentBacked: false,
  blockedReason: null,
};

const skillPack: PluginInfo = {
  ...github,
  id: "acme",
  name: "Acme",
  source: "skill-pack",
  kind: "skill-pack",
};

const disabledCatalog: PluginInfo = {
  ...github,
  id: "linear",
  name: "Linear",
  enabled: false,
  installed: false,
};

test("load populates plugins from listPlugins", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [builtin, github] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();
  await usePlugins.getState().load();
  expect(spy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins.map((p) => p.id)).toEqual(["native", "github"]);
  expect(usePlugins.getState().loaded).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("load leaves plugins untouched and surfaces no crash on error", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();
  await usePlugins.getState().load();
  expect(usePlugins.getState().plugins).toEqual([]);
  expect(usePlugins.getState().loaded).toBe(false);
  spy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("load populates restartRequired from pluginsRestartRequired", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [] });
  const restartSpy = stubRestartRequired(true);
  const catalogSpy = stubCatalogStatus();
  await usePlugins.getState().load();
  expect(restartSpy).toHaveBeenCalled();
  expect(usePlugins.getState().restartRequired).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("load leaves restartRequired untouched when pluginsRestartRequired errors", async () => {
  reset();
  usePlugins.setState({ restartRequired: true });
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [] });
  const restartSpy = spyOn(commands, "pluginsRestartRequired").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  const catalogSpy = stubCatalogStatus();
  await usePlugins.getState().load();
  expect(usePlugins.getState().restartRequired).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("loadDoctor populates doctorFindings", async () => {
  reset();
  const findings: DoctorFinding[] = [
    { pluginId: "github", severity: "warn", kind: "attach-failed", message: "github failed to attach", suggestedAction: "Check github" },
  ];
  const spy = spyOn(commands, "pluginDoctor").mockResolvedValue({ status: "ok", data: findings });
  await usePlugins.getState().loadDoctor();
  expect(spy).toHaveBeenCalled();
  expect(usePlugins.getState().doctorFindings).toEqual(findings);
  expect(usePlugins.getState().doctorLoaded).toBe(true);
  spy.mockRestore();
});

test("setEnabled optimistically flips the flag, calls the command, then reconciles via reload", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const setSpy = spyOn(commands, "setPluginEnabled").mockResolvedValue({ status: "ok", data: null });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({
    status: "ok",
    data: [{ ...github, enabled: false }],
  });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  const p = usePlugins.getState().setEnabled("github", false);
  // Optimistic update lands synchronously before the awaited command resolves.
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  await p;

  expect(setSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "github", false);
  expect(listSpy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  setSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("setEnabled reloads (not crashes) when the command errors, so state reconciles with the server", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const setSpy = spyOn(commands, "setPluginEnabled").mockResolvedValue({ status: "error", error: { message: "denied" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [github] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  await usePlugins.getState().setEnabled("github", false);

  expect(setSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "github", false);
  expect(listSpy).toHaveBeenCalled();
  // Reload brought back the server truth (still enabled), undoing the optimistic flip.
  expect(usePlugins.getState().plugins[0].enabled).toBe(true);
  setSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("browsePlugins keeps only not-installed entries", () => {
  expect(browsePlugins([builtin, github, skillPack, disabledCatalog]).map((p) => p.id)).toEqual(["native", "linear"]);
});

test("installedPlugins keeps only installed entries", () => {
  expect(installedPlugins([builtin, github, skillPack]).map((p) => p.id)).toEqual(["github", "acme"]);
});

test("uninstall swaps in the returned list", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const spy = spyOn(commands, "uninstallPlugin").mockResolvedValue({
    status: "ok",
    data: [{ ...github, installed: false, enabled: false, configured: false }],
  });
  const ok = await usePlugins.getState().uninstall("github");
  expect(ok).toBe(true);
  expect(usePlugins.getState().plugins[0].installed).toBe(false);
  spy.mockRestore();
});

test("uninstall failure toasts and keeps state", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const spy = spyOn(commands, "uninstallPlugin").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  const ok = await usePlugins.getState().uninstall("github");
  expect(ok).toBe(false);
  expect(usePlugins.getState().plugins[0].installed).toBe(true);
  spy.mockRestore();
});

test("update calls updatePlugin with force and reloads on an `updated` outcome", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const updateSpy = spyOn(commands, "updatePlugin").mockResolvedValue({ status: "ok", data: { kind: "updated" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  await usePlugins.getState().update("acme", true);

  expect(updateSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "acme", true);
  expect(listSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("update refreshes cached doctor findings when they were already loaded", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true, doctorFindings: [], doctorLoaded: true });
  const updateSpy = spyOn(commands, "updatePlugin").mockResolvedValue({ status: "ok", data: { kind: "alreadyCurrent" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();
  const doctorSpy = spyOn(commands, "pluginDoctor").mockResolvedValue({ status: "ok", data: [] });

  await usePlugins.getState().update("acme", false);

  expect(doctorSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
  doctorSpy.mockRestore();
});

test("update toasts the error and still reloads when updatePlugin itself errors", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const updateSpy = spyOn(commands, "updatePlugin").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  await usePlugins.getState().update("acme", false);

  expect(listSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("pin optimistically flips the plugin's pinned flag, calls the command, then reconciles via reload", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const pinSpy = spyOn(commands, "setPluginPin").mockResolvedValue({ status: "ok", data: null });
  const listSpy = spyOn(commands, "listPlugins");
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  listSpy.mockResolvedValueOnce({ status: "ok", data: [{ ...skillPack, pinned: true }] });
  const p = usePlugins.getState().pin("acme", true, "vendored fork");
  // Optimistic update lands synchronously before the awaited command resolves.
  expect(usePlugins.getState().plugins[0].pinned).toBe(true);
  await p;

  expect(pinSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "acme", true, "vendored fork");
  expect(listSpy).toHaveBeenCalled();
  // The source of truth after reload is the server's `pinned` ledger flag,
  // not the transient optimistic paint.
  expect(usePlugins.getState().plugins[0].pinned).toBe(true);

  listSpy.mockResolvedValueOnce({ status: "ok", data: [{ ...skillPack, pinned: false }] });
  await usePlugins.getState().pin("acme", false);
  expect(pinSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "acme", false, null);
  expect(usePlugins.getState().plugins[0].pinned).toBe(false);

  pinSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("pin failure toasts and reloads, reconciling the flag back to the server's (unchanged) value", async () => {
  reset();
  // skillPack.pinned is false; the write below fails, so it should never
  // have actually flipped server-side — the post-reload value stays false.
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const pinSpy = spyOn(commands, "setPluginPin").mockResolvedValue({ status: "error", error: { message: "denied" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  await usePlugins.getState().pin("acme", true);

  expect(usePlugins.getState().plugins[0].pinned).toBe(false);
  pinSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("load carries a persisted pinned flag straight from the server — no pin() call needed for it to survive a reload", async () => {
  reset();
  const pinnedFixture: PluginInfo = {
    ...skillPack,
    pinned: true,
    sourceSpec: "https://github.com/acme/pack",
    resolvedCommit: "deadbeefcafe",
    trustTier: "acknowledged",
  };
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [pinnedFixture] });
  const restartSpy = stubRestartRequired();
  const catalogSpy = stubCatalogStatus();

  await usePlugins.getState().load();

  const reloaded = usePlugins.getState().plugins[0];
  expect(reloaded.pinned).toBe(true);
  expect(reloaded.sourceSpec).toBe("https://github.com/acme/pack");
  expect(reloaded.trustTier).toBe("acknowledged");

  spy.mockRestore();
  restartSpy.mockRestore();
  catalogSpy.mockRestore();
});

test("summarizeUpdateAll counts updated/needsReack/failed outcomes", () => {
  const summary = summarizeUpdateAll([
    { id: "a", outcome: { kind: "updated" } },
    { id: "b", outcome: { kind: "updated" } },
    { id: "c", outcome: { kind: "failed", detail: "boom" } },
    { id: "d", outcome: { kind: "needsReack" } },
    { id: "e", outcome: { kind: "alreadyCurrent" } },
  ]);
  expect(summary).toBe("2 updated, 1 need re-review, 1 failed");
});

// ---------- Component-plugin (WASM bundle) release management — Task 12 ----------

test("componentPluginIds derives ids from the plugin list, not a hardcoded set", () => {
  const plugins = [
    { id: "github", source: "component" },
    { id: "anthropic", source: "builtin" },
    { id: "discord", source: "component" },
    { id: "superpowers", source: "skill-pack" },
  ];
  expect(componentPluginIds(plugins)).toEqual(["github", "discord"]);
});

test("componentPluginIds is empty when no component plugin is registered", () => {
  expect(componentPluginIds([{ id: "anthropic", source: "builtin" }])).toEqual([]);
});

test("loadComponentBootstrapStatus populates componentBootstrapStatus", async () => {
  reset();
  const spy = spyOn(commands, "componentBootstrapStatus").mockResolvedValue({
    status: "ok",
    data: { pending: true, message: "network unreachable" },
  });

  await usePlugins.getState().loadComponentBootstrapStatus();

  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER);
  expect(usePlugins.getState().componentBootstrapStatus).toEqual({ pending: true, message: "network unreachable" });
  spy.mockRestore();
});

test("loadComponentBootstrapStatus leaves the prior status untouched on error", async () => {
  reset();
  usePlugins.setState({ componentBootstrapStatus: { pending: false, message: null } });
  const spy = spyOn(commands, "componentBootstrapStatus").mockResolvedValue({ status: "error", error: { message: "boom" } });

  await usePlugins.getState().loadComponentBootstrapStatus();

  expect(usePlugins.getState().componentBootstrapStatus).toEqual({ pending: false, message: null });
  spy.mockRestore();
});

/** Seed the plugin list with the component-sourced rows the release-management
 *  actions derive their id list from (`componentPluginIds`). */
function seedComponentPlugins() {
  usePlugins.setState({
    plugins: [
      { ...github, id: "mimo", name: "MiMo", source: "component" },
      { ...github, id: "opencode", name: "OpenCode", source: "component" },
    ],
  });
}

test("loadComponentPlugins fetches every component id and keeps only the ok results", async () => {
  reset();
  seedComponentPlugins();
  const spy = spyOn(commands, "pluginReleaseDetail").mockImplementation(async (_runnerId, id) => {
    if (id === "mimo") return { status: "ok", data: componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" }) };
    return { status: "error", error: { message: "boom" } };
  });

  await usePlugins.getState().loadComponentPlugins();

  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo");
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "opencode");
  expect(usePlugins.getState().componentPlugins).toEqual([componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" })]);
  expect(usePlugins.getState().componentPluginsLoaded).toBe(true);
  spy.mockRestore();
});

test("pluginReleaseDetail returns the fetched detail", async () => {
  reset();
  const detail = componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.2.0" });
  const spy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "ok", data: detail });

  const result = await usePlugins.getState().pluginReleaseDetail("mimo");

  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo");
  expect(result).toEqual(detail);
  spy.mockRestore();
});

test("pluginReleaseDetail toasts and returns null on error", async () => {
  reset();
  const spy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "error", error: { message: "boom" } });

  const result = await usePlugins.getState().pluginReleaseDetail("mimo");

  expect(result).toBeNull();
  spy.mockRestore();
});

test("installComponentPlugin installs, reloads componentPlugins, and returns the release detail", async () => {
  reset();
  seedComponentPlugins();
  const installed = componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.2.0" });
  const installSpy = spyOn(commands, "installComponentPlugin").mockResolvedValue({ status: "ok", data: installed });
  const detailSpy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "ok", data: installed });

  const result = await usePlugins.getState().installComponentPlugin("mimo");

  expect(installSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo", null);
  expect(result).toEqual(installed);
  expect(usePlugins.getState().componentPlugins).toEqual([installed, installed]);
  expect(usePlugins.getState().componentPluginsLoaded).toBe(true);
  installSpy.mockRestore();
  detailSpy.mockRestore();
});

test("installComponentPlugin passes an explicit version through", async () => {
  reset();
  const installSpy = spyOn(commands, "installComponentPlugin").mockResolvedValue({
    status: "ok",
    data: componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" }),
  });
  const detailSpy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({
    status: "ok",
    data: componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" }),
  });

  await usePlugins.getState().installComponentPlugin("mimo", "0.1.0");

  expect(installSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo", "0.1.0");
  installSpy.mockRestore();
  detailSpy.mockRestore();
});

test("installComponentPlugin toasts the error and returns null without touching componentPlugins", async () => {
  reset();
  const installSpy = spyOn(commands, "installComponentPlugin").mockResolvedValue({
    status: "error",
    error: { message: "disabled until…" },
  });

  const result = await usePlugins.getState().installComponentPlugin("mimo");

  expect(result).toBeNull();
  expect(usePlugins.getState().componentPluginsLoaded).toBe(false);
  installSpy.mockRestore();
});

test("rollbackComponentPlugin dispatches from/to versions, reloads componentPlugins, and returns the release detail", async () => {
  reset();
  const rolledBack = componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" });
  const rollbackSpy = spyOn(commands, "rollbackComponentPlugin").mockResolvedValue({ status: "ok", data: rolledBack });
  const detailSpy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "ok", data: rolledBack });

  const result = await usePlugins.getState().rollbackComponentPlugin("mimo", "0.2.0", "0.1.0");

  expect(rollbackSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo", "0.2.0", "0.1.0");
  expect(result).toEqual(rolledBack);
  expect(usePlugins.getState().componentPluginsLoaded).toBe(true);
  rollbackSpy.mockRestore();
  detailSpy.mockRestore();
});

test("rollbackComponentPlugin toasts the error and returns null", async () => {
  reset();
  const rollbackSpy = spyOn(commands, "rollbackComponentPlugin").mockResolvedValue({
    status: "error",
    error: { message: "no such version" },
  });

  const result = await usePlugins.getState().rollbackComponentPlugin("mimo", "0.2.0", "9.9.9");

  expect(result).toBeNull();
  rollbackSpy.mockRestore();
});

test("retryComponentBootstrap installs every known first-party id, then reloads status and componentPlugins", async () => {
  reset();
  seedComponentPlugins();
  const installSpy = spyOn(commands, "installComponentPlugin").mockResolvedValue({
    status: "ok",
    data: componentReleaseDetail(),
  });
  const detailSpy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "ok", data: componentReleaseDetail() });
  const statusSpy = spyOn(commands, "componentBootstrapStatus").mockResolvedValue({
    status: "ok",
    data: { pending: false, message: null },
  });

  await usePlugins.getState().retryComponentBootstrap();

  expect(installSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "mimo", null);
  expect(installSpy).toHaveBeenCalledWith(LOCAL_RUNNER, "opencode", null);
  expect(statusSpy).toHaveBeenCalled();
  expect(usePlugins.getState().componentBootstrapStatus).toEqual({ pending: false, message: null });
  installSpy.mockRestore();
  detailSpy.mockRestore();
  statusSpy.mockRestore();
});

test("retryComponentBootstrap tolerates a per-id install failure and still refreshes the pending status", async () => {
  reset();
  seedComponentPlugins();
  const installSpy = spyOn(commands, "installComponentPlugin").mockImplementation(async (_runnerId, id) => {
    if (id === "mimo") return { status: "ok", data: componentReleaseDetail({ pluginId: "mimo", activeVersion: "0.1.0" }) };
    return { status: "error", error: { message: "still unreachable" } };
  });
  const detailSpy = spyOn(commands, "pluginReleaseDetail").mockResolvedValue({ status: "ok", data: componentReleaseDetail() });
  const statusSpy = spyOn(commands, "componentBootstrapStatus").mockResolvedValue({
    status: "ok",
    data: { pending: true, message: "opencode still unreachable" },
  });

  await usePlugins.getState().retryComponentBootstrap();

  expect(usePlugins.getState().componentBootstrapStatus).toEqual({ pending: true, message: "opencode still unreachable" });
  installSpy.mockRestore();
  detailSpy.mockRestore();
  statusSpy.mockRestore();
});
