import { test, expect } from "bun:test";
import { useStore } from "./store";

function reset() {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSessionPk: null });
}

test("text and status events append lines to the session transcript", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" });
  s.applyCoreEvent({ kind: "status", session_pk: "s1", text: "Bash: ls" });
  s.applyCoreEvent({ kind: "text", session_pk: "s1", text: "hello" });
  const lines = useStore.getState().transcripts["s1"];
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
  expect(useStore.getState().transcripts["s1"].map((l) => l.text)).toEqual(["a", "b"]);
});

test("pending approvals from different sessions both count", () => {
  useStore.setState({ projects: [], sessions: [], transcripts: {}, pendingApprovals: [], focusedSessionPk: null });
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s1", request_id: "r1", tool: "Bash", summary: "x" });
  s.applyCoreEvent({ kind: "approvalRequested", session_pk: "s2", request_id: "r2", tool: "Write", summary: "y" });
  expect(useStore.getState().pendingApprovals).toHaveLength(2);
});
