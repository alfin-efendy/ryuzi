import { test, expect } from "bun:test";
import { claudeCodeRuntime } from "../../src/providers/runtimes/claude-code";

test("claude-code descriptor: no fields, builds a harness, detect resolves a shape", async () => {
  expect(claudeCodeRuntime.id).toBe("claude-code");
  expect(claudeCodeRuntime.kind).toBe("runtime");
  expect(claudeCodeRuntime.fields).toEqual([]);
  expect(claudeCodeRuntime.build().id).toBe("claude-code");
  const info = await claudeCodeRuntime.detect();
  expect(typeof info.found).toBe("boolean"); // claude may or may not be installed in CI
});
