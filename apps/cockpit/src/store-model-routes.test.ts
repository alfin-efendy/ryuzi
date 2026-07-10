import { test, expect, spyOn } from "bun:test";
import { useModelRoutes } from "./store-model-routes";
import { useStore } from "./store";
import { useConnections } from "./store-connections";
import { commands, type ConnectionInfo, type ModelRouteInfo } from "./bindings";

const route: ModelRouteInfo = {
  id: "r1",
  name: "smart",
  enabled: true,
  strategy: "fallback",
  targets: [{ provider: "anthropic", model: "claude-opus-4", effort: null }],
  createdAt: 100,
  updatedAt: 100,
};
const connection: ConnectionInfo = {
  id: "c1",
  provider: "openai",
  providerName: "OpenAI",
  color: "#888",
  initial: "O",
  authType: "apiKey",
  label: "OpenAI",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["gpt-5"],
  keyMasked: "sk-…",
  needsRelogin: false,
  claudeCloaking: false,
};

function reset() {
  useModelRoutes.setState({ routes: [], loaded: false });
}

test("successful_connection_and_route_mutations_reload_structured_models_once", async () => {
  reset();
  const saveSpy = spyOn(commands, "saveModelRoute").mockResolvedValue({ status: "ok", data: [route] });
  const reloadSpy = spyOn(useStore.getState(), "refreshModelConfiguration").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().save(route);
  expect(ok).toBe(true);
  expect(useModelRoutes.getState().routes).toEqual([route]);
  expect(reloadSpy).toHaveBeenCalledTimes(1);
  reloadSpy.mockClear();
  const addSpy = spyOn(commands, "addConnection").mockResolvedValue({ status: "ok", data: [connection] });
  expect(await useConnections.getState().add("openai", "OpenAI", "sk-test", null)).toBe(true);
  expect(reloadSpy).toHaveBeenCalledTimes(1);
  addSpy.mockRestore();
  saveSpy.mockRestore();
  reloadSpy.mockRestore();
});

test("failed_connection_and_route_mutations_do_not_reload_structured_models", async () => {
  reset();
  const saveSpy = spyOn(commands, "saveModelRoute").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const reloadSpy = spyOn(useStore.getState(), "refreshModelConfiguration").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().save(route);
  expect(ok).toBe(false);
  expect(reloadSpy).not.toHaveBeenCalled();
  const addSpy = spyOn(commands, "addConnection").mockResolvedValue({ status: "error", error: { message: "boom" } });
  expect(await useConnections.getState().add("openai", "OpenAI", "sk-test", null)).toBe(false);
  expect(reloadSpy).not.toHaveBeenCalled();
  addSpy.mockRestore();
  saveSpy.mockRestore();
  reloadSpy.mockRestore();
});

test("successful delete reloads structured models once", async () => {
  reset();
  useModelRoutes.setState({ routes: [route], loaded: true });
  const delSpy = spyOn(commands, "deleteModelRoute").mockResolvedValue({ status: "ok", data: [] });
  const reloadSpy = spyOn(useStore.getState(), "refreshModelConfiguration").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().remove("r1");
  expect(ok).toBe(true);
  expect(useModelRoutes.getState().routes).toEqual([]);
  expect(reloadSpy).toHaveBeenCalledTimes(1);
  delSpy.mockRestore();
  reloadSpy.mockRestore();
});
