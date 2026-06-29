// apps/router/src/gateways/discord/index.ts
import type { Gateway, MessageRef } from "../types";
import type { Surface, ApprovalRequest, ApprovalDecision, PermMode, ProjectSettings, AttachmentRef } from "@harness/protocol";

export interface InboundMessage {
  channelId: string;
  isThread: boolean;
  authorBot: boolean;
  authorId: string;
  mentionsBot: boolean;
  content: string;
  attachments: AttachmentRef[];
}
export interface InboundInteraction {
  name: string;
  userId: string;
  channelId: string;
  options: Record<string, string | undefined>;
  roleIds?: string[];
}
export interface DiscordPort {
  botUserId(): string | undefined;
  disconnect?(): Promise<void>;
  connect(handlers: {
    onMessage: (e: InboundMessage) => Promise<void>;
    onInteraction: (e: InboundInteraction, reply: (text: string) => Promise<void>) => Promise<void>;
  }): Promise<void>;
  createTextChannel(name: string): Promise<string>;
  createThread(channelId: string, name: string): Promise<string>;
  sendMessage(channelId: string, text: string): Promise<string>;
  editMessage(channelId: string, messageId: string, text: string): Promise<void>;
  requestApproval(
    conversationId: string,
    req: {
      requestId: string;
      tool: string;
      summary: string;
      approverRoleIds: string[];
      startedBy?: string;
      timeoutMs: number;
    },
  ): Promise<{ decision: "allow" | "deny"; actor: string }>;
}
export interface InboundRouter {
  onConnect(
    gatewayId: string,
    actor: string,
    opts: { name?: string; gitUrl?: string; settings?: ProjectSettings; actorRoleIds?: string[] },
  ): Promise<{ workspaceId: string; project: { name: string }; permModeDowngraded?: boolean }>;
  onStart(gatewayId: string, workspaceId: string, actor: string, prompt: string, attachments?: AttachmentRef[]): Promise<void>;
  onReply(gatewayId: string, conversationId: string, actor: string, prompt: string, attachments?: AttachmentRef[]): Promise<void>;
  onEnd(gatewayId: string, conversationId: string): Promise<void>;
  onStop(gatewayId: string, conversationId: string): Promise<void>;
}

function stripMentions(content: string): string {
  return content.replace(/<@!?\d+>/g, "").trim();
}

export class DiscordGateway implements Gateway {
  readonly id = "discord";
  constructor(
    private port: DiscordPort,
    private router: InboundRouter,
  ) {}

  async start(): Promise<void> {
    await this.port.connect({
      onMessage: (e) => this.handleMessage(e),
      onInteraction: (e, reply) => this.handleInteraction(e, reply),
    });
  }

  async stop(): Promise<void> {
    await this.port.disconnect?.();
  }

  async createWorkspace(name: string): Promise<string> {
    return this.port.createTextChannel(name);
  }
  async createConversation(workspaceId: string, title: string): Promise<string> {
    return this.port.createThread(workspaceId, title.slice(0, 90) || "session");
  }
  async postStatus(target: Surface, text: string): Promise<MessageRef> {
    const messageId = await this.port.sendMessage(target.conversationId, text);
    return { surface: target, messageId };
  }
  async editStatus(ref: MessageRef, text: string): Promise<void> {
    await this.port.editMessage(ref.surface.conversationId, ref.messageId, text);
  }
  async postResult(target: Surface, chunks: string[]): Promise<void> {
    for (const c of chunks) await this.port.sendMessage(target.conversationId, c);
  }
  async postError(target: Surface, text: string): Promise<void> {
    await this.port.sendMessage(target.conversationId, `❌ ${text}`);
  }
  async requestApproval(target: Surface, req: ApprovalRequest): Promise<ApprovalDecision> {
    return this.port.requestApproval(target.conversationId, {
      requestId: req.requestId,
      tool: req.tool,
      summary: req.summary,
      approverRoleIds: req.approverRoleIds ?? [],
      startedBy: req.startedBy,
      timeoutMs: req.timeoutMs ?? 300000,
    });
  }

  async handleMessage(e: InboundMessage): Promise<void> {
    if (e.authorBot) return;
    if (e.isThread) {
      if (e.content || e.attachments.length > 0) {
        await this.router.onReply(this.id, e.channelId, e.authorId, e.content, e.attachments);
      }
      return;
    }
    if (e.mentionsBot) {
      const prompt = stripMentions(e.content);
      if (prompt || e.attachments.length > 0) {
        await this.router.onStart(this.id, e.channelId, e.authorId, prompt, e.attachments);
      }
    }
  }

  async handleInteraction(e: InboundInteraction, reply: (text: string) => Promise<void>): Promise<void> {
    try {
      if (e.name === "connect") {
        const settings: ProjectSettings = {
          model: e.options.model,
          effort: e.options.effort,
          permMode: e.options.mode as PermMode | undefined,
        };
        const { workspaceId, permModeDowngraded } = await this.router.onConnect(this.id, e.userId, {
          name: e.options.name,
          gitUrl: e.options.git,
          settings,
          actorRoleIds: e.roleIds ?? [],
        });
        await reply(
          `✅ connected → <#${workspaceId}>` +
            (permModeDowngraded ? `\n⚠️ bypassPermissions requires an admin role — using default mode.` : ""),
        );
      } else if (e.name === "end") {
        await this.router.onEnd(this.id, e.channelId);
        await reply("🟥 session ended");
      } else if (e.name === "stop") {
        await this.router.onStop(this.id, e.channelId);
        await reply("⏹️ stopping the current turn");
      } else if (e.name === "status") {
        await reply("harness is running ✅");
      }
    } catch (err) {
      await reply(`❌ ${(err as Error).message}`);
    }
  }
}
