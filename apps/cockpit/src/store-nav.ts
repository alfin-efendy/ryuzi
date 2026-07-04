import { create } from "zustand";

// Agent ids come from the engine's agent catalog (see store-runtimes).
export type AgentId = string;

// View router for the Relay v3 shell. Session focus itself stays in the main
// store (focusedSessionPk); this store only decides which screen is showing.
export type View =
  | { kind: "home" }
  | { kind: "session" }
  | { kind: "models" }
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
  | { kind: "settings" };

export type RightTab = "review" | "term" | "file";

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

type NavState = {
  history: NavHistory;
  sidebarOpen: boolean;
  bottomOpen: boolean;
  rightOpen: boolean;
  rightTab: RightTab;
  searchQuery: string;
  composerAgent: AgentId;
  composerBranch: string;
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
  setSearchQuery: (q: string) => void;
  setComposerAgent: (a: AgentId) => void;
  setComposerBranch: (b: string) => void;
  setProjectSettingsFor: (id: string | null) => void;
};

export const useNav = create<NavState>((set, get) => ({
  history: { back: [], current: { kind: "home" }, forward: [] },
  sidebarOpen: readBool(KEY_SIDEBAR, true),
  bottomOpen: false,
  rightOpen: false,
  rightTab: "review",
  searchQuery: "",
  composerAgent: "claude",
  composerBranch: "main",
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
  toggleBottom: () => set((s) => ({ bottomOpen: !s.bottomOpen })),
  toggleRight: () => set((s) => ({ rightOpen: !s.rightOpen })),
  setRightOpen: (open) => set({ rightOpen: open }),
  setRightTab: (tab) => set({ rightTab: tab }),
  setSearchQuery: (q) => set({ searchQuery: q }),
  setComposerAgent: (a) => set({ composerAgent: a }),
  setComposerBranch: (b) => set({ composerBranch: b }),
  setProjectSettingsFor: (id) => set({ projectSettingsFor: id }),
}));
