import { test, expect, spyOn } from "bun:test";
import { toast } from "sonner";
import { useAgent } from "./store-agent";
import { NATIVE_AGENT } from "./constants";
import { commands } from "./bindings";

function reset() {
  useAgent.setState({ models: [], model: null, permMode: null, loaded: false });
}

test("NATIVE_AGENT mirrors the engine's native descriptor", () => {
  expect(NATIVE_AGENT).toEqual({ id: "native", name: "Ryuzi", color: "#7C5CFF", initial: "R" });
});

test("load pulls settings and the selectable model list, and marks the store loaded", async () => {
  reset();
  const sSpy = spyOn(commands, "getAgentSettings").mockResolvedValue({
    status: "ok",
    data: { model: "anthropic/claude-opus-4", permMode: "edit" },
  });
  const mSpy = spyOn(commands, "listSelectableModels").mockResolvedValue({
    status: "ok",
    data: ["smart", "anthropic/claude-opus-4"],
  });
  await useAgent.getState().load();
  expect(useAgent.getState().model).toBe("anthropic/claude-opus-4");
  expect(useAgent.getState().permMode).toBe("edit");
  expect(useAgent.getState().models).toEqual(["smart", "anthropic/claude-opus-4"]);
  expect(useAgent.getState().loaded).toBe(true);
  sSpy.mockRestore();
  mSpy.mockRestore();
});

test("a failed load surfaces a toast and leaves the store not loaded", async () => {
  reset();
  const toastSpy = spyOn(toast, "error");
  const sSpy = spyOn(commands, "getAgentSettings").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  const mSpy = spyOn(commands, "listSelectableModels").mockResolvedValue({ status: "ok", data: [] });
  await useAgent.getState().load();
  expect(useAgent.getState().loaded).toBe(false);
  expect(useAgent.getState().model).toBeNull();
  expect(toastSpy).toHaveBeenCalledTimes(1);
  expect(toastSpy.mock.calls[0]?.[0]).toContain("boom");
  toastSpy.mockRestore();
  sSpy.mockRestore();
  mSpy.mockRestore();
});

test("setModel no-ops before a successful load has hydrated the store", async () => {
  reset();
  const spy = spyOn(commands, "setAgentSettings").mockResolvedValue({ status: "ok", data: null });
  await useAgent.getState().setModel("smart");
  expect(useAgent.getState().model).toBeNull();
  expect(spy).not.toHaveBeenCalled();
  spy.mockRestore();
});

test("setModel writes optimistically and passes through the hydrated permMode unchanged", async () => {
  reset();
  useAgent.setState({ loaded: true, permMode: "ask" });
  const spy = spyOn(commands, "setAgentSettings").mockResolvedValue({ status: "ok", data: null });
  await useAgent.getState().setModel("smart");
  expect(useAgent.getState().model).toBe("smart");
  expect(spy).toHaveBeenCalledWith("smart", "ask");
  spy.mockRestore();
});

test("setModel rolls back on a rejected write", async () => {
  reset();
  useAgent.setState({ loaded: true, model: "old-model", permMode: "ask" });
  const spy = spyOn(commands, "setAgentSettings").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await useAgent.getState().setModel("new-model");
  expect(useAgent.getState().model).toBe("old-model");
  spy.mockRestore();
});
