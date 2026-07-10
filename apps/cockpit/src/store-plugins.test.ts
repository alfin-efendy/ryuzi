import { test, expect, spyOn } from "bun:test";
import { usePlugins, sidebarPlugins, catalogPlugins } from "./store-plugins";
import { commands, type PluginInfo } from "./bindings";

function reset() {
  usePlugins.setState({ plugins: [], loaded: false });
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
};

const userPlugin: PluginInfo = {
  ...github,
  id: "acme",
  name: "Acme",
  source: "user",
};

const disabledCatalog: PluginInfo = { ...github, id: "linear", name: "Linear", enabled: false };

test("load populates plugins from listPlugins", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [builtin, github] });
  await usePlugins.getState().load();
  expect(spy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins.map((p) => p.id)).toEqual(["native", "github"]);
  expect(usePlugins.getState().loaded).toBe(true);
  spy.mockRestore();
});

test("load leaves plugins untouched and surfaces no crash on error", async () => {
  reset();
  const spy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "error", error: { message: "boom" } });
  await usePlugins.getState().load();
  expect(usePlugins.getState().plugins).toEqual([]);
  expect(usePlugins.getState().loaded).toBe(false);
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

  const p = usePlugins.getState().setEnabled("github", false);
  // Optimistic update lands synchronously before the awaited command resolves.
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  await p;

  expect(setSpy).toHaveBeenCalledWith("github", false);
  expect(listSpy).toHaveBeenCalled();
  expect(usePlugins.getState().plugins[0].enabled).toBe(false);
  setSpy.mockRestore();
  listSpy.mockRestore();
});

test("setEnabled reloads (not crashes) when the command errors, so state reconciles with the server", async () => {
  reset();
  usePlugins.setState({ plugins: [github], loaded: true });
  const setSpy = spyOn(commands, "setPluginEnabled").mockResolvedValue({ status: "error", error: { message: "denied" } });
  const listSpy = spyOn(commands, "listPlugins").mockResolvedValue({ status: "ok", data: [github] });

  await usePlugins.getState().setEnabled("github", false);

  expect(setSpy).toHaveBeenCalledWith("github", false);
  expect(listSpy).toHaveBeenCalled();
  // Reload brought back the server truth (still enabled), undoing the optimistic flip.
  expect(usePlugins.getState().plugins[0].enabled).toBe(true);
  setSpy.mockRestore();
  listSpy.mockRestore();
});

test("sidebarPlugins keeps only enabled catalog/user plugins, hiding builtins and disabled ones", () => {
  const result = sidebarPlugins([builtin, github, userPlugin, disabledCatalog]);
  expect(result.map((p) => p.id)).toEqual(["github", "acme"]);
});

test("catalogPlugins keeps every catalog/user plugin regardless of enabled state, hiding builtins", () => {
  const result = catalogPlugins([builtin, github, userPlugin, disabledCatalog]);
  expect(result.map((p) => p.id)).toEqual(["github", "acme", "linear"]);
});
