import { expect, test } from "bun:test";
import { attentionCount, notifyIntentForEvent } from "./notify";
import type { Session, CoreEvent } from "../bindings";

function sess(pk: string, lastActive: number | null): Session {
  return {
    sessionPk: pk,
    projectId: "p",
    agentSessionId: null,
    worktreePath: null,
    branch: null,
    title: pk,
    status: "idle",
    startedBy: null,
    createdAt: 0,
    lastActive,
    resumeAttempts: 0,
    branchOwned: false,
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
    sessionPk: "a",
    kind: "finished",
    settle: true,
  });
});

test("approvalRequested → approval immediate with tool detail", () => {
  expect(notifyIntentForEvent(ev({ kind: "approvalRequested", session_pk: "a", tool: "bash" }), null, false)).toEqual({
    sessionPk: "a",
    kind: "approval",
    settle: false,
    detail: "bash",
  });
});

test("error → error immediate", () => {
  expect(notifyIntentForEvent(ev({ kind: "error", session_pk: "a", message: "boom" }), null, false)).toEqual({
    sessionPk: "a",
    kind: "error",
    settle: false,
  });
});

test("unrelated events → null", () => {
  expect(notifyIntentForEvent(ev({ kind: "message", session_pk: "a" }), null, false)).toBeNull();
  expect(notifyIntentForEvent(ev({ kind: "sessionCreated", session_pk: "a" }), null, false)).toBeNull();
});

import { createNotifier, notificationText, SETTLE_MS, type NotifierDeps } from "./notify";

function fakeDeps(over: Partial<NotifierDeps> = {}) {
  const sent: Array<{ title: string; body: string }> = [];
  const badges: Array<number | undefined> = [];
  const timers: Array<{ fn: () => void; ms: number; cancelled: boolean }> = [];
  const deps: NotifierDeps = {
    sendNotification: (o) => sent.push(o),
    setBadgeCount: (n) => badges.push(n),
    ensurePermission: async () => true,
    isEnabled: () => true,
    schedule: (fn, ms) => {
      const t = { fn, ms, cancelled: false };
      timers.push(t);
      return () => {
        t.cancelled = true;
      };
    },
    ...over,
  };
  const runTimers = () => {
    for (const t of timers) {
      if (!t.cancelled) t.fn();
    }
  };
  return { deps, sent, badges, timers, runTimers };
}

test("settle intent schedules, fires after the settle window", async () => {
  const f = fakeDeps();
  const n = createNotifier(f.deps);
  n.handle({ sessionPk: "a", kind: "finished", settle: true }, undefined);
  expect(f.timers[0].ms).toBe(SETTLE_MS);
  expect(f.sent.length).toBe(0);
  f.runTimers();
  await Promise.resolve();
  expect(f.sent.length).toBe(1);
});

test("a second event for the same session cancels the pending settle", () => {
  const f = fakeDeps();
  const n = createNotifier(f.deps);
  n.handle({ sessionPk: "a", kind: "finished", settle: true }, undefined);
  n.cancelSettle("a"); // new activity arrives
  f.runTimers();
  expect(f.sent.length).toBe(0);
});

test("immediate intent sends now and cancels any pending settle", async () => {
  const f = fakeDeps();
  const n = createNotifier(f.deps);
  n.handle({ sessionPk: "a", kind: "finished", settle: true }, undefined);
  n.handle({ sessionPk: "a", kind: "approval", settle: false, detail: "bash" }, undefined);
  await Promise.resolve();
  expect(f.sent.length).toBe(1);
  expect(f.sent[0].body).toContain("bash");
  f.runTimers(); // the settle timer was cancelled
  await Promise.resolve();
  expect(f.sent.length).toBe(1);
});

test("disabled → no send, but updateBadge still works", async () => {
  const f = fakeDeps({ isEnabled: () => false });
  const n = createNotifier(f.deps);
  n.handle({ sessionPk: "a", kind: "approval", settle: false, detail: "x" }, undefined);
  await Promise.resolve();
  expect(f.sent.length).toBe(0);
  n.updateBadge(3);
  expect(f.badges).toEqual([3]);
});

test("updateBadge maps 0 to undefined (clears)", () => {
  const f = fakeDeps();
  const n = createNotifier(f.deps);
  n.updateBadge(0);
  n.updateBadge(5);
  expect(f.badges).toEqual([undefined, 5]);
});

test("permission denied → no send", async () => {
  const f = fakeDeps({ ensurePermission: async () => false });
  const n = createNotifier(f.deps);
  n.handle({ sessionPk: "a", kind: "error", settle: false }, undefined);
  await Promise.resolve();
  await Promise.resolve();
  expect(f.sent.length).toBe(0);
});

test("notificationText formats per kind", () => {
  const s = { title: "My session" } as never;
  expect(notificationText({ sessionPk: "a", kind: "finished", settle: true }, s)).toEqual({ title: "My session", body: "Turn finished" });
  expect(notificationText({ sessionPk: "a", kind: "approval", settle: false, detail: "bash" }, s)).toEqual({
    title: "My session",
    body: "Needs approval: bash",
  });
  expect(notificationText({ sessionPk: "a", kind: "error", settle: false }, s)).toEqual({ title: "My session", body: "Turn errored" });
});

import { badgeCountFor } from "./notify";

test("badgeCountFor composes attentionCount from store slices", () => {
  const sessions = [sess("a", 500), sess("b", 50)];
  // a unread (500>100), b read; +1 pending → 2
  expect(badgeCountFor(sessions, { a: 100, b: 100 }, null, 1)).toBe(2);
});
