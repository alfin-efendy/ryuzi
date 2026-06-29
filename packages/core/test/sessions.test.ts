import { test, expect } from "bun:test";
import type { Session } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { SessionsStore } from "../src/store/sessions";

function sample(): Session {
  return { sessionPk: "s1", projectId: "p1", status: "running", title: "t", createdAt: 1, lastActive: 1 };
}

test("insert + get + update", () => {
  const s = new SessionsStore(openDb(":memory:"));
  s.insert(sample());
  expect(s.get("s1")?.status).toBe("running");
  s.update("s1", { status: "ended", agentSessionId: "agent-xyz" });
  expect(s.get("s1")?.status).toBe("ended");
  expect(s.get("s1")?.agentSessionId).toBe("agent-xyz");
});

test("list by project", () => {
  const s = new SessionsStore(openDb(":memory:"));
  s.insert(sample());
  s.insert({ ...sample(), sessionPk: "s2", projectId: "p2" });
  expect(s.list("p1").map((x) => x.sessionPk)).toEqual(["s1"]);
  expect(s.list().length).toBe(2);
});

test("surfaces: one session, multiple conversations", () => {
  const s = new SessionsStore(openDb(":memory:"));
  s.insert(sample());
  s.addSurface("discord", "thread-1", "s1");
  s.addSurface("slack", "ts-2", "s1");
  expect(s.resolveByConversation("discord", "thread-1")?.sessionPk).toBe("s1");
  expect(
    s
      .surfaces("s1")
      .map((x) => x.conversationId)
      .sort(),
  ).toEqual(["thread-1", "ts-2"]);
});
