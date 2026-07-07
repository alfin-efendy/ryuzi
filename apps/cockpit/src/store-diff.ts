import { create } from "zustand";
import { commands } from "@/bindings";
import { parseUnifiedDiff, type ReviewFile } from "@/lib/diff";

// Per-session git diff, shared by the Review tab and the transcript's
// file-edit cards so both render from ONE fetch.

export type SessionDiff = { files: ReviewFile[]; loading: boolean; error: string | null };

const EMPTY: SessionDiff = { files: [], loading: false, error: null };

type DiffState = {
  bySession: Record<string, SessionDiff>;
  /** File the Review tab should select on next render (set by edit cards). */
  pendingReviewPath: string | null;
  fetch: (sessionPk: string) => Promise<void>;
  setPendingReviewPath: (path: string | null) => void;
};

export const useDiff = create<DiffState>((set) => ({
  bySession: {},
  pendingReviewPath: null,
  fetch: async (sessionPk) => {
    set((s) => ({ bySession: { ...s.bySession, [sessionPk]: { ...(s.bySession[sessionPk] ?? EMPTY), loading: true } } }));
    const res = await commands.gitDiff(sessionPk);
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
  setPendingReviewPath: (path) => set({ pendingReviewPath: path }),
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
