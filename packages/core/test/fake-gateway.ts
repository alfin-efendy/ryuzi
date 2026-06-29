import type { Gateway, MessageRef } from "../src/gateways/types";
import type { Surface, ApprovalRequest, ApprovalDecision } from "@harness/protocol";

export class FakeGateway implements Gateway {
  readonly id: string;
  calls: string[] = [];
  private n = 0;
  constructor(id = "fake") {
    this.id = id;
  }
  async start(): Promise<void> {}
  async createWorkspace(name: string): Promise<string> {
    this.calls.push(`createWorkspace:${name}`);
    return `ws-${name}`;
  }
  async createConversation(workspaceId: string, title: string): Promise<string> {
    this.calls.push(`createConversation:${workspaceId}:${title}`);
    return `conv-${++this.n}`;
  }
  async postStatus(t: Surface, text: string): Promise<MessageRef> {
    this.calls.push(`postStatus:${t.conversationId}:${text}`);
    return { surface: t, messageId: `m-${++this.n}` };
  }
  async editStatus(ref: MessageRef, text: string): Promise<void> {
    this.calls.push(`editStatus:${ref.messageId}:${text}`);
  }
  async postResult(t: Surface, chunks: string[]): Promise<void> {
    this.calls.push(`postResult:${t.conversationId}:${chunks.join("|")}`);
  }
  async postError(t: Surface, text: string): Promise<void> {
    this.calls.push(`postError:${t.conversationId}:${text}`);
  }
  approvalHandler?: (req: ApprovalRequest) => Promise<ApprovalDecision>;
  async requestApproval(t: Surface, req: ApprovalRequest): Promise<ApprovalDecision> {
    this.calls.push(`requestApproval:${t.conversationId}:${req.tool}`);
    if (this.approvalHandler) return this.approvalHandler(req);
    return { decision: "allow", actor: "x" };
  }
}
