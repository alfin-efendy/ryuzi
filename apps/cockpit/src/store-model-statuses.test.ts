import { expect, spyOn, test } from "bun:test";
import { commands } from "./bindings";
import { statusKey, useModelStatuses } from "./store-model-statuses";

function reset() {
  useModelStatuses.setState({ byKey: {} });
}

test("statusKey joins family and model with a NUL separator", () => {
  expect(statusKey("openai", "gpt-5.5")).toBe("openai\u0000gpt-5.5");
});

test("hydrate loads every persisted verdict keyed by family+model", async () => {
  reset();
  const spy = spyOn(commands, "listAllModelStatuses").mockResolvedValue({
    status: "ok",
    data: [
      { family: "anthropic", model: "claude-x", status: "valid", message: "Model claude-x OK", testedAt: 100 },
      { family: "openai", model: "gpt-x", status: "invalid", message: "Model gpt-x returned HTTP 404", testedAt: 101 },
    ],
  });
  await useModelStatuses.getState().hydrate();
  expect(useModelStatuses.getState().byKey).toEqual({
    [statusKey("anthropic", "claude-x")]: "valid",
    [statusKey("openai", "gpt-x")]: "invalid",
  });
  spy.mockRestore();
});

test("hydrate leaves state untouched on error", async () => {
  reset();
  useModelStatuses.setState({ byKey: { [statusKey("openai", "gpt-x")]: "invalid" } });
  const spy = spyOn(commands, "listAllModelStatuses").mockResolvedValue({
    status: "error",
    error: { message: "boom" },
  });
  await useModelStatuses.getState().hydrate();
  expect(useModelStatuses.getState().byKey).toEqual({ [statusKey("openai", "gpt-x")]: "invalid" });
  spy.mockRestore();
});

test("upsert records definitive verdicts and ignores transient unknown", () => {
  reset();
  useModelStatuses.getState().upsert("openai", "gpt-x", "invalid");
  expect(useModelStatuses.getState().byKey[statusKey("openai", "gpt-x")]).toBe("invalid");
  // Mirrors Store::upsert_model_status: "unknown" never clobbers a verdict.
  useModelStatuses.getState().upsert("openai", "gpt-x", "unknown");
  expect(useModelStatuses.getState().byKey[statusKey("openai", "gpt-x")]).toBe("invalid");
  useModelStatuses.getState().upsert("openai", "gpt-x", "valid");
  expect(useModelStatuses.getState().byKey[statusKey("openai", "gpt-x")]).toBe("valid");
});
