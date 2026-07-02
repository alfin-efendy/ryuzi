// @ryuzi/protocol — runtime-free shared contracts for the ryuzi monorepo.
// (Only plain const arrays at runtime — no Node/Bun deps — so any client can consume it.)

export const PERM_MODES = ["default", "acceptEdits", "bypassPermissions", "plan"] as const;
export type PermMode = (typeof PERM_MODES)[number];

export const SESSION_STATUSES = ["idle", "running", "interrupted", "ended"] as const;
export type SessionStatus = (typeof SESSION_STATUSES)[number];

export interface Project {
  projectId: string;
  name: string;
  workdir: string;
  source?: string;
  harness: string;
  model?: string;
  effort?: string;
  permMode: PermMode;
  createdBy?: string;
  createdAt?: number;
}

export interface Session {
  sessionPk: string;
  projectId: string;
  agentSessionId?: string;
  worktreePath?: string;
  branch?: string;
  title?: string;
  status: SessionStatus;
  startedBy?: string;
  createdAt?: number;
  lastActive?: number;
  resumeAttempts?: number;
}

export interface Surface {
  gateway: string;
  conversationId: string;
}

export interface AttachmentRef {
  name: string;
  url: string;
  contentType?: string;
  size: number;
}

export interface ProjectSettings {
  harness?: string;
  model?: string;
  effort?: string;
  permMode?: PermMode;
}

export interface ApprovalRequest {
  requestId: string;
  tool: string;
  summary: string;
  approverRoleIds?: string[];
  startedBy?: string;
  timeoutMs?: number;
}

export type ApprovalDecision = { decision: "allow" | "deny"; actor: string };

export type Unsubscribe = () => void;

export type CoreEvent =
  | { kind: "session.created"; sessionPk: string; projectId: string }
  | { kind: "status"; sessionPk: string; text: string }
  | { kind: "text"; sessionPk: string; text: string }
  | { kind: "result"; sessionPk: string; usage?: unknown }
  | { kind: "approval.requested"; sessionPk: string; requestId: string; tool: string; summary: string }
  | { kind: "error"; sessionPk: string; message: string }
  | { kind: "session.branch"; sessionPk: string; branch: string }
  | { kind: "notice"; sessionPk: string; text: string }
  | { kind: "session.ended"; sessionPk: string };

export interface StartSessionRequest {
  projectId: string;
  prompt: string;
  actor?: string;
  surface?: Surface;
  attachments?: AttachmentRef[];
}

export interface ContinueSessionRequest {
  sessionPk: string;
  prompt: string;
  actor?: string;
  attachments?: AttachmentRef[];
}

export interface ConnectProjectRequest {
  gateway: string;
  workspaceId: string;
  actor?: string;
  name?: string;
  gitUrl?: string;
  settings?: ProjectSettings;
  actorRoleIds?: string[];
}

// Client-facing API contract. All core methods are implemented.
// Future: resolveApproval.
export interface ControlPlaneApi {
  listProjects(): Project[];
  getProject(id: string): Project | undefined;
  listSessions(projectId?: string): Session[];
  startSession(req: StartSessionRequest): Promise<Session>;
  continueSession(req: ContinueSessionRequest): Promise<void>;
  connectProject(req: ConnectProjectRequest): Promise<Project>;
  stopSession(sessionPk: string): Promise<void>;
  endSession(sessionPk: string, opts?: { keepBranch?: boolean }): Promise<void>;
  requestApproval(req: { sessionPk: string; tool: string; input: unknown }): Promise<"allow" | "deny">;
  subscribe(handler: (e: CoreEvent) => void): Unsubscribe;
}
