import type { AgentSummaryInfo } from "./bindings";
import { create } from "zustand";

// View router for the Relay v3 shell. Session focus itself stays in the main
// store (focusedSessionPk); this store only decides which screen is showing.
export type View =
  | { kind: "home" }
  | { kind: "inbox" }
  | { kind: "session" }
  | { kind: "models" }
  | { kind: "providerDetail"; provider: string }
  | { kind: "scheduler" }
  | { kind: "automations"; tab?: "scheduler" | "hooks" | "commands" }
  | { kind: "jobDetail"; id: string }
  | { kind: "jobNew" }
  | { kind: "plugins" }
  | { kind: "appDetail"; id: string }
  | { kind: "gateways" }
  | { kind: "gatewayDetail"; id: string }
  | { kind: "pluginDetail"; id: string }
  | { kind: "settings" }
  | { kind: "agents" }
  | { kind: "agentDetail"; agentId: string };

export type RightTab = "review" | "file" | "agents";

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

export const LAST_PRIMARY_AGENT_KEY = "cockpit.lastPrimaryAgentId";

export function choosePrimaryAgent(
  agents: AgentSummaryInfo[],
  requestedId: string | null,
  lastId: string | null,
  defaultId: string | null,
): string | null {
  const valid = (id: string | null) => id !== null && agents.some((agent) => agent.id === id && agent.executable);
  return [requestedId, lastId, defaultId].find(valid) ?? agents.find((agent) => agent.executable)?.id ?? null;
}

function readBool(key: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  const v = localStorage.getItem(key);
  return v === null ? fallback : v === "1";
}

const KEY_SIDEBAR = "cockpit.nav.sidebarOpen";
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
  return raw === "file" ? "file" : raw === "agents" ? "agents" : "review";
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

const KEY_DRAFTS = "cockpit.composer.drafts";

/** Parse the persisted composer-drafts map. Malformed JSON, non-object shapes,
 *  and non-string/empty entries collapse away — a corrupt localStorage value
 *  must never take the composer down. Pure so it is directly testable. */
export function readDrafts(raw: string | null): Record<string, string> {
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "string" && v !== "") out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

/** Set/replace one draft. Empty text deletes the key (identity-preserving no-op
 *  when absent) so the persisted map only holds composers with real unsent text. */
export function upsertDraft(drafts: Record<string, string>, key: string, text: string): Record<string, string> {
  if (text === "") {
    if (!(key in drafts)) return drafts;
    return Object.fromEntries(Object.entries(drafts).filter(([k]) => k !== key));
  }
  return { ...drafts, [key]: text };
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
  /** Unsent composer text keyed by composer identity: a sessionPk (SessionView)
   *  or `home:{projectId}` (HomeView). Persisted so drafts survive restarts. */
  drafts: Record<string, string>;
  projectSettingsFor: string | null;
  /** Agent id recorded by openAgentChat for the next New-session composer;
   *  Plan 4's HomeView consumes it via consumePendingPrimaryAgentId. */
  pendingPrimaryAgentId: string | null;
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
  setDraft: (key: string, text: string) => void;
  clearDraft: (key: string) => void;
  /** Refill a draft after a failed send — no-op if the user already typed anew. */
  restoreDraft: (key: string, text: string) => void;
  setProjectSettingsFor: (id: string | null) => void;
  /** "Start chat" from the agent hub/detail: record the agent and open Home. */
  openAgentChat: (agentId: string) => void;
  /** Return the pending primary agent id and clear it (one-shot handoff). */
  consumePendingPrimaryAgentId: () => string | null;
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
  drafts: readDrafts(readStored(KEY_DRAFTS)),
  projectSettingsFor: null,
  pendingPrimaryAgentId: null,

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
  setDraft: (key, text) =>
    set((s) => {
      const drafts = upsertDraft(s.drafts, key, text);
      if (drafts !== s.drafts) persist(KEY_DRAFTS, JSON.stringify(drafts));
      return { drafts };
    }),
  clearDraft: (key) => get().setDraft(key, ""),
  restoreDraft: (key, text) => {
    if ((get().drafts[key] ?? "") === "") get().setDraft(key, text);
  },
  setProjectSettingsFor: (id) => set({ projectSettingsFor: id }),
  openAgentChat: (agentId) =>
    set((s) => ({
      pendingPrimaryAgentId: agentId,
      history: navigateHistory(s.history, { kind: "home" }),
    })),
  consumePendingPrimaryAgentId: () => {
    const id = get().pendingPrimaryAgentId;
    if (id !== null) set({ pendingPrimaryAgentId: null });
    return id;
  },
}));
