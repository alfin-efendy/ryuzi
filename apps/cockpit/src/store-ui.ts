import { create } from "zustand";

export type DockTab = { id: string; kind: "file"; path: string; title: string };

function titleOf(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

export function openFileTab(tabs: DockTab[], path: string): { tabs: DockTab[]; activeTabId: string } {
  if (tabs.some((t) => t.id === path)) return { tabs, activeTabId: path };
  const tab: DockTab = { id: path, kind: "file", path, title: titleOf(path) };
  return { tabs: [...tabs, tab], activeTabId: path };
}

export function closeTab(
  tabs: DockTab[],
  activeTabId: string | null,
  id: string,
): { tabs: DockTab[]; activeTabId: string | null } {
  const idx = tabs.findIndex((t) => t.id === id);
  if (idx === -1) return { tabs, activeTabId };
  const next = tabs.filter((t) => t.id !== id);
  if (activeTabId !== id) return { tabs: next, activeTabId };
  if (next.length === 0) return { tabs: next, activeTabId: null };
  const neighbor = next[Math.min(idx, next.length - 1)];
  return { tabs: next, activeTabId: neighbor.id };
}

const KEY = { left: "cockpit.ui.leftOpen", right: "cockpit.ui.rightOpen", tabs: "cockpit.ui.tabs", active: "cockpit.ui.activeTab" };

function readBool(key: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  const v = localStorage.getItem(key);
  return v === null ? fallback : v === "1";
}
function readTabs(): DockTab[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(KEY.tabs);
    return raw ? (JSON.parse(raw) as DockTab[]) : [];
  } catch {
    return [];
  }
}
function persist(key: string, value: string): void {
  if (typeof localStorage !== "undefined") localStorage.setItem(key, value);
}
export function normalizeActive(raw: string | null): string | null {
  return raw && raw.length > 0 ? raw : null;
}

type UiState = {
  leftPanelOpen: boolean;
  rightPanelOpen: boolean;
  tabs: DockTab[];
  activeTabId: string | null;
  toggleLeft: () => void;
  toggleRight: () => void;
  openFile: (path: string) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
};

export const useUi = create<UiState>((set, get) => ({
  leftPanelOpen: readBool(KEY.left, true),
  rightPanelOpen: readBool(KEY.right, true),
  tabs: readTabs(),
  activeTabId: normalizeActive(typeof localStorage !== "undefined" ? localStorage.getItem(KEY.active) : null),
  toggleLeft: () => set((s) => { const v = !s.leftPanelOpen; persist(KEY.left, v ? "1" : "0"); return { leftPanelOpen: v }; }),
  toggleRight: () => set((s) => { const v = !s.rightPanelOpen; persist(KEY.right, v ? "1" : "0"); return { rightPanelOpen: v }; }),
  openFile: (path) => {
    const r = openFileTab(get().tabs, path);
    persist(KEY.tabs, JSON.stringify(r.tabs));
    persist(KEY.active, r.activeTabId);
    set(r);
  },
  closeTab: (id) => {
    const r = closeTab(get().tabs, get().activeTabId, id);
    persist(KEY.tabs, JSON.stringify(r.tabs));
    if (r.activeTabId === null) {
      if (typeof localStorage !== "undefined") localStorage.removeItem(KEY.active);
    } else {
      persist(KEY.active, r.activeTabId);
    }
    set(r);
  },
  setActiveTab: (id) => { persist(KEY.active, id); set({ activeTabId: id }); },
}));
