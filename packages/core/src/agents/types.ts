import type { PermMode } from "@harness/protocol";

export type ToolPermissionResult = { behavior: "allow"; updatedInput?: unknown } | { behavior: "deny"; message: string };

export type ApproveFn = (req: { tool: string; input: unknown }) => Promise<ToolPermissionResult>;

export type AgentEvent =
  | { type: "init"; sessionId: string }
  | { type: "status"; text: string }
  | { type: "text"; text: string }
  | { type: "result"; usage?: unknown; sessionId?: string }
  | { type: "error"; message: string };

export interface AgentRunInput {
  workdir: string;
  resume?: string;
  prompt: string;
  model?: string;
  effort?: string;
  permissionMode: PermMode;
  signal: AbortSignal;
  approve: ApproveFn;
  approval?: { url: string; sessionPk: string; hookBinPath: string };
}

export interface Agent {
  readonly id: string;
  run(input: AgentRunInput): AsyncIterable<AgentEvent>;
}
