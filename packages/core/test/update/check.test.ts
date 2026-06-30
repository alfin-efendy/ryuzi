import { test, expect } from "bun:test";
import { checkForUpdate } from "../../src/update/check";

function fakeFetch(status: number, body: unknown): typeof fetch {
  return (async () =>
    new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })) as unknown as typeof fetch;
}

test("reports an available update when the latest tag exceeds current", async () => {
  const res = await checkForUpdate({ currentVersion: "0.2.0", repo: "o/r", fetchImpl: fakeFetch(200, { tag_name: "v0.3.0" }) });
  expect(res).toEqual({ currentVersion: "0.2.0", latestVersion: "0.3.0", updateAvailable: true, tag: "v0.3.0" });
});

test("no update when latest equals current", async () => {
  const res = await checkForUpdate({ currentVersion: "0.3.0", repo: "o/r", fetchImpl: fakeFetch(200, { tag_name: "v0.3.0" }) });
  expect(res.updateAvailable).toBe(false);
});

test("non-OK response yields no update, no throw", async () => {
  const res = await checkForUpdate({ currentVersion: "0.2.0", repo: "o/r", fetchImpl: fakeFetch(403, {}) });
  expect(res).toEqual({ currentVersion: "0.2.0", latestVersion: null, updateAvailable: false, tag: null });
});

test("missing tag_name yields no update", async () => {
  const res = await checkForUpdate({ currentVersion: "0.2.0", repo: "o/r", fetchImpl: fakeFetch(200, {}) });
  expect(res.updateAvailable).toBe(false);
});

test("a thrown fetch (network error) yields no update, no throw", async () => {
  const throwing = (async () => {
    throw new Error("network down");
  }) as unknown as typeof fetch;
  const res = await checkForUpdate({ currentVersion: "0.2.0", repo: "o/r", fetchImpl: throwing });
  expect(res.updateAvailable).toBe(false);
  expect(res.latestVersion).toBeNull();
});
