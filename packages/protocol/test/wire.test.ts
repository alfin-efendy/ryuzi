import { test, expect } from "bun:test";
import { RPC_METHODS } from "../src/index";
import type { RpcRequest, RpcResponse, ServerFrame, ClientFrame, ApprovalRequestFrame } from "../src/index";

test("RPC_METHODS lists the eight command methods", () => {
  expect([...RPC_METHODS]).toEqual([
    "listProjects",
    "getProject",
    "listSessions",
    "startSession",
    "continueSession",
    "connectProject",
    "stopSession",
    "endSession",
  ]);
});

test("wire types compile with the expected shapes", () => {
  const req: RpcRequest = { id: "1", method: "listProjects" };
  const ok: RpcResponse = { id: "1", ok: true, result: [] };
  const err: RpcResponse = { id: "1", ok: false, error: "boom" };
  const appr: ApprovalRequestFrame = { t: "approval.request", requestId: "r1", sessionPk: "s1", tool: "Bash", summary: "Bash: ls", timeoutMs: 1000 };
  const sf: ServerFrame = appr;
  const cf: ClientFrame = { t: "approval.resolve", requestId: "r1", decision: "allow" };
  expect(req.method).toBe("listProjects");
  expect(ok.ok).toBe(true);
  expect(err.ok).toBe(false);
  expect(sf.t).toBe("approval.request");
  expect(cf.t).toBe("approval.resolve");
});
