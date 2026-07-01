import { test, expect } from "bun:test";
import { useStore } from "./store";

function reset() {
  useStore.setState({
    projects: [], sessions: [], transcripts: {}, pendingApprovals: [],
    focusedSessionPk: null, selectedProjectId: null, lastSeq: {}, loaded: {},
  });
}

test("selectProject sets the selected project and clears the focused session", () => {
  reset();
  useStore.setState({ focusedSessionPk: "s1" });
  useStore.getState().selectProject("p1");
  expect(useStore.getState().selectedProjectId).toBe("p1");
  expect(useStore.getState().focusedSessionPk).toBeNull();
});

test("message events project to lines by role/blockType and dedupe by seq", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" });
  s.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 1, role: "user", block_type: "text",
    payload: { text: "hi" }, tool_call_id: null, status: null, tool_kind: null });
  s.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 2, role: "assistant", block_type: "status",
    payload: { summary: "Bash: ls" }, tool_call_id: null, status: null, tool_kind: null });
  s.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 3, role: "assistant", block_type: "text",
    payload: { text: "hello" }, tool_call_id: null, status: null, tool_kind: null });
  // A duplicate/stale seq is ignored.
  s.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 2, role: "assistant", block_type: "status",
    payload: { summary: "dup" }, tool_call_id: null, status: null, tool_kind: null });

  const lines = useStore.getState().transcripts.s1;
  expect(lines.map((l) => l.kind)).toEqual(["user", "status", "text"]);
  expect(lines.map((l) => l.text)).toEqual(["hi", "Bash: ls", "hello"]);
});

test("hydrateTranscript replaces the transcript from persisted messages and sets lastSeq", async () => {
  reset();
  const rows = [
    { sessionPk: "s1", seq: 1, role: "user", blockType: "text", payload: { text: "hi" },
      toolCallId: null, status: null, toolKind: null, createdAt: 1 },
    { sessionPk: "s1", seq: 2, role: "assistant", blockType: "text", payload: { text: "yo" },
      toolCallId: null, status: null, toolKind: null, createdAt: 2 },
  ];
  await useStore.getState().hydrateTranscript("s1", async () => rows);
  const st = useStore.getState();
  expect(st.transcripts.s1.map((l) => l.text)).toEqual(["hi", "yo"]);
  expect(st.lastSeq.s1).toBe(2);
  expect(st.loaded.s1).toBe(true);

  // A live event with seq <= lastSeq is ignored; a newer one appends.
  st.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 2, role: "assistant", block_type: "text",
    payload: { text: "again" }, tool_call_id: null, status: null, tool_kind: null });
  st.applyCoreEvent({ kind: "message", session_pk: "s1", seq: 3, role: "assistant", block_type: "text",
    payload: { text: "next" }, tool_call_id: null, status: null, tool_kind: null });
  expect(useStore.getState().transcripts.s1.map((l) => l.text)).toEqual(["hi", "yo", "next"]);
});

test("approval.requested adds a pending approval; resolving removes it", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s1", request_id: "r1", tool: "Bash", summary: "Bash: rm" });
  expect(useStore.getState().pendingApprovals).toHaveLength(1);
  useStore.getState().clearApproval("r1");
  expect(useStore.getState().pendingApprovals).toHaveLength(0);
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
