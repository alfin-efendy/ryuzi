import { create } from "zustand";

// Agent ids come from the engine's agent catalog (see store-runtimes).
export type AgentId = string;

// View router for the Relay v3 shell. Session focus itself stays in the main
// store (focusedSessionPk); this store only decides which screen is showing.
export type View =
  | { kind: "home" }
  | { kind: "session" }
  | { kind: "models" }
  | { kind: "providerDetail"; provider: string }
  | { kind: "connectionDetail"; id: string }
  | { kind: "runtime" }
  | { kind: "runtimeDetail"; id: AgentId }
  | { kind: "scheduler" }
  | { kind: "jobDetail"; id: string }
  | { kind: "jobNew" }
  | { kind: "apps" }
  | { kind: "appDetail"; id: string }
  | { kind: "registry" }
  | { kind: "gateways" }
  | { kind: "gatewayDetail"; id: string }
  | { kind: "pluginDetail"; id: string }
  | { kind: "settings" };

export type RightTab = "review" | "file";

export type NavHistory = { back: View[]; current: View; forward: View[] };

export function navigateHistory(h: NavHistory, view: View): NavHistory {
  if (JSON.stringify(view) === JSON.stringify(h.current)) return h;
  return { back: [...h.back, h.current], current: view, forward: [] };
}

export function goBackHistory(h: NavHistory): NavHistory {
  const prev = h.back[h.back.length - 1];
  if (!prev) return h;
  return { back: h.back.slice(0, -1), current: prev, forward: [h.current, ...h.forward] };
}

export function goForwardHistory(h: NavHistory): NavHistory {
  const next = h.forward[0];
  if (!next) return h;
  return { back: [...h.back, h.current], current: next, forward: h.forward.slice(1) };
}

const KEY_SIDEBAR = "cockpit.nav.sidebarOpen";

function readBool(key: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  const v = localStorage.getItem(key);
  return v === null ? fallback : v === "1";
}

const KEY_RIGHT_OPEN = "cockpit.nav.rightOpen";
const KEY_RIGHT_TAB = "cockpit.nav.rightTab";
const KEY_RIGHT_WIDTH = "cockpit.nav.rightWidth";
const KEY_BOTTOM_OPEN = "cockpit.nav.bottomOpen";
const KEY_BOTTOM_HEIGHT = "cockpit.nav.bottomHeight";

export const RIGHT_WIDTH = { min: 320, def: 560, maxFrac: 0.8 };
export const BOTTOM_HEIGHT = { min: 120, def: 240, maxFrac: 0.6 };

/** Clamp a panel size to [min, viewport*maxFrac]. */
export function clampPanelSize(px: number, viewport: number, b: { min: number; maxFrac: number }): number {
  const max = Math.max(b.min, Math.round(viewport * b.maxFrac));
  return Math.min(Math.max(Math.round(px), b.min), max);
}

/** Legacy persisted values ("term") and garbage collapse to "review". */
export function sanitizeRightTab(raw: string | null): RightTab {
  return raw === "file" ? "file" : "review";
}

/** Parse a persisted panel size and clamp it to the current viewport. A size
 *  saved on a large monitor must not exceed a smaller window on the next launch,
 *  so we clamp on read (not just on resize). Missing/invalid values fall back to
 *  the default (itself clamped, though the default always fits). Pure — the
 *  caller supplies the raw string and viewport so it stays unit-testable. */
export function readClampedPanelSize(raw: string | null, viewport: number, b: { min: number; def: number; maxFrac: number }): number {
  const n = Number(raw);
  const px = raw !== null && Number.isFinite(n) && n > 0 ? n : b.def;
  return clampPanelSize(px, viewport, b);
}

function readStored(key: string): string | null {
  return typeof localStorage !== "undefined" ? localStorage.getItem(key) : null;
}

function persist(key: string, value: string): void {
  if (typeof localStorage !== "undefined") localStorage.setItem(key, value);
}

type NavState = {
  history: NavHistory;
  sidebarOpen: boolean;
  bottomOpen: boolean;
  rightOpen: boolean;
  rightTab: RightTab;
  rightWidth: number;
  bottomHeight: number;
  rightMaximized: boolean;
  searchQuery: string;
  /** null until the selected project's branch list loads, then its current branch. */
  composerBranch: string | null;
  /** Run the session in an isolated git worktree (matrix column 1). */
  composerUseWorktree: boolean;
  /** Create a new branch for the session (matrix column 2). */
  composerCreateBranch: boolean;
  /** Model the next composed session should run on; null = project/runtime default. */
  composerModel: string | null;
  projectSettingsFor: string | null;
  view: () => View;
  navigate: (view: View) => void;
  goBack: () => void;
  goForward: () => void;
  toggleSidebar: () => void;
  toggleBottom: () => void;
  toggleRight: () => void;
  setRightOpen: (open: boolean) => void;
  setRightTab: (tab: RightTab) => void;
  setRightWidth: (px: number) => void;
  setBottomHeight: (px: number) => void;
  setRightMaximized: (v: boolean) => void;
  setSearchQuery: (q: string) => void;
  setComposerBranch: (b: string | null) => void;
  setComposerUseWorktree: (v: boolean) => void;
  setComposerCreateBranch: (v: boolean) => void;
  setComposerModel: (model: string | null) => void;
  setProjectSettingsFor: (id: string | null) => void;
};

export const useNav = create<NavState>((set, get) => ({
  history: { back: [], current: { kind: "home" }, forward: [] },
  sidebarOpen: readBool(KEY_SIDEBAR, true),
  bottomOpen: readBool(KEY_BOTTOM_OPEN, false),
  rightOpen: readBool(KEY_RIGHT_OPEN, false),
  rightTab: sanitizeRightTab(typeof localStorage !== "undefined" ? localStorage.getItem(KEY_RIGHT_TAB) : null),
  rightWidth: readClampedPanelSize(readStored(KEY_RIGHT_WIDTH), typeof window !== "undefined" ? window.innerWidth : 1920, RIGHT_WIDTH),
  bottomHeight: readClampedPanelSize(
    readStored(KEY_BOTTOM_HEIGHT),
    typeof window !== "undefined" ? window.innerHeight : 1080,
    BOTTOM_HEIGHT,
  ),
  rightMaximized: false,
  searchQuery: "",
  composerBranch: null,
  composerUseWorktree: true,
  composerCreateBranch: true,
  composerModel: null,
  projectSettingsFor: null,

  view: () => get().history.current,
  navigate: (view) => set((s) => ({ history: navigateHistory(s.history, view) })),
  goBack: () => set((s) => ({ history: goBackHistory(s.history) })),
  goForward: () => set((s) => ({ history: goForwardHistory(s.history) })),

  toggleSidebar: () =>
    set((s) => {
      const v = !s.sidebarOpen;
      if (typeof localStorage !== "undefined") localStorage.setItem(KEY_SIDEBAR, v ? "1" : "0");
      return { sidebarOpen: v };
    }),
  toggleBottom: () =>
    set((s) => {
      const v = !s.bottomOpen;
      persist(KEY_BOTTOM_OPEN, v ? "1" : "0");
      return { bottomOpen: v };
    }),
  toggleRight: () =>
    set((s) => {
      const v = !s.rightOpen;
      persist(KEY_RIGHT_OPEN, v ? "1" : "0");
      return { rightOpen: v, rightMaximized: v ? s.rightMaximized : false };
    }),
  setRightOpen: (open) => {
    persist(KEY_RIGHT_OPEN, open ? "1" : "0");
    set((s) => ({ rightOpen: open, rightMaximized: open ? s.rightMaximized : false }));
  },
  setRightTab: (tab) => {
    persist(KEY_RIGHT_TAB, tab);
    set({ rightTab: tab });
  },
  setRightWidth: (px) => {
    persist(KEY_RIGHT_WIDTH, String(px));
    set({ rightWidth: px });
  },
  setBottomHeight: (px) => {
    persist(KEY_BOTTOM_HEIGHT, String(px));
    set({ bottomHeight: px });
  },
  setRightMaximized: (v) => set({ rightMaximized: v }),
  setSearchQuery: (q) => set({ searchQuery: q }),
  setComposerBranch: (b) => set({ composerBranch: b }),
  setComposerUseWorktree: (v) => set({ composerUseWorktree: v }),
  setComposerCreateBranch: (v) => set({ composerCreateBranch: v }),
  setComposerModel: (model) => set({ composerModel: model }),
  setProjectSettingsFor: (id) => set({ projectSettingsFor: id }),
}));
