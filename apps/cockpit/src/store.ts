import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  events,
  type Project,
  type Session,
  type CoreEvent,
  type Message,
  type ChatRequestOptions,
  type GitOptions,
  type PermMode,
} from "./bindings";
import { basename } from "./lib/paths";
import { useRuntimes } from "./store-runtimes";
import { messageToRow, mergeToolRow, type Row } from "./lib/transcript";

export type PendingApproval = { sessionPk: string; requestId: string; tool: string; summary: string };
export type ChatOptions = {
  runtimeId?: string | null;
  model?: string | null;
  context?: {
    branch?: string | null;
    voiceTranscript?: string | null;
    references?: string[];
  } | null;
  attachments?: string[];
  git?: GitOptions | null;
};

type State = {
  projects: Project[];
  sessions: Session[];
  transcripts: Record<string, Row[]>;
  pendingApprovals: PendingApproval[];
  focusedSessionPk: string | null;
  selectedProjectId: string | null;
  lastSeq: Record<string, number>;
  loaded: Record<string, boolean>;
  applyCoreEvent: (e: CoreEvent) => void;
  clearApproval: (requestId: string) => void;
  setFocused: (pk: string | null) => void;
  selectProject: (id: string | null) => void;
  refresh: () => Promise<void>;
  addProject: () => Promise<void>;
  /** Pin (or clear, with null) the model future turns of this project use. */
  setProjectModel: (projectId: string, model: string | null) => Promise<void>;
  /** Change the permission mode future turns of this project run under. */
  setProjectPermMode: (projectId: string, permMode: PermMode) => Promise<void>;
  /** Resolves true as soon as the backend accepts — navigate immediately;
   *  the session list refresh completes in the background. */
  start: (projectId: string, prompt: string, options?: ChatOptions | null) => Promise<boolean>;
  send: (sessionPk: string, prompt: string, options?: ChatOptions | null) => Promise<void>;
  stop: (sessionPk: string) => Promise<void>;
  /** Resolves true only when the backend teardown actually succeeded. */
  end: (sessionPk: string) => Promise<boolean>;
  resolveApproval: (requestId: string, allow: boolean) => Promise<void>;
  hydrateTranscript: (pk: string, fetcher?: (pk: string) => Promise<Message[]>) => Promise<void>;
  init: () => Promise<void>;
};

function append(map: Record<string, Row[]>, pk: string, row: Row): Record<string, Row[]> {
  return { ...map, [pk]: [...(map[pk] ?? []), row] };
}

function toChatRequestOptions(options?: ChatOptions | null): ChatRequestOptions | null {
  if (!options) return null;
  return {
    runtimeId: options.runtimeId ?? null,
    model: options.model ?? null,
    context: options.context
      ? {
          branch: options.context.branch ?? null,
          voiceTranscript: options.context.voiceTranscript ?? null,
          references: options.context.references ?? [],
        }
      : null,
    attachments: options.attachments ?? [],
    git: options.git ?? null,
  };
}

export const useStore = create<State>((set, get) => ({
  projects: [],
  sessions: [],
  transcripts: {},
  pendingApprovals: [],
  focusedSessionPk: null,
  selectedProjectId: null,
  lastSeq: {},
  loaded: {},

  applyCoreEvent: (e) =>
    set((st) => {
      switch (e.kind) {
        case "sessionCreated":
          return {
            transcripts: { ...st.transcripts, [e.session_pk]: st.transcripts[e.session_pk] ?? [] },
            loaded: { ...st.loaded, [e.session_pk]: true },
          };
        case "message": {
          const pk = e.session_pk;
          // Tool updates re-use the original row's seq — upsert by identity
          // BEFORE the seq high-water guard would drop them as stale.
          if (e.tool_call_id) {
            const rows = st.transcripts[pk] ?? [];
            const idx = rows.findIndex((r) => r.toolCallId === e.tool_call_id);
            if (idx >= 0) {
              const next = rows.slice();
              next[idx] = mergeToolRow(rows[idx], e.payload, e.status, e.tool_kind);
              return { transcripts: { ...st.transcripts, [pk]: next } };
            }
          }
          const prev = st.lastSeq[pk] ?? 0;
          if (e.seq <= prev) return {}; // stale/duplicate (covers reload/replay races)
          const row = messageToRow(e.seq, e.role, e.block_type, e.payload, e.tool_call_id, e.status, e.tool_kind, Date.now());
          return {
            transcripts: append(st.transcripts, pk, row),
            lastSeq: { ...st.lastSeq, [pk]: e.seq },
          };
        }
        case "error":
          // Transient infrastructure error (not a persisted transcript row; seq 0).
          return {
            transcripts: append(st.transcripts, e.session_pk, {
              seq: 0,
              role: "system",
              blockType: "error",
              text: e.message,
              toolCallId: null,
              toolStatus: null,
              toolKind: null,
              toolName: null,
              toolOutput: null,
              createdAt: Date.now(),
              attachments: [],
              toolPath: null,
            }),
          };
        case "approvalRequested":
          return {
            pendingApprovals: [
              ...st.pendingApprovals,
              { sessionPk: e.session_pk, requestId: e.request_id, tool: e.tool, summary: e.summary },
            ],
          };
        case "result":
          // Turn finished — the session is alive but awaiting input. Flip it out of "running"
          // so the composer leaves Stop mode and the user can reply.
          return { sessions: st.sessions.map((s) => (s.sessionPk === e.session_pk ? { ...s, status: "idle" as const } : s)) };
        case "sessionEnded":
          return { sessions: st.sessions.map((s) => (s.sessionPk === e.session_pk ? { ...s, status: "ended" as const } : s)) };
        default:
          return {};
      }
    }),

  clearApproval: (requestId) => set((st) => ({ pendingApprovals: st.pendingApprovals.filter((a) => a.requestId !== requestId) })),

  setFocused: (pk) => {
    set({ focusedSessionPk: pk });
    if (pk && !get().loaded[pk]) void get().hydrateTranscript(pk);
  },

  hydrateTranscript: async (pk, fetcher) => {
    if (get().loaded[pk]) return;
    const rows = fetcher
      ? await fetcher(pk)
      : await (async () => {
          const res = await commands.listMessages(pk);
          return res.status === "ok" ? res.data : [];
        })();
    const hydrated = rows.map((m) => messageToRow(m.seq, m.role, m.blockType, m.payload, m.toolCallId, m.status, m.toolKind, m.createdAt));
    const maxSeq = rows.reduce((mx, m) => Math.max(mx, m.seq), 0);
    set((st) => {
      // Rows appended by applyCoreEvent while listMessages was in flight (fresher
      // than the snapshot, or transient seq-0 error rows) must survive the replace.
      const liveTail = (st.transcripts[pk] ?? []).filter((r) => r.seq > maxSeq || r.seq === 0);
      return {
        transcripts: { ...st.transcripts, [pk]: [...hydrated, ...liveTail] },
        lastSeq: { ...st.lastSeq, [pk]: Math.max(st.lastSeq[pk] ?? 0, maxSeq) },
        loaded: { ...st.loaded, [pk]: true },
      };
    });
  },

  // Selecting a project clears the focused session so the center shows the "start a new session" composer.
  selectProject: (id) => set({ selectedProjectId: id, focusedSessionPk: null }),

  refresh: async () => {
    const projects = await commands.listProjects();
    const sessions = await commands.listSessions(null);
    if (projects.status === "ok") set({ projects: projects.data });
    if (sessions.status === "ok") set({ sessions: sessions.data });
  },

  addProject: async () => {
    const dir = await commands.pickDirectory();
    if (!dir) return;
    const name = basename(dir) || "project";
    const res = await commands.connectProject(dir, name);
    if (res.status === "ok") {
      await get().refresh();
    } else if (res.status === "error") {
      toast.error("Couldn't add project: " + res.error.message);
    }
  },

  setProjectModel: async (projectId, model) => {
    const project = get().projects.find((p) => p.projectId === projectId);
    if (!project) return;
    const next = model ?? null;
    if ((project.model ?? null) === next) return;
    // Optimistic paint so the composer label updates immediately.
    set({ projects: get().projects.map((p) => (p.projectId === projectId ? { ...p, model: next } : p)) });
    const res = await commands.updateProject(projectId, next, project.permMode, project.harness);
    if (res.status === "error") {
      toast.error("Couldn't set model: " + res.error.message);
      await get().refresh();
    }
  },
  setProjectPermMode: async (projectId, permMode) => {
    const project = get().projects.find((p) => p.projectId === projectId);
    if (!project || project.permMode === permMode) return;
    set({ projects: get().projects.map((p) => (p.projectId === projectId ? { ...p, permMode } : p)) });
    const res = await commands.updateProject(projectId, project.model, permMode, project.harness);
    if (res.status === "error") {
      toast.error("Couldn't set permission mode: " + res.error.message);
      await get().refresh();
    }
  },

  start: async (projectId, prompt, options) => {
    const res = await commands.startSession(projectId, prompt, toChatRequestOptions(options));
    if (res.status === "error") {
      toast.error("Couldn't start session: " + res.error.message);
      return false;
    }
    // Optimistic navigation: the backend returns the session row before its
    // git/harness startup finishes. Seed and focus it now; the full refresh
    // catches up in the background.
    set({ focusedSessionPk: res.data.sessionPk, sessions: [...get().sessions, res.data] });
    void get().refresh();
    return true;
  },
  send: async (sessionPk, prompt, options) => {
    const res = await commands.continueSession(sessionPk, prompt, toChatRequestOptions(options));
    if (res.status === "error") {
      toast.error("Couldn't send message: " + res.error.message);
    }
    await get().refresh();
  },
  stop: async (sessionPk) => {
    const res = await commands.stopSession(sessionPk);
    if (res.status === "error") {
      toast.error("Couldn't stop session: " + res.error.message);
    }
    await get().refresh();
  },
  end: async (sessionPk) => {
    const res = await commands.endSession(sessionPk);
    if (res.status === "error") {
      toast.error("Couldn't end session: " + res.error.message);
    }
    await get().refresh();
    return res.status === "ok";
  },
  resolveApproval: async (requestId, allow) => {
    try {
      await commands.resolveApproval(requestId, allow);
      get().clearApproval(requestId);
    } catch (e) {
      console.error("resolveApproval failed", e);
      toast.error("Approval failed: " + String(e));
    }
  },

  init: async () => {
    await get().refresh();
    await events.coreEventMsg.listen((e) => {
      const event = e.payload.event;
      get().applyCoreEvent(event);
      // Sessions can be created outside UI actions (e.g. scheduler runs) —
      // refresh the list so they appear in the sidebar immediately.
      if (event.kind === "sessionCreated") void get().refresh();
      else if (event.kind === "runtimeUpdateLog") useRuntimes.getState().onUpdateLog(event.runtime_id, event.line);
      else if (event.kind === "runtimeUpdateDone") useRuntimes.getState().onUpdateDone(event.runtime_id, event.ok, event.message);
    });
  },
}));
