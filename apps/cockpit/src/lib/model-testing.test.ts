import { expect, test } from "bun:test";
import { runPool, visibleModels, type ModelTestEntry } from "./model-testing";

test("runPool caps concurrency and preserves item order", async () => {
  let active = 0;
  let peak = 0;
  const results = await runPool([1, 2, 3, 4, 5, 6, 7], 3, async (n) => {
    active += 1;
    peak = Math.max(peak, active);
    await new Promise((r) => setTimeout(r, 5));
    active -= 1;
    return n * 10;
  });
  expect(results).toEqual([10, 20, 30, 40, 50, 60, 70]);
  expect(peak).toBeLessThanOrEqual(3);
  expect(peak).toBeGreaterThan(1);
});

test("runPool handles limits larger than the item count and empty input", async () => {
  expect(await runPool([1], 3, async (n) => n)).toEqual([1]);
  expect(await runPool([], 3, async (n) => n)).toEqual([]);
});

test("visibleModels hides only persisted-invalid rows when the toggle is on", () => {
  const statuses = new Map<string, ModelTestEntry>([
    ["a", { status: "valid", message: "" }],
    ["b", { status: "invalid", message: "Model b returned HTTP 404" }],
    ["c", { status: "unknown", message: "Model c returned HTTP 429" }],
  ]);
  const models = ["a", "b", "c", "d"];
  expect(visibleModels(models, statuses, false)).toEqual(models);
  expect(visibleModels(models, statuses, true)).toEqual(["a", "c", "d"]);
});
