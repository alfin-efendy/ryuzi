// packages/core/src/core/control-plane.ts
import type {
  ControlPlaneApi,
  Project,
  Session,
  CoreEvent,
  Unsubscribe,
  StartSessionRequest,
  ContinueSessionRequest,
  ConnectProjectRequest,
  PermMode,
} from "@harness/protocol";
import type { ProjectsStore } from "../store/projects";
import type { SessionsStore } from "../store/sessions";
import type { SettingsStore } from "../config/store";
import type { Agent, AgentRunInput } from "../agents/types";
import type { Gateway } from "../gateways/types";
import type { Telemetry } from "../observability/types";
import { NoopTelemetry } from "../observability/types";
import { mkdirSync, rmSync } from "node:fs";
import { basename, join } from "node:path";
import { createWorktree, removeWorktree, worktreePathFor } from "../agents/worktree";
import { expandHome } from "../config/paths";
import { Registry } from "./registry";
import { GatewayRegistry } from "./gateway-registry";
import { EventBus } from "./events";
import { resolveToolPolicy, summarizeTool, isAdmin, gatePermMode, parseRoleIds } from "./permissions";
import { materializeAttachments, buildManifest, parseAllowedExt, parseAllowedHosts, type MaterializeResult } from "./attachments";
import type { AttachmentRef } from "@harness/protocol";

export interface WorktreeOps {
  pathFor: (workdirRoot: string, projectId: string, sessionPk: string) => string;
  create: (repoDir: string, worktreePath: string, branch: string) => Promise<void>;
  remove: (repoDir: string, worktreePath: string) => Promise<void>;
}

export interface ControlPlaneDeps {
  projects: ProjectsStore;
  sessions: SessionsStore;
  settings: SettingsStore;
  workdirRoot: string;
  worktree?: WorktreeOps;
  telemetry?: Telemetry;
  fetchImpl?: typeof fetch;
}

const allowAll = async () => ({ behavior: "allow" as const });

export class ControlPlane implements ControlPlaneApi {
  readonly harnesses = new Registry<Agent>();
  readonly gateways = new GatewayRegistry();
  readonly events = new EventBus();
  approvalUrl?: string;
  hookBinPath?: string;
  private worktree: WorktreeOps;
  private telemetry: Telemetry;
  private running = new Map<string, AbortController>();
  private chains = new Map<string, Promise<unknown>>();

  constructor(private deps: ControlPlaneDeps) {
    this.worktree = deps.worktree ?? { pathFor: worktreePathFor, create: createWorktree, remove: removeWorktree };
    this.telemetry = deps.telemetry ?? new NoopTelemetry();
  }

  listProjects(): Project[] {
    return this.deps.projects.list();
  }
  getProject(id: string): Project | undefined {
    return this.deps.projects.get(id);
  }
  listSessions(projectId?: string): Session[] {
    return this.deps.sessions.list(projectId);
  }
  subscribe(handler: (e: CoreEvent) => void): Unsubscribe {
    return this.events.subscribe(handler);
  }
  emit(e: CoreEvent): void {
    this.events.emit(e);
  }

  private serial<T>(key: string, fn: () => Promise<T>): Promise<T> {
    const prev = this.chains.get(key) ?? Promise.resolve();
    const next = prev.catch(() => {}).then(fn);
    const stored = next.catch(() => {});
    this.chains.set(key, stored);
    stored.then(() => {
      if (this.chains.get(key) === stored) this.chains.delete(key);
    });
    return next;
  }

  async startSession(req: StartSessionRequest): Promise<Session> {
    const project = this.deps.projects.get(req.projectId);
    if (!project) throw new Error(`unknown project: ${req.projectId}`);
    const sessionPk = crypto.randomUUID();
    const branch = `harness/${sessionPk.slice(0, 8)}`;
    const worktreePath = this.worktree.pathFor(this.deps.workdirRoot, project.projectId, sessionPk);
    await this.worktree.create(project.workdir, worktreePath, branch);
    try {
      const now = Date.now();
      this.deps.sessions.insert({
        sessionPk,
        projectId: project.projectId,
        worktreePath,
        branch,
        title: req.prompt.slice(0, 80),
        status: "running",
        startedBy: req.actor,
        createdAt: now,
        lastActive: now,
      });
      if (req.surface) this.deps.sessions.addSurface(req.surface.gateway, req.surface.conversationId, sessionPk);
      this.events.emit({ kind: "session.created", sessionPk, projectId: project.projectId });
    } catch (e) {
      await this.worktree.remove(project.workdir, worktreePath).catch(() => {});
      throw e;
    }
    await this.serial(sessionPk, async () => {
      const finalPrompt = await this.withAttachments(sessionPk, req.prompt, req.attachments);
      return this.runHarness(project, sessionPk, finalPrompt, undefined);
    });
    return this.deps.sessions.get(sessionPk)!;
  }

  async continueSession(req: ContinueSessionRequest): Promise<void> {
    const session = this.deps.sessions.get(req.sessionPk);
    if (!session) throw new Error(`unknown session: ${req.sessionPk}`);
    const project = this.deps.projects.get(session.projectId);
    if (!project) throw new Error(`unknown project: ${session.projectId}`);
    this.deps.sessions.update(req.sessionPk, { status: "running" });
    await this.serial(req.sessionPk, async () => {
      const fresh = this.deps.sessions.get(req.sessionPk);
      const finalPrompt = await this.withAttachments(req.sessionPk, req.prompt, req.attachments);
      return this.runHarness(project, req.sessionPk, finalPrompt, fresh?.agentSessionId);
    });
  }

  private validateProjectName(name: string): void {
    if (name === "." || name === ".." || name.startsWith(".") || !/^[A-Za-z0-9._-]+$/.test(name)) {
      throw new Error(`invalid project name: ${name}`);
    }
  }

  async connectProject(req: ConnectProjectRequest): Promise<Project> {
    const rawRoot = this.deps.settings.get("workdir_root");
    if (!rawRoot) throw new Error("workdir_root is not set");
    const root = expandHome(rawRoot);

    let name: string;
    let source: string | undefined;
    if (req.name) {
      name = req.name;
      this.validateProjectName(name);
      const workdir = join(root, name);
      mkdirSync(workdir, { recursive: true });
      try {
        await Bun.$`git -C ${workdir} init -q`.quiet();
        await Bun.$`git -C ${workdir} commit -q --allow-empty -m init`.quiet();
      } catch (e) {
        rmSync(workdir, { recursive: true, force: true });
        throw e;
      }
    } else if (req.gitUrl) {
      // Strip trailing .git and extract the directory name
      let urlPath = req.gitUrl.replace(/\.git$/, "");
      name = basename(urlPath);
      if (!name) {
        // Fallback: if basename is empty, use the parent directory name
        name = basename(urlPath.replace(/\/$/, ""));
      }
      this.validateProjectName(name);
      const workdir = join(root, name);
      try {
        await Bun.$`git clone --quiet ${req.gitUrl} ${workdir}`.quiet();
      } catch (e) {
        rmSync(workdir, { recursive: true, force: true });
        throw e;
      }
      source = req.gitUrl;
    } else {
      throw new Error("connectProject requires name or gitUrl");
    }

    const workdir = join(root, name);
    const s = req.settings ?? {};
    const requestedMode = (s.permMode ?? (this.deps.settings.get("default_perm_mode") as PermMode) ?? "default") as PermMode;
    const admin = isAdmin({
      userRoleIds: req.actorRoleIds ?? [],
      adminRoleIds: parseRoleIds(this.deps.settings.get("admin_role_ids")),
    });
    const { mode: permMode } = gatePermMode(requestedMode, admin);
    const project: Project = {
      projectId: crypto.randomUUID(),
      name,
      workdir,
      source,
      harness: s.harness ?? (this.deps.settings.get("default_runtime") || "claude-code"),
      model: s.model ?? (this.deps.settings.get("default_model") || undefined),
      effort: s.effort ?? (this.deps.settings.get("default_effort") || undefined),
      permMode,
      createdBy: req.actor,
      createdAt: Date.now(),
    };
    this.deps.projects.insert(project);
    this.deps.projects.bind(req.gateway, req.workspaceId, project.projectId);
    return project;
  }

  async stopSession(sessionPk: string): Promise<void> {
    this.running.get(sessionPk)?.abort();
    if (this.deps.sessions.get(sessionPk)) this.deps.sessions.update(sessionPk, { status: "idle" });
  }

  async endSession(sessionPk: string, _opts?: { keepBranch?: boolean }): Promise<void> {
    this.running.get(sessionPk)?.abort();
    const session = this.deps.sessions.get(sessionPk);
    if (!session) return;
    const project = this.deps.projects.get(session.projectId);
    if (project && session.worktreePath) {
      await this.worktree.remove(project.workdir, session.worktreePath).catch(() => {});
    }
    const attRoot = expandHome(this.deps.settings.get("workdir_root") ?? "");
    try {
      rmSync(join(attRoot, ".harness-attachments", sessionPk), { recursive: true, force: true });
    } catch {}
    this.deps.sessions.update(sessionPk, { status: "ended" });
    this.events.emit({ kind: "session.ended", sessionPk });
  }

  async requestApproval(req: { sessionPk: string; tool: string; input: unknown }): Promise<"allow" | "deny"> {
    const decide = async (): Promise<"allow" | "deny"> => {
      const session = this.deps.sessions.get(req.sessionPk);
      if (!session) return "deny";
      const project = this.deps.projects.get(session.projectId);
      if (!project) return "deny";

      if (resolveToolPolicy(project.permMode, req.tool) === "allow") return "allow";

      const targets = this.deps.sessions
        .surfaces(req.sessionPk)
        .map((s) => ({ s, gw: this.gateways.get(s.gateway) }))
        .filter((t): t is { s: typeof t.s; gw: NonNullable<typeof t.gw> } => Boolean(t.gw));
      if (targets.length === 0) return "deny";

      const requestId = crypto.randomUUID();
      const summary = summarizeTool(req.tool, req.input);
      const rawTimeout = Number(this.deps.settings.get("approval_timeout_ms") ?? "300000");
      const timeoutMs = Number.isFinite(rawTimeout) ? rawTimeout : 300000;

      const approverRoleIds = parseRoleIds(this.deps.settings.get("approver_role_ids"));
      const startedBy = session.startedBy;

      this.events.emit({ kind: "approval.requested", sessionPk: req.sessionPk, requestId, tool: req.tool, summary });

      let timer: ReturnType<typeof setTimeout> | undefined;
      const timeoutP = new Promise<"timeout">((res) => {
        timer = setTimeout(() => res("timeout"), timeoutMs);
      });
      const decisionP = Promise.race(
        targets.map((t) =>
          t.gw
            .requestApproval(t.s, { requestId, tool: req.tool, summary, approverRoleIds, startedBy, timeoutMs })
            .catch(() => ({ decision: "deny" as const, actor: "error" })),
        ),
      );
      try {
        const result = await Promise.race([decisionP, timeoutP]);
        if (result === "timeout") return "deny";
        return result.decision === "allow" ? "allow" : "deny";
      } finally {
        if (timer) clearTimeout(timer);
      }
    };

    const decision = await decide();
    this.telemetry.count("approval." + decision, { tool: req.tool });
    return decision;
  }

  private async withAttachments(sessionPk: string, prompt: string, attachments?: AttachmentRef[]): Promise<string> {
    if (!attachments || attachments.length === 0) return prompt;
    const maxCount = Number(this.deps.settings.get("attachment_max_count") ?? "10");
    if (maxCount <= 0) return prompt || "User sent attachments, but attachment support is disabled."; // feature disabled
    const root = expandHome(this.deps.settings.get("workdir_root") ?? "");
    const destDir = join(root, ".harness-attachments", sessionPk);
    let result: MaterializeResult;
    try {
      result = await materializeAttachments(attachments, {
        destDir,
        maxBytes: Number(this.deps.settings.get("attachment_max_bytes") ?? "26214400"),
        maxCount,
        allowedExt: parseAllowedExt(this.deps.settings.get("attachment_allowed_ext")),
        allowedHosts: parseAllowedHosts(this.deps.settings.get("attachment_allowed_hosts")),
        fetchImpl: this.deps.fetchImpl,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      return prompt
        ? `${prompt}\n\n⚠️ Could not process attachments: ${msg}`
        : `User sent attachments, but they could not be processed: ${msg}`;
    }
    const manifest = buildManifest(result);
    if (!manifest) return prompt;
    if (!prompt) {
      return result.saved.length > 0
        ? `User sent attachments with no message text.\n\n${manifest}`
        : `User sent attachments but none could be processed:\n${manifest}`;
    }
    return `${prompt}\n\n${manifest}`;
  }

  private async runHarness(project: Project, sessionPk: string, prompt: string, resume?: string): Promise<void> {
    const harness = this.harnesses.create(project.harness);
    const controller = new AbortController();
    this.running.set(sessionPk, controller);
    const approval =
      project.permMode === "default" && this.approvalUrl && this.hookBinPath
        ? { url: this.approvalUrl, sessionPk, hookBinPath: this.hookBinPath }
        : undefined;
    const input: AgentRunInput = {
      workdir: this.deps.sessions.get(sessionPk)?.worktreePath ?? project.workdir,
      resume,
      prompt,
      model: project.model,
      effort: project.effort,
      permissionMode: project.permMode,
      signal: controller.signal,
      approve: allowAll,
      approval,
    };
    this.telemetry.count("session.run");
    const span = this.telemetry.startSpan("harness.run", {
      project_id: project.projectId,
      session_pk: sessionPk,
      resume: Boolean(resume),
    });
    try {
      for await (const ev of harness.run(input)) {
        if (ev.type === "init") this.deps.sessions.update(sessionPk, { agentSessionId: ev.sessionId });
        else if (ev.type === "status") this.events.emit({ kind: "status", sessionPk, text: ev.text });
        else if (ev.type === "text") this.events.emit({ kind: "text", sessionPk, text: ev.text });
        else if (ev.type === "result") {
          if (ev.sessionId) this.deps.sessions.update(sessionPk, { agentSessionId: ev.sessionId });
          this.events.emit({ kind: "result", sessionPk, usage: ev.usage });
        } else if (ev.type === "error") {
          span.setError(ev.message);
          this.telemetry.count("harness.error");
          this.events.emit({ kind: "error", sessionPk, message: ev.message });
        }
      }
    } finally {
      span.end();
      this.running.delete(sessionPk);
      const cur = this.deps.sessions.get(sessionPk);
      if (cur?.status === "running") this.deps.sessions.update(sessionPk, { status: "idle", lastActive: Date.now() });
    }
  }
}
