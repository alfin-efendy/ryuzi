import { expect, test } from "bun:test";
import { enqueue, dequeue, removeById, type QueuedMessage } from "./queue";

const msg = (id: string, text = id): QueuedMessage => ({ id, text, options: null });

test("enqueue appends immutably", () => {
  const a = [msg("1")];
  const b = enqueue(a, msg("2"));
  expect(b.map((m) => m.id)).toEqual(["1", "2"]);
  expect(a.map((m) => m.id)).toEqual(["1"]); // original untouched
});

test("enqueue treats undefined as empty", () => {
  expect(enqueue(undefined, msg("1")).map((m) => m.id)).toEqual(["1"]);
});

test("dequeue splits head and rest", () => {
  const { head, rest } = dequeue([msg("1"), msg("2"), msg("3")]);
  expect(head?.id).toBe("1");
  expect(rest.map((m) => m.id)).toEqual(["2", "3"]);
});

test("dequeue on empty/undefined returns null head", () => {
  expect(dequeue([])).toEqual({ head: null, rest: [] });
  expect(dequeue(undefined)).toEqual({ head: null, rest: [] });
});

test("removeById removes the match and preserves order", () => {
  const out = removeById([msg("1"), msg("2"), msg("3")], "2");
  expect(out.map((m) => m.id)).toEqual(["1", "3"]);
});

test("removeById no-ops on a missing id and on undefined", () => {
  expect(removeById([msg("1")], "x").map((m) => m.id)).toEqual(["1"]);
  expect(removeById(undefined, "x")).toEqual([]);
});
