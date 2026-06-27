import { test, expect } from "bun:test";
import { reduceSessions, type LiveSession } from "../../src/cli/ui/sessions-reducer";
import type { CoreEvent } from "@harness/protocol";

test("reducer tracks status + last text per session", () => {
  const m = new Map<string, LiveSession>();
  const events: CoreEvent[] = [
    { kind: "session.created", sessionPk: "s1", projectId: "p1" },
    { kind: "status", sessionPk: "s1", text: "Bash: ls" },
    { kind: "text", sessionPk: "s1", text: "all done" },
    { kind: "result", sessionPk: "s1" },
  ];
  for (const e of events) reduceSessions(m, e);
  expect(m.get("s1")).toEqual({ status: "idle", lastText: "all done" });
});

test("reducer marks error and ended", () => {
  const m = new Map<string, LiveSession>();
  reduceSessions(m, { kind: "session.created", sessionPk: "s2", projectId: "p1" });
  reduceSessions(m, { kind: "error", sessionPk: "s2", message: "boom" });
  expect(m.get("s2")).toEqual({ status: "error", lastText: "boom" });
  reduceSessions(m, { kind: "session.ended", sessionPk: "s2" });
  expect(m.get("s2")!.status).toBe("ended");
});
