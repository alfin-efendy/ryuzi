import { expect, test } from "bun:test";
import { sessionRuntimeScope } from "./session-runtime";

test("projectless worker and review sessions never use the mutable chat runtime", () => {
  expect(sessionRuntimeScope("worker", null)).toBeNull();
  expect(sessionRuntimeScope("review", null)).toBeNull();
});

test("only chat sessions use session runtime while project sessions use project runtime", () => {
  expect(sessionRuntimeScope("chat", null)).toBe("session");
  expect(sessionRuntimeScope("project", "p1")).toBe("project");
});
