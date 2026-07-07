import { test, expect, spyOn } from "bun:test";
import { useStore } from "./store";
import { commands } from "./bindings";

function reset() {
  useStore.setState({
    projects: [],
    sessions: [],
    transcripts: {},
    pendingApprovals: [],
    focusedSessionPk: null,
    selectedProjectId: null,
    lastSeq: {},
    loaded: {},
  });
}

test("selectProject sets the selected project and clears the focused session", () => {
  reset();
  useStore.setState({ focusedSessionPk: "s1" });
  useStore.getState().selectProject("p1");
  expect(useStore.getState().selectedProjectId).toBe("p1");
  expect(useStore.getState().focusedSessionPk).toBeNull();
});

test("message events project to rows by role/blockType and dedupe by seq", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({ kind: "sessionCreated", session_pk: "s1", project_id: "p1" });
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 1,
    role: "user",
    block_type: "text",
    payload: { text: "hi" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 2,
    role: "assistant",
    block_type: "thought",
    payload: { text: "pondering" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 3,
    role: "assistant",
    block_type: "text",
    payload: { text: "hello" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });
  // A duplicate/stale seq is ignored.
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 2,
    role: "assistant",
    block_type: "text",
    payload: { text: "dup" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });

  const rows = useStore.getState().transcripts.s1;
  expect(rows.map((r) => [r.seq, r.role, r.blockType, r.text])).toEqual([
    [1, "user", "text", "hi"],
    [2, "assistant", "thought", "pondering"],
    [3, "assistant", "text", "hello"],
  ]);
});

test("tool_call events append once, then merge in place by toolCallId (same-seq update)", () => {
  reset();
  const s = useStore.getState();
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 1,
    role: "assistant",
    block_type: "tool_call",
    payload: { name: "Bash", input: { command: "ls" } },
    tool_call_id: "tc-1",
    status: "pending",
    tool_kind: "execute",
  });
  // Completion re-emit re-uses seq 1 — must merge, not append, not be dropped.
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 1,
    role: "assistant",
    block_type: "tool_call",
    payload: { name: "Bash", input: { command: "ls" }, output: "file.txt" },
    tool_call_id: "tc-1",
    status: "completed",
    tool_kind: "execute",
  });
  // lastSeq high-water mark is untouched by the merge: a later fresh row still lands.
  s.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 2,
    role: "assistant",
    block_type: "text",
    payload: { text: "done" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });

  const rows = useStore.getState().transcripts.s1;
  expect(rows).toHaveLength(2);
  expect(rows[0].toolCallId).toBe("tc-1");
  expect(rows[0].toolStatus).toBe("completed");
  expect(rows[0].toolName).toBe("Bash");
  expect(rows[0].toolOutput).toBe("file.txt");
  expect(rows[1].text).toBe("done");
});

test("hydrateTranscript replaces the transcript from persisted messages and sets lastSeq", async () => {
  reset();
  const rows = [
    {
      sessionPk: "s1",
      seq: 1,
      role: "user",
      blockType: "text",
      payload: { text: "hi" },
      toolCallId: null,
      status: null,
      toolKind: null,
      createdAt: 1,
    },
    {
      sessionPk: "s1",
      seq: 2,
      role: "assistant",
      blockType: "tool_call",
      payload: { name: "Read", input: {}, output: { ok: true } },
      toolCallId: "tc-9",
      status: "completed",
      toolKind: "read",
      createdAt: 2,
    },
  ];
  await useStore.getState().hydrateTranscript("s1", async () => rows);
  const st = useStore.getState();
  expect(st.transcripts.s1[0].text).toBe("hi");
  expect(st.transcripts.s1[1].toolName).toBe("Read");
  expect(st.transcripts.s1[1].toolOutput).toBe(JSON.stringify({ ok: true }, null, 2));
  expect(st.lastSeq.s1).toBe(2);
  expect(st.loaded.s1).toBe(true);

  // A live non-tool event with seq <= lastSeq is ignored; a newer one appends.
  st.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 2,
    role: "assistant",
    block_type: "text",
    payload: { text: "again" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });
  st.applyCoreEvent({
    kind: "message",
    session_pk: "s1",
    seq: 3,
    role: "assistant",
    block_type: "text",
    payload: { text: "next" },
    tool_call_id: null,
    status: null,
    tool_kind: null,
  });
  expect(useStore.getState().transcripts.s1.map((r) => r.seq)).toEqual([1, 2, 3]);
});

test("hydrateTranscript keeps live rows that arrived during the fetch (and never regresses lastSeq)", async () => {
  reset();
  const dbRows = [
    {
      sessionPk: "s1",
      seq: 1,
      role: "user",
      blockType: "text",
      payload: { text: "hi" },
      toolCallId: null,
      status: null,
      toolKind: null,
      createdAt: 1,
    },
    {
      sessionPk: "s1",
      seq: 2,
      role: "assistant",
      blockType: "text",
      payload: { text: "yo" },
      toolCallId: null,
      status: null,
      toolKind: null,
      createdAt: 2,
    },
  ];
  await useStore.getState().hydrateTranscript("s1", async () => {
    // Simulates events landing while listMessages is in flight.
    useStore.getState().applyCoreEvent({
      kind: "message",
      session_pk: "s1",
      seq: 3,
      role: "assistant",
      block_type: "text",
      payload: { text: "live" },
      tool_call_id: null,
      status: null,
      tool_kind: null,
    });
    useStore.getState().applyCoreEvent({ kind: "error", session_pk: "s1", message: "transient" });
    return dbRows;
  });
  const st = useStore.getState();
  expect(st.transcripts.s1.map((r) => r.seq)).toEqual([1, 2, 3, 0]);
  expect(st.transcripts.s1[2].text).toBe("live");
  expect(st.transcripts.s1[3].blockType).toBe("error");
  expect(st.lastSeq.s1).toBe(3);
});

test("transient error events append a seq-0 error row", () => {
  reset();
  useStore.getState().applyCoreEvent({ kind: "error", session_pk: "s1", message: "spawn failed" });
  const rows = useStore.getState().transcripts.s1;
  expect(rows).toHaveLength(1);
  expect(rows[0].seq).toBe(0);
  expect(rows[0].blockType).toBe("error");
  expect(rows[0].text).toBe("spawn failed");
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
  startedBy: null,
  resumeAttempts: 0,
  branchOwned: true,
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

test("start forwards chat options so composer runtime, context, and attachments reach IPC", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "s1",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: "harness/s1",
      title: "/review",
      status: "running",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: true,
    },
  });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });

  await useStore.getState().start("p1", "/review", {
    runtimeId: "native",
    model: "fable",
    context: { branch: "feature/auth", voiceTranscript: null, references: ["src/main.rs"] },
    attachments: ["C:\\tmp\\notes.txt"],
  });

  expect(start).toHaveBeenCalledWith("p1", "/review", {
    runtimeId: "native",
    model: "fable",
    context: { branch: "feature/auth", voiceTranscript: null, references: ["src/main.rs"] },
    attachments: ["C:\\tmp\\notes.txt"],
    git: null,
  });
  expect(useStore.getState().focusedSessionPk).toBe("s1");

  start.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});

test("start forwards composer git options to IPC", async () => {
  reset();
  const start = spyOn(commands, "startSession").mockResolvedValue({
    status: "ok",
    data: {
      sessionPk: "s2",
      projectId: "p1",
      agentSessionId: null,
      worktreePath: null,
      branch: "feat/login",
      title: "go",
      status: "running",
      startedBy: "cockpit",
      createdAt: 1,
      lastActive: 1,
      resumeAttempts: 0,
      branchOwned: false,
    },
  });
  const listProjects = spyOn(commands, "listProjects").mockResolvedValue({ status: "ok", data: [] });
  const listSessions = spyOn(commands, "listSessions").mockResolvedValue({ status: "ok", data: [] });

  await useStore.getState().start("p1", "go", {
    git: { useWorktree: false, createBranch: true, branchName: "feat/login", baseBranch: null },
  });

  expect(start).toHaveBeenCalledWith("p1", "go", {
    runtimeId: null,
    model: null,
    context: null,
    attachments: [],
    git: { useWorktree: false, createBranch: true, branchName: "feat/login", baseBranch: null },
  });

  start.mockRestore();
  listProjects.mockRestore();
  listSessions.mockRestore();
});
