import { test, expect, spyOn } from "bun:test";
import { useRuntimes } from "./store-runtimes";
import { useStore } from "./store";
import { commands, type RuntimeInfo } from "./bindings";

function makeRuntime(overrides: Partial<RuntimeInfo> = {}): RuntimeInfo {
  return {
    id: "native",
    name: "Native",
    color: "#888",
    initial: "N",
    connection: "in-process",
    binaryPath: "in-process",
    installedVersion: "0.0.0",
    latestVersion: null,
    npmPackage: null,
    models: ["anthropic/claude-opus-4"],
    selectableModels: [],
    enabled: true,
    model: "anthropic/claude-opus-4",
    permMode: "ask",
    flags: "",
    tiers: [],
    isDefault: true,
    runnable: true,
    ...overrides,
  };
}

function reset() {
  useRuntimes.setState({ runtimes: [], loaded: false, refreshing: false, updating: {}, updateLog: {} });
}

test("reloadList replaces runtimes on ok without touching loaded/refreshing", async () => {
  reset();
  const fresh = makeRuntime({ models: ["smart", "anthropic/claude-opus-4"] });
  const spy = spyOn(commands, "listRuntimes").mockResolvedValue({ status: "ok", data: [fresh] });
  await useRuntimes.getState().reloadList();
  expect(useRuntimes.getState().runtimes).toEqual([fresh]);
  // Deliberately untouched: reloadList is a silent list refresh, not a hydrate.
  expect(useRuntimes.getState().loaded).toBe(false);
  expect(useRuntimes.getState().refreshing).toBe(false);
  spy.mockRestore();
});

test("reloadList leaves the previous list on error", async () => {
  reset();
  const stale = makeRuntime();
  useRuntimes.setState({ runtimes: [stale] });
  const spy = spyOn(commands, "listRuntimes").mockResolvedValue({ status: "error", error: { message: "boom" } });
  await useRuntimes.getState().reloadList();
  expect(useRuntimes.getState().runtimes).toEqual([stale]);
  spy.mockRestore();
});

test("newer_model_configuration_refresh_owns_the_runtime_list_commit", async () => {
  reset();
  const old = makeRuntime({ selectableModels: [], models: ["old"] });
  const fresh = makeRuntime({ selectableModels: [], models: ["new"] });
  let resolveOld!: (value: unknown) => void;
  const oldPromise = new Promise((resolve) => {
    resolveOld = resolve;
  });
  const list = spyOn(commands, "listRuntimes")
    .mockImplementationOnce(() => oldPromise as never)
    .mockResolvedValueOnce({ status: "ok", data: [fresh] });
  const first = useStore.getState().refreshModelConfiguration();
  const second = useStore.getState().refreshModelConfiguration();
  await second;
  resolveOld({ status: "ok", data: [old] });
  await first;
  expect(useRuntimes.getState().runtimes[0].models).toEqual(["new"]);
  list.mockRestore();
});
