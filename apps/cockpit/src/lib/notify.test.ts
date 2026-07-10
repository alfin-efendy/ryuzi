import { expect, test } from "bun:test";
import { attentionCount, notifyIntentForEvent } from "./notify";
import type { Session, CoreEvent } from "../bindings";

function sess(pk: string, lastActive: number | null): Session {
  return {
    sessionPk: pk, projectId: "p", agentSessionId: null, worktreePath: null,
    branch: null, title: pk, status: "idle", startedBy: null, createdAt: 0,
    lastActive, resumeAttempts: 0, branchOwned: false,
  };
}

test("attentionCount = unread sessions + pending approvals; focused excluded", () => {
  const sessions = [sess("a", 500), sess("b", 50), sess("c", 900)];
  const readAt = { a: 100, b: 100, c: 100 }; // a,c unread; b read
  // focused = c → c not counted; +2 pending
  expect(attentionCount(sessions, readAt, "c", 2)).toBe(1 + 2); // only a unread + 2
  // no focus → a,c unread + 0 pending
  expect(attentionCount(sessions, readAt, null, 0)).toBe(2);
});

test("attentionCount zero when all read and none pending", () => {
  expect(attentionCount([sess("a", 100)], { a: 100 }, null, 0)).toBe(0);
});

const ev = (o: Record<string, unknown>) => o as unknown as CoreEvent;

test("windowFocused suppresses every intent", () => {
  expect(notifyIntentForEvent(ev({ kind: "result", session_pk: "a" }), null, true)).toBeNull();
  expect(notifyIntentForEvent(ev({ kind: "approvalRequested", session_pk: "a", tool: "bash" }), null, true)).toBeNull();
});

test("result → finished + settle", () => {
  expect(notifyIntentForEvent(ev({ kind: "result", session_pk: "a" }), null, false)).toEqual({
    sessionPk: "a", kind: "finished", settle: true,
  });
});

test("approvalRequested → approval immediate with tool detail", () => {
  expect(notifyIntentForEvent(ev({ kind: "approvalRequested", session_pk: "a", tool: "bash" }), null, false)).toEqual({
    sessionPk: "a", kind: "approval", settle: false, detail: "bash",
  });
});

test("error → error immediate", () => {
  expect(notifyIntentForEvent(ev({ kind: "error", session_pk: "a", message: "boom" }), null, false)).toEqual({
    sessionPk: "a", kind: "error", settle: false,
  });
});

test("unrelated events → null", () => {
  expect(notifyIntentForEvent(ev({ kind: "message", session_pk: "a" }), null, false)).toBeNull();
  expect(notifyIntentForEvent(ev({ kind: "sessionCreated", session_pk: "a" }), null, false)).toBeNull();
});
