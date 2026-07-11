import { create } from "zustand";
import { commands, type AgentInfo, type CommandInfo, type TodoItem } from "./bindings";

// Native-runtime metadata: the agents and slash commands available to a
// project, and a session's live todo list. Populated on demand from the
// native_agents / native_commands / session_todos Tauri commands.
type NativeState = {
  agentsByProject: Record<string, AgentInfo[]>;
  commandsByProject: Record<string, CommandInfo[]>;
  todosBySession: Record<string, TodoItem[]>;
  // Whether the floating plan panel is collapsed to a pill, per session.
  planCollapsed: Record<string, boolean>;
  loadAgents: (projectId: string) => Promise<void>;
  loadCommands: (projectId: string) => Promise<void>;
  loadTodos: (sessionPk: string) => Promise<void>;
  setPlanCollapsed: (sessionPk: string, collapsed: boolean) => void;
  exportSession: (sessionPk: string) => Promise<string | null>;
  importSession: (projectId: string, data: string) => Promise<boolean>;
  shareSession: (sessionPk: string) => Promise<string | null>;
};

// Monotonic per-session fetch tokens for loadTodos: `todowrite` events and the
// settle-time reload can race, and command responses may resolve out of order.
// Only the newest in-flight fetch for a session may commit its result.
const todoFetchToken: Record<string, number> = {};

export const useNative = create<NativeState>((set) => ({
  agentsByProject: {},
  commandsByProject: {},
  todosBySession: {},
  planCollapsed: {},

  loadAgents: async (projectId) => {
    const res = await commands.nativeAgents(projectId);
    if (res.status === "ok") {
      set((s) => ({ agentsByProject: { ...s.agentsByProject, [projectId]: res.data } }));
    }
  },

  loadCommands: async (projectId) => {
    const res = await commands.nativeCommands(projectId);
    if (res.status === "ok") {
      set((s) => ({ commandsByProject: { ...s.commandsByProject, [projectId]: res.data } }));
    }
  },

  loadTodos: async (sessionPk) => {
    const token = (todoFetchToken[sessionPk] ?? 0) + 1;
    todoFetchToken[sessionPk] = token;
    const res = await commands.sessionTodos(sessionPk);
    if (res.status === "ok" && todoFetchToken[sessionPk] === token) {
      set((s) => ({ todosBySession: { ...s.todosBySession, [sessionPk]: res.data } }));
    }
  },

  setPlanCollapsed: (sessionPk, collapsed) => set((s) => ({ planCollapsed: { ...s.planCollapsed, [sessionPk]: collapsed } })),

  // Returns the session's portable JSON, or null on failure.
  exportSession: async (sessionPk) => {
    const res = await commands.exportSession(sessionPk);
    return res.status === "ok" ? res.data : null;
  },

  // Imports a previously exported session JSON under a project.
  importSession: async (projectId, data) => {
    const res = await commands.importSession(projectId, data);
    return res.status === "ok";
  },

  // Renders the session as a self-contained HTML document, or null on failure.
  shareSession: async (sessionPk) => {
    const res = await commands.shareSession(sessionPk);
    return res.status === "ok" ? res.data : null;
  },
}));
