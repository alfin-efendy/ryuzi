import type { Project, Session, CoreEvent, StartSessionRequest, ContinueSessionRequest, ApprovalRequestFrame } from "@harness/protocol";

export const IPC_COMMANDS = [
  "listProjects",
  "getProject",
  "listSessions",
  "startSession",
  "continueSession",
  "stopSession",
  "endSession",
  "getConnId",
  "connectProject",
  "resolveApproval",
] as const;
export type IpcCommand = (typeof IPC_COMMANDS)[number];

export const EVENT_CHANNEL = "harness:event";
export const CONNECTION_CHANNEL = "harness:connection";
export const APPROVAL_CHANNEL = "harness:approval";
export type ConnState = "connecting" | "open" | "closed";

export interface HarnessBridge {
  listProjects(): Promise<Project[]>;
  getProject(id: string): Promise<Project | undefined>;
  listSessions(projectId?: string): Promise<Session[]>;
  startSession(req: StartSessionRequest): Promise<Session>;
  continueSession(req: ContinueSessionRequest): Promise<void>;
  stopSession(sessionPk: string): Promise<void>;
  endSession(sessionPk: string, opts?: { keepBranch?: boolean }): Promise<void>;
  getConnId(): Promise<string | null>;
  onEvent(cb: (e: CoreEvent) => void): () => void;
  onConnectionChange(cb: (s: ConnState) => void): () => void;
  connectProject(input: { gitUrl?: string; name?: string }): Promise<Project>;
  onApprovalRequest(cb: (r: ApprovalRequestFrame) => void): () => void;
  resolveApproval(requestId: string, decision: "allow" | "deny"): void;
}

declare global {
  interface Window {
    harness: HarnessBridge;
  }
}
