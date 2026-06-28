// apps/ide/src/renderer/store.ts
import { create } from "zustand";
import type { Project, Session, CoreEvent, ApprovalRequestFrame } from "@harness/protocol";
import type { ConnState } from "../shared/ipc-contract";

interface CockpitState {
  connection: ConnState;
  connId: string | null;
  projects: Project[];
  sessions: Session[];
  activeSessionPk: string | null;
  transcripts: Record<string, CoreEvent[]>;
  pendingApprovals: ApprovalRequestFrame[];
  setConnection: (s: ConnState) => void;
  setConnId: (id: string | null) => void;
  setProjects: (p: Project[]) => void;
  setSessions: (s: Session[]) => void;
  setActive: (pk: string | null) => void;
  applyEvent: (e: CoreEvent) => void;
  addApproval: (r: ApprovalRequestFrame) => void;
  removeApproval: (requestId: string) => void;
  clearApprovals: () => void;
}

export const useStore = create<CockpitState>((set) => ({
  connection: "closed",
  connId: null,
  projects: [],
  sessions: [],
  activeSessionPk: null,
  transcripts: {},
  pendingApprovals: [],
  setConnection: (s) => set({ connection: s }),
  setConnId: (id) => set({ connId: id }),
  setProjects: (p) => set({ projects: p }),
  setSessions: (s) => set({ sessions: s }),
  setActive: (pk) => set({ activeSessionPk: pk }),
  applyEvent: (e) =>
    set((st) => {
      const prev = st.transcripts[e.sessionPk] ?? [];
      const transcripts = { ...st.transcripts, [e.sessionPk]: [...prev, e] };
      let sessions = st.sessions;
      if (e.kind === "session.ended") {
        sessions = st.sessions.map((s) => (s.sessionPk === e.sessionPk ? { ...s, status: "ended" } : s));
      }
      return { transcripts, sessions };
    }),
  addApproval: (r) => set((st) => ({ pendingApprovals: [...st.pendingApprovals, r] })),
  removeApproval: (requestId) =>
    set((st) => ({
      pendingApprovals: st.pendingApprovals.filter((a) => a.requestId !== requestId),
    })),
  clearApprovals: () => set({ pendingApprovals: [] }),
}));
