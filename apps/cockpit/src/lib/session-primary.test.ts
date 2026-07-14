import { expect, test } from "bun:test";
import { sessionIsReadOnly, sessionPrimaryLabel } from "./session-primary";

const snapshot = { id: "reviewer", name: "Reviewer", avatarColor: "violet" };

test("uses the immutable primary snapshot rather than a mutable agent profile", () => {
  expect(sessionPrimaryLabel(snapshot)).toBe("Reviewer");
  expect(sessionIsReadOnly(snapshot)).toBe(false);
});

test("legacy sessions without a snapshot are explicitly read-only", () => {
  expect(sessionPrimaryLabel(null)).toBe("Legacy session");
  expect(sessionIsReadOnly(null)).toBe(true);
});
