import { create } from "zustand";
import { toast } from "sonner";
import { createTerm, getTerm, setOnExit, setCopyOnSelect as cacheSetCopyOnSelect } from "./lib/term-cache";

const KEY_COPY = "cockpit.term.copyOnSelect";

function readCopyOnSelect(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(KEY_COPY) === "1";
}

export type TermTab = { termId: string; title: string; exited: boolean };

export function nextTitle(tabs: TermTab[]): string {
  const used = tabs.map((t) => Number(/^Terminal (\d+)$/.exec(t.title)?.[1] ?? 0));
  return `Terminal ${Math.max(0, ...used) + 1}`;
}

export function closeTermTab(tabs: TermTab[], active: string | null, termId: string): { tabs: TermTab[]; active: string | null } {
  const idx = tabs.findIndex((t) => t.termId === termId);
  if (idx === -1) return { tabs, active };
  const next = tabs.filter((t) => t.termId !== termId);
  if (active !== termId) return { tabs: next, active };
  if (next.length === 0) return { tabs: next, active: null };
  return { tabs: next, active: next[Math.min(idx, next.length - 1)].termId };
}

export function markExited(tabs: TermTab[], termId: string): TermTab[] {
  return tabs.map((t) => (t.termId === termId ? { ...t, exited: true } : t));
}

type TermsState = {
  tabs: Record<string, TermTab[]>;
  active: Record<string, string>;
  open: (sessionPk: string) => Promise<void>;
  /** Spawn Terminal 1 iff the session has none. Module-level in-flight guard
   *  makes this safe under React StrictMode's dev double-mount. */
  ensureOne: (sessionPk: string) => Promise<void>;
  close: (sessionPk: string, termId: string) => void;
  setActive: (sessionPk: string, termId: string) => void;
  /** Archive teardown: drop every cached terminal for a session. The backend
   *  term_close_session already killed the PTYs; this clears the JS side. */
  disposeSession: (sessionPk: string) => void;
  copyOnSelect: boolean;
  setCopyOnSelect: (v: boolean) => void;
};

const inflight = new Set<string>();

export const useTerms = create<TermsState>((set, get) => {
  setOnExit((termId) => {
    set((s) => {
      const tabs: Record<string, TermTab[]> = {};
      for (const [pk, list] of Object.entries(s.tabs)) tabs[pk] = markExited(list, termId);
      return { tabs };
    });
  });

  return {
    tabs: {},
    active: {},

    open: async (sessionPk) => {
      const inst = await createTerm(sessionPk);
      if ("error" in inst) {
        toast.error(`Terminal failed to open: ${inst.error}`);
        return;
      }
      set((s) => {
        const list = s.tabs[sessionPk] ?? [];
        return {
          tabs: { ...s.tabs, [sessionPk]: [...list, { termId: inst.termId, title: nextTitle(list), exited: false }] },
          active: { ...s.active, [sessionPk]: inst.termId },
        };
      });
    },

    ensureOne: async (sessionPk) => {
      if (inflight.has(sessionPk) || (get().tabs[sessionPk] ?? []).length > 0) return;
      inflight.add(sessionPk);
      try {
        await get().open(sessionPk);
      } finally {
        inflight.delete(sessionPk);
      }
    },

    close: (sessionPk, termId) => {
      getTerm(termId)?.dispose();
      set((s) => {
        const r = closeTermTab(s.tabs[sessionPk] ?? [], s.active[sessionPk] ?? null, termId);
        const active = { ...s.active };
        if (r.active === null) delete active[sessionPk];
        else active[sessionPk] = r.active;
        return { tabs: { ...s.tabs, [sessionPk]: r.tabs }, active };
      });
    },

    setActive: (sessionPk, termId) => set((s) => ({ active: { ...s.active, [sessionPk]: termId } })),

    disposeSession: (sessionPk) => {
      for (const t of get().tabs[sessionPk] ?? []) getTerm(t.termId)?.dispose();
      set((s) => {
        const tabs = { ...s.tabs };
        const active = { ...s.active };
        delete tabs[sessionPk];
        delete active[sessionPk];
        return { tabs, active };
      });
    },

    copyOnSelect: (() => {
      const v = readCopyOnSelect();
      cacheSetCopyOnSelect(v);
      return v;
    })(),
    setCopyOnSelect: (v: boolean) => {
      if (typeof localStorage !== "undefined") localStorage.setItem(KEY_COPY, v ? "1" : "0");
      cacheSetCopyOnSelect(v);
      set({ copyOnSelect: v });
    },
  };
});
