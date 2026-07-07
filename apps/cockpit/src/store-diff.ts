import { create } from "zustand";
import { commands } from "@/bindings";
import { parseUnifiedDiff, type ReviewFile } from "@/lib/diff";

// Per-session git diff, shared by the Review tab and the transcript's
// file-edit cards so both render from ONE fetch.

export type SessionDiff = { files: ReviewFile[]; loading: boolean; error: string | null };

export const EMPTY: SessionDiff = { files: [], loading: false, error: null };

export type PendingReview = { sessionPk: string; path: string };

type DiffState = {
  bySession: Record<string, SessionDiff>;
  /** File the Review tab should select on next render (set by edit cards).
   *  Scoped to a session so a jump queued in one session never selects a
   *  same-suffix file in another. */
  pendingReview: PendingReview | null;
  fetch: (sessionPk: string) => Promise<void>;
  setPendingReview: (pending: PendingReview | null) => void;
};

// Per-session fetch counter: concurrent fetches would otherwise race
// last-resolve-wins, so only the newest call may write its result back.
const fetchGeneration = new Map<string, number>();

export const useDiff = create<DiffState>((set) => ({
  bySession: {},
  pendingReview: null,
  fetch: async (sessionPk) => {
    const gen = (fetchGeneration.get(sessionPk) ?? 0) + 1;
    fetchGeneration.set(sessionPk, gen);
    set((s) => ({ bySession: { ...s.bySession, [sessionPk]: { ...(s.bySession[sessionPk] ?? EMPTY), loading: true } } }));
    const res = await commands.gitDiff(sessionPk);
    if (fetchGeneration.get(sessionPk) !== gen) return; // superseded — the newer call owns the state
    set((s) => ({
      bySession: {
        ...s.bySession,
        [sessionPk]:
          res.status === "ok"
            ? { files: parseUnifiedDiff(res.data), loading: false, error: null }
            : { files: s.bySession[sessionPk]?.files ?? [], loading: false, error: res.error.message },
      },
    }));
  },
  setPendingReview: (pending) => set({ pendingReview: pending }),
}));

/** Index of `path` in the review list — matches repo-relative paths and
 *  absolute paths from either OS by suffix. -1 when absent. */
export function reviewFileIndex(files: ReviewFile[], path: string): number {
  const norm = path.replace(/\\/g, "/");
  return files.findIndex((f) => {
    const rel = `${f.dir}${f.name}`;
    return norm === rel || norm.endsWith(`/${rel}`);
  });
}
