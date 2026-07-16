import { expect, test } from "bun:test";
import type { AgentSummaryInfo } from "@/bindings";
import { sessionIsReadOnly, sessionPrimaryLabel } from "./session-primary";

const snapshot = { id: "reviewer", name: "Reviewer", avatarColor: "violet" };

test("uses the immutable primary snapshot rather than a mutable agent profile", () => {
  const agents: AgentSummaryInfo[] = [{ id: "reviewer" } as AgentSummaryInfo];
  expect(sessionPrimaryLabel(snapshot, agents)).toBe("Reviewer");
  expect(sessionIsReadOnly(snapshot)).toBe(false);
});

test("legacy sessions are explicitly marked without a registry lookup", () => {
  expect(sessionPrimaryLabel(null, [])).toBe("Legacy agent");
  expect(sessionIsReadOnly(null)).toBe(true);
});

test("a captured profile missing from an available registry is marked deleted", () => {
  expect(sessionPrimaryLabel(snapshot, [])).toBe("Reviewer (Deleted)");
});

test("an unavailable registry preserves the captured identity without a deletion claim", () => {
  expect(sessionPrimaryLabel(snapshot, undefined)).toBe("Reviewer");
});
