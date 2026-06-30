import { test, expect } from "bun:test";
import { useStore } from "./store";

function reset() {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSessionPk: null, selectedProjectId: null });
}

test("selectProject sets the selected project and clears the focused session", () => {
  reset();
  useStore.setState({ focusedSessionPk: "s1" });
  useStore.getState().selectProject("p1");
  expect(useStore.getState().selectedProjectId).toBe("p1");
  expect(useStore.getState().focusedSessionPk).toBeNull();
});

test("text and status events append lines to the session transcript", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" });
  s.applyCoreEvent({ kind: "status", session_pk: "s1", text: "Bash: ls" });
  s.applyCoreEvent({ kind: "text", session_pk: "s1", text: "hello" });
  const lines = useStore.getState().transcripts.s1;
  expect(lines.map((l) => l.kind)).toEqual(["status", "text"]);
  expect(lines[1].text).toBe("hello");
});

test("approval.requested adds a pending approval; resolving removes it", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s1", request_id: "r1", tool: "Bash", summary: "Bash: rm" });
  expect(useStore.getState().pendingApprovals).toHaveLength(1);
  useStore.getState().clearApproval("r1");
  expect(useStore.getState().pendingApprovals).toHaveLength(0);
});

test("multiple text events accumulate in order", () => {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSessionPk: null });
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "text", session_pk: "s1", text: "a" });
  s.applyCoreEvent({ kind: "text", session_pk: "s1", text: "b" });
  expect(useStore.getState().transcripts.s1.map((l) => l.text)).toEqual(["a", "b"]);
});

test("pending approvals from different sessions both count", () => {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSessionPk: null });
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s1", request_id: "r1", tool: "Bash", summary: "x" });
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s2", request_id: "r2", tool: "Write", summary: "y" });
  expect(useStore.getState().pendingApprovals).toHaveLength(2);
});

const runningSession = (pk: string) => ({
  sessionPk: pk,
  projectId: "p1",
  agentSessionId: null,
  worktreePath: null,
  branch: null,
  title: null,
  status: "running" as const,
  createdAt: null,
  lastActive: null,
});

test("result event flips the session status back to idle (so the composer leaves Stop mode)", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  useStore.getState().applyCoreEvent({ kind: "result", session_pk: "s1" });
  expect(useStore.getState().sessions[0].status).toBe("idle");
});

test("sessionEnded event marks the session ended", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1")] });
  useStore.getState().applyCoreEvent({ kind: "sessionEnded", session_pk: "s1" });
  expect(useStore.getState().sessions[0].status).toBe("ended");
});

test("result event leaves other sessions' status untouched", () => {
  reset();
  useStore.setState({ sessions: [runningSession("s1"), runningSession("s2")] });
  useStore.getState().applyCoreEvent({ kind: "result", session_pk: "s1" });
  const byPk = Object.fromEntries(useStore.getState().sessions.map((s) => [s.sessionPk, s.status]));
  expect(byPk).toEqual({ s1: "idle", s2: "running" });
});
