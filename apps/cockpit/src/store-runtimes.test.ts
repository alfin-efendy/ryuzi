import { test, expect, spyOn } from "bun:test";
import { useRuntimes } from "./store-runtimes";
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
