import { test, expect } from "bun:test";
import { useStore } from "../src/renderer/store";

const frame = (id: string) => ({
  t: "approval.request" as const,
  requestId: id,
  sessionPk: "s1",
  tool: "Bash",
  summary: "Bash: ls",
  timeoutMs: 1000,
});

test("addApproval appends, removeApproval filters by requestId", () => {
  useStore.setState({ pendingApprovals: [] });
  useStore.getState().addApproval(frame("r1"));
  useStore.getState().addApproval(frame("r2"));
  expect(useStore.getState().pendingApprovals.map((a) => a.requestId)).toEqual(["r1", "r2"]);
  useStore.getState().removeApproval("r1");
  expect(useStore.getState().pendingApprovals.map((a) => a.requestId)).toEqual(["r2"]);
});

test("clearApprovals empties pendingApprovals", () => {
  useStore.setState({ pendingApprovals: [frame("r1"), frame("r2")] });
  useStore.getState().clearApprovals();
  expect(useStore.getState().pendingApprovals).toEqual([]);
});
