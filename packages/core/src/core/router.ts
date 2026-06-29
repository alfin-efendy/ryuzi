// packages/core/src/core/router.ts
import type { CoreEvent, Surface, Project, ProjectSettings, AttachmentRef } from "@harness/protocol";
import type { MessageRef } from "../gateways/types";
import type { SessionsStore } from "../store/sessions";
import type { ProjectsStore } from "../store/projects";
import type { ControlPlane } from "./control-plane";
import { basename } from "node:path";

interface RenderState {
  status: Map<string, MessageRef>; // surfaceKey -> status message
  buffer: string[];
}

function surfaceKey(s: Surface): string {
  return `${s.gateway}:${s.conversationId}`;
}

export function chunk(s: string): string[] {
  if (!s) return ["(done)"];
  const out: string[] = [];
  for (let i = 0; i < s.length; i += 1900) out.push(s.slice(i, i + 1900));
  return out;
}

export class Router {
  private state = new Map<string, RenderState>();
  private chains = new Map<string, Promise<unknown>>();

  constructor(
    private core: ControlPlane,
    private sessions: SessionsStore,
    private projects: ProjectsStore,
  ) {
    this.core.subscribe((e) => this.onEvent(e));
  }

  async onConnect(
    gatewayId: string,
    actor: string,
    opts: { name?: string; gitUrl?: string; settings?: ProjectSettings; actorRoleIds?: string[] },
  ): Promise<{ workspaceId: string; project: Project; permModeDowngraded: boolean }> {
    const gw = this.core.gateways.get(gatewayId);
    if (!gw) throw new Error(`unknown gateway: ${gatewayId}`);
    const display = opts.name ?? (opts.gitUrl ? basename(opts.gitUrl).replace(/\.git$/, "") : undefined);
    if (!display) throw new Error("connect requires name or gitUrl");
    const workspaceId = await gw.createWorkspace(display);
    const project = await this.core.connectProject({ gateway: gatewayId, workspaceId, actor, ...opts });
    const permModeDowngraded = opts.settings?.permMode === "bypassPermissions" && project.permMode !== "bypassPermissions";
    return { workspaceId, project, permModeDowngraded };
  }

  async onStart(gatewayId: string, workspaceId: string, actor: string, prompt: string, attachments?: AttachmentRef[]): Promise<void> {
    const project = this.projects.resolveByWorkspace(gatewayId, workspaceId);
    if (!project) return;
    const gw = this.core.gateways.get(gatewayId);
    if (!gw) throw new Error(`unknown gateway: ${gatewayId}`);
    const conversationId = await gw.createConversation(workspaceId, prompt.slice(0, 80) || "session");
    await this.core.startSession({
      projectId: project.projectId,
      prompt,
      actor,
      surface: { gateway: gatewayId, conversationId },
      attachments,
    });
  }

  async onReply(gatewayId: string, conversationId: string, actor: string, prompt: string, attachments?: AttachmentRef[]): Promise<void> {
    const session = this.sessions.resolveByConversation(gatewayId, conversationId);
    if (!session) return;
    await this.core.continueSession({ sessionPk: session.sessionPk, prompt, actor, attachments });
  }

  async onEnd(gatewayId: string, conversationId: string): Promise<void> {
    const session = this.sessions.resolveByConversation(gatewayId, conversationId);
    if (session) await this.core.endSession(session.sessionPk);
  }

  async onStop(gatewayId: string, conversationId: string): Promise<void> {
    const session = this.sessions.resolveByConversation(gatewayId, conversationId);
    if (session) await this.core.stopSession(session.sessionPk);
  }

  idle(): Promise<void> {
    return Promise.all([...this.chains.values()].map((p) => p.catch(() => {}))).then(() => {});
  }

  private serial(key: string, fn: () => Promise<void>): void {
    const prev = this.chains.get(key) ?? Promise.resolve();
    const stored = prev
      .catch(() => {})
      .then(fn)
      .catch(() => {});
    this.chains.set(key, stored);
    stored.then(() => {
      if (this.chains.get(key) === stored) this.chains.delete(key);
    });
  }

  private stateFor(sessionPk: string): RenderState {
    let st = this.state.get(sessionPk);
    if (!st) {
      st = { status: new Map(), buffer: [] };
      this.state.set(sessionPk, st);
    }
    return st;
  }

  private onEvent(e: CoreEvent): void {
    if (e.kind === "text") {
      this.stateFor(e.sessionPk).buffer.push(e.text);
      return;
    }
    if (e.kind === "status") {
      this.serial(e.sessionPk, () => this.renderStatus(e.sessionPk, e.text));
      return;
    }
    if (e.kind === "result") {
      this.serial(e.sessionPk, () => this.renderResult(e.sessionPk));
      return;
    }
    if (e.kind === "error") {
      this.serial(e.sessionPk, () => this.renderError(e.sessionPk, e.message));
      return;
    }
    if (e.kind === "session.ended") {
      this.state.delete(e.sessionPk);
      return;
    }
  }

  private async renderStatus(sessionPk: string, text: string): Promise<void> {
    const st = this.stateFor(sessionPk);
    for (const surface of this.sessions.surfaces(sessionPk)) {
      const gw = this.core.gateways.get(surface.gateway);
      if (!gw) continue;
      const key = surfaceKey(surface);
      const ref = st.status.get(key);
      if (ref) await gw.editStatus(ref, text);
      else st.status.set(key, await gw.postStatus(surface, text));
    }
  }

  private async renderResult(sessionPk: string): Promise<void> {
    const st = this.stateFor(sessionPk);
    const chunks = chunk(st.buffer.join(""));
    for (const surface of this.sessions.surfaces(sessionPk)) {
      const gw = this.core.gateways.get(surface.gateway);
      if (gw) await gw.postResult(surface, chunks);
    }
    this.state.delete(sessionPk);
  }

  private async renderError(sessionPk: string, message: string): Promise<void> {
    for (const surface of this.sessions.surfaces(sessionPk)) {
      const gw = this.core.gateways.get(surface.gateway);
      if (gw) await gw.postError(surface, message);
    }
    this.state.delete(sessionPk);
  }
}
