import { test, expect, spyOn } from "bun:test";
import { useModelRoutes } from "./store-model-routes";
import { useRuntimes } from "./store-runtimes";
import { commands, type ModelRouteInfo } from "./bindings";

const route: ModelRouteInfo = {
  id: "r1",
  name: "smart",
  enabled: true,
  strategy: "fallback",
  targets: [{ provider: "anthropic", model: "claude-opus-4" }],
  createdAt: 100,
  updatedAt: 100,
};

function reset() {
  useModelRoutes.setState({ routes: [], loaded: false });
}

test("successful save updates routes and reloads the runtime list", async () => {
  reset();
  const saveSpy = spyOn(commands, "saveModelRoute").mockResolvedValue({ status: "ok", data: [route] });
  const reloadSpy = spyOn(useRuntimes.getState(), "reloadList").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().save(route);
  expect(ok).toBe(true);
  expect(useModelRoutes.getState().routes).toEqual([route]);
  expect(reloadSpy).toHaveBeenCalledTimes(1);
  saveSpy.mockRestore();
  reloadSpy.mockRestore();
});

test("failed save does not reload the runtime list", async () => {
  reset();
  const saveSpy = spyOn(commands, "saveModelRoute").mockResolvedValue({ status: "error", error: { message: "boom" } });
  const reloadSpy = spyOn(useRuntimes.getState(), "reloadList").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().save(route);
  expect(ok).toBe(false);
  expect(reloadSpy).not.toHaveBeenCalled();
  saveSpy.mockRestore();
  reloadSpy.mockRestore();
});

test("successful delete reloads the runtime list", async () => {
  reset();
  useModelRoutes.setState({ routes: [route], loaded: true });
  const delSpy = spyOn(commands, "deleteModelRoute").mockResolvedValue({ status: "ok", data: [] });
  const reloadSpy = spyOn(useRuntimes.getState(), "reloadList").mockResolvedValue(undefined);
  const ok = await useModelRoutes.getState().remove("r1");
  expect(ok).toBe(true);
  expect(useModelRoutes.getState().routes).toEqual([]);
  expect(reloadSpy).toHaveBeenCalledTimes(1);
  delSpy.mockRestore();
  reloadSpy.mockRestore();
});
