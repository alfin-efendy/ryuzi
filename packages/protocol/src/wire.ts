// @harness/protocol — wire contract for the network transport (types + const only; runtime-free).
import type { CoreEvent } from "./index";

export const RPC_METHODS = [
  "listProjects",
  "getProject",
  "listSessions",
  "startSession",
  "continueSession",
  "connectProject",
  "stopSession",
  "endSession",
] as const;
export type RpcMethod = (typeof RPC_METHODS)[number];

export interface RpcRequest {
  id: string;
  method: RpcMethod;
  params?: unknown;
}

export type RpcResponse =
  | { id: string; ok: true; result: unknown }
  | { id: string; ok: false; error: string };

// Named so @harness/client can reference the same shape. Distinct from the
// in-process `ApprovalRequest` interface in index.ts (which has no `t`/`sessionPk`).
export interface ApprovalRequestFrame {
  t: "approval.request";
  requestId: string;
  sessionPk: string;
  tool: string;
  summary: string;
  timeoutMs: number;
}

export type ServerFrame =
  | { t: "hello"; connId: string }
  | { t: "event"; event: CoreEvent }
  | ApprovalRequestFrame
  | { t: "pong" };

export type ClientFrame =
  | { t: "approval.resolve"; requestId: string; decision: "allow" | "deny" }
  | { t: "ping" };

export interface WsTicketResponse {
  ticket: string;
}
