import { create } from "zustand";
import { commands, type AgentRun, type CoreEvent, type Message } from "./bindings";
import { dispatchSlotKey } from "./lib/agent-runs";

export const delegationSessionKey = (runnerId: string, sessionPk: string): string => `${runnerId}:${sessionPk}`;
export const delegationRunKey = (runnerId: string, sessionPk: string, runId: string): string => `${runnerId}:${sessionPk}:${runId}`;

export type LoadState = { status: "idle" | "loading" | "ready" | "error"; error: string | null };

type DelegationState = {
  bySession: Record<string, AgentRun[]>;
  rootRunBySession: Record<string, string | null>;
  rosterStateBySession: Record<string, LoadState>;
  transcriptByRun: Record<string, Message[]>;
  transcriptStateByRun: Record<string, LoadState>;
  seenRunsByDispatch: Record<string, Record<string, string[]>>;
  selectedBySession: Record<string, string | null>;
  load: (runnerId: string, sessionPk: string) => Promise<void>;
  select: (runnerId: string, sessionPk: string, runId: string | null) => void;
  loadTranscript: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  stop: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  retry: (runnerId: string, sessionPk: string, runId: string) => Promise<void>;
  applyCoreEvent: (event: CoreEvent, runnerId: string) => void;
};

const rosterRequests = new Map<string, Promise<void>>();
const pendingRosterRefreshes = new Set<string>();
const transcriptRequests = new Map<string, Promise<void>>();

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function linkedDispatchKey(run: AgentRun): string | null {
  return run.parentRunId && run.sourceToolCallId && run.dispatchIndex !== null
    ? dispatchSlotKey(run.parentRunId, run.sourceToolCallId, run.dispatchIndex)
    : null;
}

function upsertLiveMessage(rows: Message[], message: Message): Message[] {
  const index = message.toolCallId
    ? rows.findIndex((row) => row.toolCallId === message.toolCallId)
    : rows.findIndex((row) => row.seq === message.seq);
  if (index < 0) return [...rows, message].sort((a, b) => a.seq - b.seq);
  const next = rows.slice();
  next[index] = message;
  return next;
}

function mergeHydratedMessages(fetched: Message[], live: Message[]): Message[] {
  const maxFetchedSeq = fetched.reduce((max, message) => Math.max(max, message.seq), 0);
  let merged = fetched.slice();
  for (const message of live) {
    const matchingTool = message.toolCallId ? merged.findIndex((row) => row.toolCallId === message.toolCallId) : -1;
    const matchingSequence = merged.findIndex((row) => row.seq === message.seq);
    const matching = matchingTool >= 0 ? matchingTool : matchingSequence;
    if (matching >= 0) {
      merged[matching] = message;
    } else if (message.seq > maxFetchedSeq) {
      merged = upsertLiveMessage(merged, message);
    }
  }
  return merged.sort((a, b) => a.seq - b.seq);
}

function eventMessage(event: Extract<CoreEvent, { kind: "agentRunMessage" }>): Message {
  return {
    sessionPk: event.session_pk,
    seq: event.seq,
    role: event.role,
    blockType: event.block_type,
    payload: event.payload,
    toolCallId: event.tool_call_id,
    status: event.status,
    toolKind: event.tool_kind,
    createdAt: Date.now(),
    speaker: event.speaker,
  };
}

export const useDelegation = create<DelegationState>((set, get) => ({
  bySession: {},
  rootRunBySession: {},
  rosterStateBySession: {},
  transcriptByRun: {},
  transcriptStateByRun: {},
  seenRunsByDispatch: {},
  selectedBySession: {},

  load: (runnerId, sessionPk) => {
    const key = delegationSessionKey(runnerId, sessionPk);
    const existing = rosterRequests.get(key);
    if (existing) return existing;
    set((state) => ({ rosterStateBySession: { ...state.rosterStateBySession, [key]: { status: "loading", error: null } } }));
    const request = (async () => {
      try {
        const result = await commands.getChildRuns(runnerId, sessionPk);
        if (result.status === "error") throw new Error(result.error.message);
        set((state) => {
          const seen = { ...(state.seenRunsByDispatch[key] ?? {}) };
          for (const run of result.data.runs) {
            const dispatch = linkedDispatchKey(run);
            if (!dispatch) continue;
            const ids = seen[dispatch] ?? [];
            if (!ids.includes(run.runId)) seen[dispatch] = [...ids, run.runId];
          }
          return {
            rootRunBySession: { ...state.rootRunBySession, [key]: result.data.rootRunId },
            bySession: { ...state.bySession, [key]: result.data.runs },
            rosterStateBySession: { ...state.rosterStateBySession, [key]: { status: "ready", error: null } },
            seenRunsByDispatch: { ...state.seenRunsByDispatch, [key]: seen },
          };
        });
        for (const run of result.data.runs) {
          const transcriptKey = delegationRunKey(runnerId, sessionPk, run.runId);
          if ((run.status === "queued" || run.status === "running") && get().transcriptStateByRun[transcriptKey]?.status !== "ready") {
            void get().loadTranscript(runnerId, sessionPk, run.runId);
          }
        }
      } catch (error) {
        set((state) => ({
          rosterStateBySession: { ...state.rosterStateBySession, [key]: { status: "error", error: errorMessage(error) } },
        }));
      } finally {
        rosterRequests.delete(key);
        if (pendingRosterRefreshes.delete(key)) {
          void get().load(runnerId, sessionPk);
        }
      }
    })();
    rosterRequests.set(key, request);
    return request;
  },

  select: (runnerId, sessionPk, runId) => {
    const key = delegationSessionKey(runnerId, sessionPk);
    set((state) => ({ selectedBySession: { ...state.selectedBySession, [key]: runId } }));
    if (runId) void get().loadTranscript(runnerId, sessionPk, runId);
  },

  loadTranscript: (runnerId, sessionPk, runId) => {
    const key = delegationRunKey(runnerId, sessionPk, runId);
    const existing = transcriptRequests.get(key);
    if (existing) return existing;
    set((state) => ({ transcriptStateByRun: { ...state.transcriptStateByRun, [key]: { status: "loading", error: null } } }));
    const request = (async () => {
      try {
        const result = await commands.getChildTranscript(runnerId, sessionPk, runId);
        if (result.status === "error") throw new Error(result.error.message);
        set((state) => ({
          transcriptByRun: { ...state.transcriptByRun, [key]: mergeHydratedMessages(result.data, state.transcriptByRun[key] ?? []) },
          transcriptStateByRun: { ...state.transcriptStateByRun, [key]: { status: "ready", error: null } },
        }));
      } catch (error) {
        set((state) => ({
          transcriptStateByRun: { ...state.transcriptStateByRun, [key]: { status: "error", error: errorMessage(error) } },
        }));
      } finally {
        transcriptRequests.delete(key);
      }
    })();
    transcriptRequests.set(key, request);
    return request;
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
    await get().loadTranscript(runnerId, sessionPk, result.data.runId);
  },

  applyCoreEvent: (event, runnerId) => {
    if (event.kind === "agentRunChanged") {
      const key = delegationSessionKey(runnerId, event.session_pk);
      if (rosterRequests.has(key)) {
        pendingRosterRefreshes.add(key);
      } else {
        void get().load(runnerId, event.session_pk);
      }
      return;
    }
    if (event.kind !== "agentRunMessage") return;
    const key = delegationRunKey(runnerId, event.session_pk, event.run_id);
    const message = eventMessage(event);
    set((state) => ({
      transcriptByRun: { ...state.transcriptByRun, [key]: upsertLiveMessage(state.transcriptByRun[key] ?? [], message) },
    }));
  },
}));
