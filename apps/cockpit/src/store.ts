import { create } from "zustand";
import { commands, events, type Project, type Session, type CoreEvent } from "./bindings";

export type Line = { kind: "text" | "status" | "error"; text: string };
export type PendingApproval = { sessionPk: string; requestId: string; tool: string; summary: string };

type State = {
  projects: Project[];
  sessions: Session[];
  transcripts: Record<string, Line[]>;
  pendingApprovals: PendingApproval[];
  focusedSessionPk: string | null;
  selectedProjectId: string | null;
  applyCoreEvent: (e: CoreEvent) => void;
  clearApproval: (requestId: string) => void;
  setFocused: (pk: string | null) => void;
  selectProject: (id: string | null) => void;
  refresh: () => Promise<void>;
  addProject: () => Promise<void>;
  start: (projectId: string, prompt: string) => Promise<void>;
  send: (sessionPk: string, prompt: string) => Promise<void>;
  stop: (sessionPk: string) => Promise<void>;
  end: (sessionPk: string) => Promise<void>;
  resolveApproval: (requestId: string, allow: boolean) => Promise<void>;
  init: () => Promise<void>;
};

function append(map: Record<string, Line[]>, pk: string, line: Line): Record<string, Line[]> {
  return { ...map, [pk]: [...(map[pk] ?? []), line] };
}

export const useStore = create<State>((set, get) => ({
  projects: [],
  sessions: [],
  transcripts: {},
  pendingApprovals: [],
  focusedSessionPk: null,
  selectedProjectId: null,

  applyCoreEvent: (e) =>
    set((st) => {
      switch (e.kind) {
        case "sessionCreated":
          return { transcripts: { ...st.transcripts, [e.session_pk]: st.transcripts[e.session_pk] ?? [] } };
        case "status":
          return { transcripts: append(st.transcripts, e.session_pk, { kind: "status", text: e.text }) };
        case "text":
          return { transcripts: append(st.transcripts, e.session_pk, { kind: "text", text: e.text }) };
        case "error":
          return { transcripts: append(st.transcripts, e.session_pk, { kind: "error", text: e.message }) };
        case "approvalRequested":
          return {
            pendingApprovals: [
              ...st.pendingApprovals,
              { sessionPk: e.session_pk, requestId: e.request_id, tool: e.tool, summary: e.summary },
            ],
          };
        case "result":
        case "sessionEnded":
        default:
          return {};
      }
    }),

  clearApproval: (requestId) =>
    set((st) => ({ pendingApprovals: st.pendingApprovals.filter((a) => a.requestId !== requestId) })),

  setFocused: (pk) => set({ focusedSessionPk: pk }),
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
    const name = dir.split("/").filter(Boolean).pop() ?? "project";
    const res = await commands.connectProject(dir, name);
    if (res.status === "ok") await get().refresh();
  },

  start: async (projectId, prompt) => {
    const res = await commands.startSession(projectId, prompt);
    if (res.status === "ok") {
      set({ focusedSessionPk: res.data.sessionPk });
      await get().refresh();
    }
  },
  send: async (sessionPk, prompt) => {
    await commands.continueSession(sessionPk, prompt);
    await get().refresh();
  },
  stop: async (sessionPk) => {
    await commands.stopSession(sessionPk);
    await get().refresh();
  },
  end: async (sessionPk) => {
    await commands.endSession(sessionPk);
    await get().refresh();
  },
  resolveApproval: async (requestId, allow) => {
    try {
      await commands.resolveApproval(requestId, allow);
      get().clearApproval(requestId);
    } catch (e) { console.error("resolveApproval failed", e); }
  },

  init: async () => {
    await get().refresh();
    await events.coreEventMsg.listen((e) => get().applyCoreEvent(e.payload.event));
  },
}));
