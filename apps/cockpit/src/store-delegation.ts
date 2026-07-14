import { create } from "zustand";
import { commands, type AgentRun, type CoreEvent, type Message } from "./bindings";

export const delegationSessionKey = (runnerId: string, sessionPk: string): string => `${runnerId}:${sessionPk}`;
export const delegationRunKey = (runnerId: string, sessionPk: string, runId: string): string => `${runnerId}:${sessionPk}:${runId}`;

type DelegationState = {
  bySession: Record<string, AgentRun[]>;
  transcriptByRun: Record<string, Message[]>;
  selectedBySession: Record<string, string | null>;
  load: (runnerId: string, sessionPk: string) => Promise<void>;
  select: (runnerId: string, sessionPk: string, runId: string | null) => void;
  loadTranscript: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  stop: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  retry: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  applyCoreEvent: (event: CoreEvent, runnerId: string) => void;
};

export const useDelegation = create<DelegationState>((set, get) => ({
  bySession: {},
  transcriptByRun: {},
  selectedBySession: {},

  load: async (runnerId, sessionPk) => {
    const result = await commands.getChildRuns(runnerId, sessionPk);
    if (result.status === "ok") {
      const key = delegationSessionKey(runnerId, sessionPk);
      set((state) => ({ bySession: { ...state.bySession, [key]: result.data } }));
    }
  },

  select: (runnerId, sessionPk, runId) => {
    const key = delegationSessionKey(runnerId, sessionPk);
    set((state) => ({ selectedBySession: { ...state.selectedBySession, [key]: runId } }));
    if (runId) void get().loadTranscript(runnerId, sessionPk, runId);
  },

  loadTranscript: async (runnerId, sessionPk, runId) => {
    const result = await commands.getChildTranscript(runnerId, sessionPk, runId);
    if (result.status === "ok") {
      const key = delegationRunKey(runnerId, sessionPk, runId);
      set((state) => ({ transcriptByRun: { ...state.transcriptByRun, [key]: result.data } }));
    }
  },

  stop: async (runnerId, sessionPk, runId) => {
    const sessionKey = delegationSessionKey(runnerId, sessionPk);
    const runs = get().bySession[sessionKey] ?? [];
    const previous = runs.find((run) => run.runId === runId);
    if (!previous) return;
    set((state) => ({
      bySession: {
        ...state.bySession,
        [sessionKey]: runs.map((run) => (run.runId === runId ? { ...run, status: "cancelled" } : run)),
      },
    }));
    const result = await commands.cancelChildRun(runnerId, sessionPk, runId);
    if (result.status === "error") {
      set((state) => ({
        bySession: {
          ...state.bySession,
          [sessionKey]: (state.bySession[sessionKey] ?? []).map((run) => (run.runId === runId ? previous : run)),
        },
      }));
    }
  },

  retry: async (runnerId, sessionPk, runId) => {
    const result = await commands.retryChildRun(runnerId, sessionPk, runId);
    if (result.status !== "ok") return;
    const key = delegationSessionKey(runnerId, sessionPk);
    set((state) => ({
      bySession: { ...state.bySession, [key]: [...(state.bySession[key] ?? []), result.data] },
      selectedBySession: { ...state.selectedBySession, [key]: result.data.runId },
    }));
    void get().loadTranscript(runnerId, sessionPk, result.data.runId);
  },

  applyCoreEvent: (event, runnerId) => {
    if (event.kind !== "agentRunChanged") return;
    void get().load(runnerId, event.session_pk);
    const selected = get().selectedBySession[delegationSessionKey(runnerId, event.session_pk)];
    if (selected === event.run_id) void get().loadTranscript(runnerId, event.session_pk, event.run_id);
  },
}));
