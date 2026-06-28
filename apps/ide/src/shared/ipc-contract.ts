import type {
  Project,
  Session,
  CoreEvent,
  StartSessionRequest,
  ContinueSessionRequest,
  ApprovalRequestFrame,
  DirEntry,
  ReadFileResult,
} from "@harness/protocol";

export type { DirEntry, ReadFileResult } from "@harness/protocol";

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
  "listConnections",
  "addConnection",
  "removeConnection",
  "selectConnection",
  "signIn",
  "signOut",
  "listDir",
  "readFile",
] as const;
export type IpcCommand = (typeof IPC_COMMANDS)[number];

export const EVENT_CHANNEL = "harness:event";
export const CONNECTION_CHANNEL = "harness:connection";
export const APPROVAL_CHANNEL = "harness:approval";
export const CONNECTIONS_CHANNEL = "harness:connections";
export type ConnState = "connecting" | "open" | "closed";

export interface ConnectionSummary {
  id: string;
  label: string;
  baseUrl: string;
  authMode: "loopback" | "oidc";
  active: boolean;
  signedIn: boolean;
}

export interface AddConnectionInput {
  label: string;
  baseUrl: string;
  authMode: "loopback" | "oidc";
  oidc?: { issuer: string; clientId: string; scopes: string };
}

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
  listConnections(): Promise<ConnectionSummary[]>;
  addConnection(input: AddConnectionInput): Promise<void>;
  removeConnection(id: string): Promise<void>;
  selectConnection(id: string): Promise<void>;
  signIn(id: string): Promise<void>;
  signOut(id: string): Promise<void>;
  onConnectionsChange(cb: (list: ConnectionSummary[]) => void): () => void;
  listDir(req: { sessionPk: string; path: string }): Promise<DirEntry[]>;
  readFile(req: { sessionPk: string; path: string }): Promise<ReadFileResult>;
}

declare global {
  interface Window {
    harness: HarnessBridge;
  }
}
