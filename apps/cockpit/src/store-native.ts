import { create } from "zustand";
import { commands, type AgentInfo, type CommandInfo, type TodoItem } from "./bindings";

// Native-runtime metadata: the agents and slash commands available to a
// project, and a session's live todo list. Populated on demand from the
// native_agents / native_commands / session_todos Tauri commands.
type NativeState = {
  agentsByProject: Record<string, AgentInfo[]>;
  commandsByProject: Record<string, CommandInfo[]>;
  todosBySession: Record<string, TodoItem[]>;
  loadAgents: (projectId: string) => Promise<void>;
  loadCommands: (projectId: string) => Promise<void>;
  loadTodos: (sessionPk: string) => Promise<void>;
  exportSession: (sessionPk: string) => Promise<string | null>;
  importSession: (projectId: string, data: string) => Promise<boolean>;
};

export const useNative = create<NativeState>((set) => ({
  agentsByProject: {},
  commandsByProject: {},
  todosBySession: {},

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
    const res = await commands.sessionTodos(sessionPk);
    if (res.status === "ok") {
      set((s) => ({ todosBySession: { ...s.todosBySession, [sessionPk]: res.data } }));
    }
  },

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
}));
