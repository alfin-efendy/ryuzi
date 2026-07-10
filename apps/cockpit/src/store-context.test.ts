import { beforeEach, expect, test } from "bun:test";
import { useStore } from "./store";

beforeEach(() => {
  useStore.setState({ contextUsage: {} });
});

test("contextUsage events update per-session usage state", () => {
  useStore.getState().applyCoreEvent({
    kind: "contextUsage",
    session_pk: "s1",
    active_tokens: 120000,
    context_window: 200000,
    usable_window: 190000,
    percent_left: 37,
    cache_read_tokens: 0,
    output_tokens: 512,
  } as never);
  expect(useStore.getState().contextUsage["s1"]).toEqual({
    activeTokens: 120000,
    usableWindow: 190000,
    percentLeft: 37,
  });
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
