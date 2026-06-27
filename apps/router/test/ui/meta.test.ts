import { test, expect } from "bun:test";
import { helpText, version } from "../../src/cli/meta";

test("help leads with OPTIONS and lists only doctor + run", () => {
  const h = helpText();
  expect(h).toContain("OPTIONS");
  expect(h).toContain("doctor");
  expect(h).toContain("run");
  expect(h).not.toContain("config");   // hidden
  expect(h).not.toContain("init");
  expect(h).not.toContain("start");
});

test("version is a semver-ish string", () => {
  expect(version()).toMatch(/\d+\.\d+\.\d+/);
});
