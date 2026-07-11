import { beforeEach, expect, test } from "bun:test";
import { useStore } from "./store";
import type { CoreEvent } from "./bindings";

beforeEach(() => {
  useStore.setState({ contextUsage: {}, sessionCost: {} });
});

test("contextUsage keeps window + cache + output fields", () => {
  useStore.getState().applyCoreEvent({
    kind: "contextUsage",
    session_pk: "s1",
    active_tokens: 1000,
    context_window: 200000,
    usable_window: 190000,
    percent_left: 95,
    cache_read_tokens: 300,
    output_tokens: 512,
  } as CoreEvent);
  expect(useStore.getState().contextUsage.s1).toEqual({
    activeTokens: 1000,
    usableWindow: 190000,
    percentLeft: 95,
    contextWindow: 200000,
    cacheReadTokens: 300,
    outputTokens: 512,
  });
});

test("sessionCost stores the total and per-model breakdown", () => {
  useStore.getState().applyCoreEvent({
    kind: "sessionCost",
    session_pk: "s1",
    total_usd: 0.1234,
    models: [{ model: "claude-sonnet-4", input: 100, output: 40, cacheRead: 20, cacheCreation: 5, usd: 0.1234 }],
  } as CoreEvent);
  const c = useStore.getState().sessionCost.s1;
  expect(c.totalUsd).toBe(0.1234);
  expect(c.models[0].model).toBe("claude-sonnet-4");
});

test("contextCompacted is a safe no-op for store state", () => {
  useStore.getState().applyCoreEvent({
    kind: "contextCompacted",
    session_pk: "s1",
    trigger: "pre_turn",
    before_tokens: 100000,
    after_tokens: 20000,
    window_number: 1,
  } as never);
  expect(useStore.getState().contextUsage["s1"]).toBeUndefined();
});
