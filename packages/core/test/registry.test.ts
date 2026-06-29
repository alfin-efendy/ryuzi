import { test, expect } from "bun:test";
import { Registry } from "../src/core/registry";

test("Registry register/create/has/ids", () => {
  const r = new Registry<{ id: string }>();
  expect(r.has("a")).toBe(false);
  r.register("a", () => ({ id: "a" }));
  expect(r.has("a")).toBe(true);
  expect(r.create("a").id).toBe("a");
  expect(r.ids()).toEqual(["a"]);
});

test("Registry create throws on unknown id", () => {
  const r = new Registry<number>();
  expect(() => r.create("x")).toThrow(/unknown/i);
});
