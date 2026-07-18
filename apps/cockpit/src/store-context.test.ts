import { beforeEach, expect, test } from "bun:test";
import { useStore } from "./store";
import type { CoreEvent } from "./bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";
import { delegationRunKey } from "@/store-delegation";

const key = sessKey(LOCAL_RUNNER, "s1");

beforeEach(() => {
  useStore.setState({ contextUsage: {}, sessionCost: {}, runContextUsage: {} });
});

test("contextUsage keeps window + cache + output fields", () => {
  useStore.getState().applyCoreEvent(
    {
      kind: "contextUsage",
      session_pk: "s1",
      active_tokens: 1000,
      context_window: 200000,
      usable_window: 190000,
      percent_left: 95,
      cache_read_tokens: 300,
      cache_creation_tokens: 4000,
      output_tokens: 512,
    } as CoreEvent,
    LOCAL_RUNNER,
  );
  expect(useStore.getState().contextUsage[key]).toEqual({
    activeTokens: 1000,
    usableWindow: 190000,
    percentLeft: 95,
    contextWindow: 200000,
    cacheReadTokens: 300,
    cacheCreationTokens: 4000,
    outputTokens: 512,
  });
});

test("sessionCost stores the total and per-model breakdown", () => {
  useStore.getState().applyCoreEvent(
    {
      kind: "sessionCost",
      session_pk: "s1",
      total_usd: 0.1234,
      models: [{ model: "claude-sonnet-4", input: 100, output: 40, cacheRead: 20, cacheCreation: 5, usd: 0.1234 }],
    } as CoreEvent,
    LOCAL_RUNNER,
  );
  const c = useStore.getState().sessionCost[key];
  expect(c.totalUsd).toBe(0.1234);
  expect(c.models[0].model).toBe("claude-sonnet-4");
});

test("agentRunContextUsage stores per-run usage without touching session usage", () => {
  useStore.setState({ contextUsage: {}, runContextUsage: {} });
  useStore.getState().applyCoreEvent(
    {
      kind: "agentRunContextUsage",
      session_pk: "s1",
      run_id: "run-1",
      active_tokens: 4_000,
      context_window: 200_000,
      usable_window: 120_000,
      percent_left: 60,
    } as CoreEvent,
    LOCAL_RUNNER,
  );
  expect(useStore.getState().runContextUsage[delegationRunKey(LOCAL_RUNNER, "s1", "run-1")]).toEqual({
    activeTokens: 4_000,
    usableWindow: 120_000,
    percentLeft: 60,
  });
  // The session-level ring must NOT be affected by a run-scoped event.
  expect(useStore.getState().contextUsage[key]).toBeUndefined();
});

test("contextCompacted is a safe no-op for store state", () => {
  useStore.getState().applyCoreEvent(
    {
      kind: "contextCompacted",
      session_pk: "s1",
      trigger: "pre_turn",
      before_tokens: 100000,
      after_tokens: 20000,
      window_number: 1,
    } as never,
    LOCAL_RUNNER,
  );
  expect(useStore.getState().contextUsage[key]).toBeUndefined();
});
