import { create } from "zustand";
import { commands, type AgentInfo, type CommandInfo, type TodoItem } from "./bindings";
import { sessKey } from "./lib/session-key";

// Native-runtime metadata: the agents and slash commands available to a
// project, and a session's live todo list. Populated on demand from the
// native_agents / native_commands / session_todos Tauri commands.
//
// `agentsByProject`/`commandsByProject` are keyed by projectId (projects live
// on the local engine). `todosBySession`/`planCollapsed` are per-session, so
// they're keyed by `sessKey(runnerId, sessionPk)` — pks collide across runners.
type NativeState = {
  agentsByProject: Record<string, AgentInfo[]>;
  commandsByProject: Record<string, CommandInfo[]>;
  todosBySession: Record<string, TodoItem[]>;
  // Whether the floating plan panel is collapsed to a pill, per session.
  planCollapsed: Record<string, boolean>;
  loadAgents: (runnerId: string, projectId: string) => Promise<void>;
  loadCommands: (runnerId: string, projectId: string) => Promise<void>;
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

export const useNative = create<NativeState>((set) => ({
  agentsByProject: {},
  commandsByProject: {},
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
