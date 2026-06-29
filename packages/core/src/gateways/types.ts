import type { Surface, ApprovalRequest, ApprovalDecision } from "@harness/protocol";

export interface MessageRef {
  surface: Surface;
  messageId: string;
}

export interface Gateway {
  readonly id: string;
  start(): Promise<void>;
  stop?(): Promise<void> | void;
  createWorkspace(name: string): Promise<string>;
  createConversation(workspaceId: string, title: string): Promise<string>;
  postStatus(target: Surface, text: string): Promise<MessageRef>;
  editStatus(ref: MessageRef, text: string): Promise<void>;
  postResult(target: Surface, chunks: string[]): Promise<void>;
  postError(target: Surface, text: string): Promise<void>;
  requestApproval(target: Surface, req: ApprovalRequest): Promise<ApprovalDecision>;
}
