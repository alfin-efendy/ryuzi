import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type AgentInfo,
  type CommandInfo,
  type ProjectCommandInfo,
  type ProjectCommandInputDto,
  type ProjectCommandMutationDto,
  type TodoItem,
} from "./bindings";
import { sessKey } from "./lib/session-key";

// Native-runtime metadata: the agents and slash commands available to a
// project, and a session's live todo list. Populated on demand from the
// native_agents / native_commands / session_todos Tauri commands.
//
// `agentsByProject`/`commandsByProject` are keyed by projectId (projects live
// on the local engine). `todosBySession`/`planCollapsed` are per-session, so
// they're keyed by `sessKey(runnerId, sessionPk)` — pks collide across runners.
export type ProjectCommandMutationResult =
  | { status: "success" }
  | { status: "conflict"; message: string }
  | { status: "error"; message: string };

type NativeState = {
  agentsByProject: Record<string, AgentInfo[]>;
  commandsByProject: Record<string, CommandInfo[]>;
  projectCommandsByProject: Record<string, ProjectCommandInfo[]>;
  todosBySession: Record<string, TodoItem[]>;
  // Whether the floating plan panel is collapsed to a pill, per session.
  planCollapsed: Record<string, boolean>;
  loadAgents: (runnerId: string, projectId: string) => Promise<void>;
  loadCommands: (runnerId: string, projectId: string) => Promise<void>;
  loadProjectCommands: (runnerId: string, projectId: string) => Promise<void>;
  createProjectCommand: (runnerId: string, projectId: string, input: ProjectCommandInputDto) => Promise<ProjectCommandMutationResult>;
  updateProjectCommand: (
    runnerId: string,
    projectId: string,
    command: ProjectCommandInfo,
    input: ProjectCommandMutationDto,
  ) => Promise<ProjectCommandMutationResult>;
  deleteProjectCommand: (runnerId: string, projectId: string, command: ProjectCommandInfo) => Promise<ProjectCommandMutationResult>;
  loadTodos: (runnerId: string, sessionPk: string) => Promise<void>;
  setPlanCollapsed: (runnerId: string, sessionPk: string, collapsed: boolean) => void;
  exportSession: (runnerId: string, sessionPk: string) => Promise<string | null>;
  importSession: (runnerId: string, projectId: string, data: string) => Promise<boolean>;
  shareSession: (runnerId: string, sessionPk: string) => Promise<string | null>;
};

// Monotonic per-session fetch tokens for loadTodos: `todowrite` events and the
// settle-time reload can race, and command responses may resolve out of order.
// Only the newest in-flight fetch for a session may commit its result. Keyed by
// the composite session key.
const todoFetchToken: Record<string, number> = {};
const projectCommandFetchToken: Record<string, number> = {};

function projectCommandKey(runnerId: string, projectId: string): string {
  return `${runnerId}:${projectId}`;
}

function invalidateProjectCommandFetch(runnerId: string, projectId: string): void {
  const key = projectCommandKey(runnerId, projectId);
  projectCommandFetchToken[key] = (projectCommandFetchToken[key] ?? 0) + 1;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const useNative = create<NativeState>((set) => ({
  agentsByProject: {},
  commandsByProject: {},
  projectCommandsByProject: {},
  todosBySession: {},
  planCollapsed: {},

  loadAgents: async (runnerId, projectId) => {
    const res = await commands.nativeAgents(runnerId, projectId);
    if (res.status === "ok") {
      set((s) => ({ agentsByProject: { ...s.agentsByProject, [projectId]: res.data } }));
    }
  },

  loadCommands: async (runnerId, projectId) => {
    const res = await commands.nativeCommands(runnerId, projectId);
    if (res.status === "ok") {
      set((s) => ({ commandsByProject: { ...s.commandsByProject, [projectId]: res.data } }));
    }
  },

  loadProjectCommands: async (runnerId, projectId) => {
    const key = projectCommandKey(runnerId, projectId);
    const token = (projectCommandFetchToken[key] ?? 0) + 1;
    projectCommandFetchToken[key] = token;
    try {
      const res = await commands.listProjectCommands(runnerId, projectId);
      if (res.status === "ok" && projectCommandFetchToken[key] === token) {
        set((s) => ({ projectCommandsByProject: { ...s.projectCommandsByProject, [projectId]: res.data } }));
      } else if (res.status === "error" && projectCommandFetchToken[key] === token) {
        toast.error(`Couldn't load project commands: ${res.error.message}`);
      }
    } catch (error) {
      if (projectCommandFetchToken[key] === token) toast.error(`Couldn't load project commands: ${errorMessage(error)}`);
    }
  },

  createProjectCommand: async (runnerId, projectId, input) => {
    invalidateProjectCommandFetch(runnerId, projectId);
    try {
      const res = await commands.createProjectCommand(runnerId, projectId, input);
      if (res.status !== "ok") {
        const message = res.error.message;
        toast.error(`Create command failed: ${message}`);
        return { status: "error", message };
      }
      invalidateProjectCommandFetch(runnerId, projectId);
      set((s) => ({
        projectCommandsByProject: {
          ...s.projectCommandsByProject,
          [projectId]: [...(s.projectCommandsByProject[projectId] ?? []), res.data].sort((a, b) => a.name.localeCompare(b.name)),
        },
      }));
      return { status: "success" };
    } catch (error) {
      const message = errorMessage(error);
      toast.error(`Create command failed: ${message}`);
      return { status: "error", message };
    }
  },

  updateProjectCommand: async (runnerId, projectId, command, input) => {
    invalidateProjectCommandFetch(runnerId, projectId);
    try {
      const res = await commands.updateProjectCommand(runnerId, projectId, command.name, command.revision, input);
      if (res.status !== "ok") {
        const message = res.error.message;
        const conflict = /modified externally|revision conflict/i.test(message);
        toast.error(conflict ? "Command changed externally. Reloaded the latest version." : `Update command failed: ${message}`);
        if (conflict) {
          await useNative.getState().loadProjectCommands(runnerId, projectId);
          return { status: "conflict", message };
        }
        return { status: "error", message };
      }
      invalidateProjectCommandFetch(runnerId, projectId);
      set((s) => ({
        projectCommandsByProject: {
          ...s.projectCommandsByProject,
          [projectId]: (s.projectCommandsByProject[projectId] ?? []).map((current) => (current.name === command.name ? res.data : current)),
        },
      }));
      return { status: "success" };
    } catch (error) {
      const message = errorMessage(error);
      toast.error(`Update command failed: ${message}`);
      return { status: "error", message };
    }
  },

  deleteProjectCommand: async (runnerId, projectId, command) => {
    invalidateProjectCommandFetch(runnerId, projectId);
    try {
      const res = await commands.deleteProjectCommand(runnerId, projectId, command.name, command.revision);
      if (res.status !== "ok") {
        const message = res.error.message;
        const conflict = /modified externally|revision conflict/i.test(message);
        toast.error(conflict ? "Command changed externally. Reloaded the latest version." : `Delete command failed: ${message}`);
        if (conflict) {
          await useNative.getState().loadProjectCommands(runnerId, projectId);
          return { status: "conflict", message };
        }
        return { status: "error", message };
      }
      invalidateProjectCommandFetch(runnerId, projectId);
      set((s) => ({
        projectCommandsByProject: {
          ...s.projectCommandsByProject,
          [projectId]: (s.projectCommandsByProject[projectId] ?? []).filter((current) => current.name !== command.name),
        },
      }));
      return { status: "success" };
    } catch (error) {
      const message = errorMessage(error);
      toast.error(`Delete command failed: ${message}`);
      return { status: "error", message };
    }
  },

  loadTodos: async (runnerId, sessionPk) => {
    const key = sessKey(runnerId, sessionPk);
    const token = (todoFetchToken[key] ?? 0) + 1;
    todoFetchToken[key] = token;
    const res = await commands.sessionTodos(runnerId, sessionPk);
    if (res.status === "ok" && todoFetchToken[key] === token) {
      set((s) => ({ todosBySession: { ...s.todosBySession, [key]: res.data } }));
    }
  },

  setPlanCollapsed: (runnerId, sessionPk, collapsed) =>
    set((s) => ({ planCollapsed: { ...s.planCollapsed, [sessKey(runnerId, sessionPk)]: collapsed } })),

  // Returns the session's portable JSON, or null on failure.
  exportSession: async (runnerId, sessionPk) => {
    const res = await commands.exportSession(runnerId, sessionPk);
    return res.status === "ok" ? res.data : null;
  },

  // Imports a previously exported session JSON under a project.
  importSession: async (runnerId, projectId, data) => {
    const res = await commands.importSession(runnerId, projectId, data);
    return res.status === "ok";
  },

  // Renders the session as a self-contained HTML document, or null on failure.
  shareSession: async (runnerId, sessionPk) => {
    const res = await commands.shareSession(runnerId, sessionPk);
    return res.status === "ok" ? res.data : null;
  },
}));
