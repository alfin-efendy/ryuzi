import { test, expect, spyOn } from "bun:test";
import { usePlugins, browsePlugins, installedPlugins, summarizeUpdateAll } from "./store-plugins";
import { commands, type DoctorFinding, type PluginInfo } from "./bindings";

function reset() {
  usePlugins.setState({
    plugins: [],
    loaded: false,
    restartRequired: false,
    doctorFindings: [],
    doctorLoaded: false,
    pinnedIds: new Set(),
  });
}

// `load()` also calls `pluginsRestartRequired` — every test that exercises it
// (directly or via `setEnabled`/`update`/`pin`) needs this stubbed too, or the
// real Tauri IPC call throws outside a webview.
function stubRestartRequired(value = false) {
  return spyOn(commands, "pluginsRestartRequired").mockResolvedValue({ status: "ok", data: value });
}

const builtin: PluginInfo = {
  id: "native",
  name: "Native",
  description: "Built-in native harness",
  icon: "cpu",
  categories: ["runtime"],
  verified: true,
  experimental: false,
  enabled: true,
  source: "builtin",
  capabilities: ["runtime"],
  configured: false,
  kind: "integration",
  installed: false,
  family: null,
};

const github: PluginInfo = {
  id: "github",
  name: "GitHub",
  description: "Repos, issues, PRs",
  icon: "github",
  categories: ["vcs"],
  verified: true,
  experimental: false,
  enabled: true,
  source: "catalog",
  capabilities: ["connector"],
  configured: false,
  kind: "integration",
  installed: true,
  family: null,
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
  await usePlugins.getState().load();
  expect(spy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins.map((p) => p.id)).toEqual(["native", "github"]);
  expect(usePlugins.getState().loaded).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
});

test("load leaves plugins untouched and surfaces no crash on error", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const restartSpy = stubRestartRequired();
  await usePlugins.getState().load();
  expect(usePlugins.getState().plugins).toEqual([]);
  expect(usePlugins.getState().loaded).toBe(false);
  spy.mockRestore();
  restartSpy.mockRestore();
});

test("load populates restartRequired from pluginsRestartRequired", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [] });
  const restartSpy = stubRestartRequired(true);
  await usePlugins.getState().load();
  expect(restartSpy).toHaveBeenCalled();
  expect(usePlugins.getState().restartRequired).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
});

test("load leaves restartRequired untouched when pluginsRestartRequired errors", async () => {
  reset();
  usePlugins.setState({ restartRequired: true });
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [] });
  const restartSpy = spyOn(commands, "pluginsRestartRequired").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await usePlugins.getState().load();
  expect(usePlugins.getState().restartRequired).toBe(true);
  spy.mockRestore();
  restartSpy.mockRestore();
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

  const p = usePlugins.getState().setEnabled("github", false);
  // Optimistic update lands synchronously before the awaited command resolves.
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  await p;

  expect(setSpy).toHaveBeenCalledWith("github", false);
  expect(listSpy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  setSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
});

test("setEnabled reloads (not crashes) when the command errors, so state reconciles with the server", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const setSpy = spyOn(commands, "setPluginEnabled").mockResolvedValue({ status: "error", error: { message: "denied" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [github] });
  const restartSpy = stubRestartRequired();

  await usePlugins.getState().setEnabled("github", false);

  expect(setSpy).toHaveBeenCalledWith("github", false);
  expect(listSpy).toHaveBeenCalled();
  // Reload brought back the server truth (still enabled), undoing the optimistic flip.
  expect(usePlugins.getState().plugins[0].enabled).toBe(true);
  setSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
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

  await usePlugins.getState().update("acme", true);

  expect(updateSpy).toHaveBeenCalledWith("acme", true);
  expect(listSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
});

test("update refreshes cached doctor findings when they were already loaded", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true, doctorFindings: [], doctorLoaded: true });
  const updateSpy = spyOn(commands, "updatePlugin").mockResolvedValue({ status: "ok", data: { kind: "alreadyCurrent" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();
  const doctorSpy = spyOn(commands, "pluginDoctor").mockResolvedValue({ status: "ok", data: [] });

  await usePlugins.getState().update("acme", false);

  expect(doctorSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
  doctorSpy.mockRestore();
});

test("update toasts the error and still reloads when updatePlugin itself errors", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const updateSpy = spyOn(commands, "updatePlugin").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();

  await usePlugins.getState().update("acme", false);

  expect(listSpy).toHaveBeenCalled();
  updateSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
});

test("pin calls setPluginPin, tracks the id optimistically, and reloads", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const pinSpy = spyOn(commands, "setPluginPin").mockResolvedValue({ status: "ok", data: null });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [skillPack] });
  const restartSpy = stubRestartRequired();

  await usePlugins.getState().pin("acme", true, "vendored fork");

  expect(pinSpy).toHaveBeenCalledWith("acme", true, "vendored fork");
  expect(usePlugins.getState().pinnedIds.has("acme")).toBe(true);

  await usePlugins.getState().pin("acme", false);
  expect(pinSpy).toHaveBeenCalledWith("acme", false, null);
  expect(usePlugins.getState().pinnedIds.has("acme")).toBe(false);

  pinSpy.mockRestore();
  listSpy.mockRestore();
  restartSpy.mockRestore();
});

test("pin failure toasts and leaves pinnedIds untouched", async () => {
  reset();
  usePlugins.setState({ plugins: [skillPack], loaded: true });
  const pinSpy = spyOn(commands, "setPluginPin").mockResolvedValue({ status: "error", error: { message: "denied" } });

  await usePlugins.getState().pin("acme", true);

  expect(usePlugins.getState().pinnedIds.has("acme")).toBe(false);
  pinSpy.mockRestore();
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
