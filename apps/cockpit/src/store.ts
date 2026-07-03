import { create } from "zustand";
import { toast } from "sonner";
import { commands, events, type Project, type Session, type CoreEvent, type Message } from "./bindings";
import { basename } from "./lib/paths";

export type Line = { kind: "user" | "text" | "status" | "error"; text: string };
export type PendingApproval = { sessionPk: string; requestId: string; tool: string; summary: string };

type State = {
  projects: Project[];
  sessions: Session[];
  transcripts: Record<string, Line[]>;
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
  start: (projectId: string, prompt: string) => Promise<void>;
  send: (sessionPk: string, prompt: string) => Promise<void>;
  stop: (sessionPk: string) => Promise<void>;
  /** Resolves true only when the backend teardown actually succeeded. */
  end: (sessionPk: string) => Promise<boolean>;
  resolveApproval: (requestId: string, allow: boolean) => Promise<void>;
  hydrateTranscript: (pk: string, fetcher?: (pk: string) => Promise<Message[]>) => Promise<void>;
  init: () => Promise<void>;
};

function append(map: Record<string, Line[]>, pk: string, line: Line): Record<string, Line[]> {
  return { ...map, [pk]: [...(map[pk] ?? []), line] };
}

// Projects a persisted/streamed message block onto the render Line shape.
function messageToLine(role: string, blockType: string, payload: unknown): Line {
  const p = (payload ?? {}) as Record<string, unknown>;
  if (role === "user") return { kind: "user", text: String(p.text ?? "") };
  if (blockType === "status" || blockType === "tool_call") return { kind: "status", text: String(p.summary ?? p.name ?? "") };
  if (blockType === "error") return { kind: "error", text: String(p.message ?? "") };
  return { kind: "text", text: String(p.text ?? "") };
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
          const prev = st.lastSeq[e.session_pk] ?? 0;
          if (e.seq <= prev) return {}; // stale/duplicate (covers reload/replay races)
          const line = messageToLine(e.role, e.block_type, e.payload);
          return {
            transcripts: append(st.transcripts, e.session_pk, line),
            lastSeq: { ...st.lastSeq, [e.session_pk]: e.seq },
          };
        }
        case "error":
          // Transient infrastructure error (not a persisted transcript row).
          return { transcripts: append(st.transcripts, e.session_pk, { kind: "error", text: e.message }) };
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
    const lines = rows.map((m) => messageToLine(m.role, m.blockType, m.payload));
    const maxSeq = rows.reduce((mx, m) => Math.max(mx, m.seq), 0);
    set((st) => ({
      transcripts: { ...st.transcripts, [pk]: lines },
      lastSeq: { ...st.lastSeq, [pk]: maxSeq },
      loaded: { ...st.loaded, [pk]: true },
    }));
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

  start: async (projectId, prompt) => {
    const res = await commands.startSession(projectId, prompt);
    if (res.status === "ok") {
      const pk = res.data.sessionPk;
      set({ focusedSessionPk: pk });
      await get().refresh();
    } else if (res.status === "error") {
      toast.error("Couldn't start session: " + res.error.message);
    }
  },
  send: async (sessionPk, prompt) => {
    const res = await commands.continueSession(sessionPk, prompt);
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
      get().applyCoreEvent(e.payload.event);
      // Sessions can be created outside UI actions (e.g. scheduler runs) —
      // refresh the list so they appear in the sidebar immediately.
      if (e.payload.event.kind === "sessionCreated") void get().refresh();
    });
  },
}));
