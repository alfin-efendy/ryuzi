import { test, expect, spyOn } from "bun:test";
import { useAgent } from "./store-agent";
import { NATIVE_AGENT } from "./constants";
import { commands } from "./bindings";

function reset() {
  useAgent.setState({ models: [], model: null, permMode: null });
}

test("NATIVE_AGENT mirrors the engine's native descriptor", () => {
  expect(NATIVE_AGENT).toEqual({ id: "native", name: "Ryuzi", color: "#7C5CFF", initial: "R" });
});

test("load pulls settings and the selectable model list", async () => {
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
  sSpy.mockRestore();
  mSpy.mockRestore();
});

test("setModel writes optimistically and persists model + current permMode", async () => {
  reset();
  useAgent.setState({ permMode: "ask" });
  const spy = spyOn(commands, "setAgentSettings").mockResolvedValue({ status: "ok", data: null });
  await useAgent.getState().setModel("smart");
  expect(useAgent.getState().model).toBe("smart");
  expect(spy).toHaveBeenCalledWith("smart", "ask");
  spy.mockRestore();
});

test("setPermMode rolls back on a rejected write", async () => {
  reset();
  useAgent.setState({ permMode: "ask" });
  const spy = spyOn(commands, "setAgentSettings").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await useAgent.getState().setPermMode("full");
  expect(useAgent.getState().permMode).toBe("ask");
  spy.mockRestore();
});
