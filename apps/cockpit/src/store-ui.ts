import { create } from "zustand";
import type { Session } from "./bindings";
import { basename } from "./lib/paths";
import type { ViewMode } from "./lib/preview";

export type DockTab = { id: string; kind: "file"; path: string; title: string; mode?: ViewMode };

function titleOf(path: string): string {
  return basename(path) || path;
}

export function openFileTab(tabs: DockTab[], path: string): { tabs: DockTab[]; activeTabId: string } {
  if (tabs.some((t) => t.id === path)) return { tabs, activeTabId: path };
  const tab: DockTab = { id: path, kind: "file", path, title: titleOf(path) };
  return { tabs: [...tabs, tab], activeTabId: path };
}

export function closeTab(tabs: DockTab[], activeTabId: string | null, id: string): { tabs: DockTab[]; activeTabId: string | null } {
  const idx = tabs.findIndex((t) => t.id === id);
  if (idx === -1) return { tabs, activeTabId };
  const next = tabs.filter((t) => t.id !== id);
  if (activeTabId !== id) return { tabs: next, activeTabId };
  if (next.length === 0) return { tabs: next, activeTabId: null };
  const neighbor = next[Math.min(idx, next.length - 1)];
  return { tabs: next, activeTabId: neighbor.id };
}

export function setTabMode(tabs: DockTab[], id: string, mode: ViewMode): DockTab[] {
  return tabs.map((t) => (t.id === id ? { ...t, mode } : t));
}

const KEY = {
  left: "cockpit.ui.leftOpen",
  right: "cockpit.ui.rightOpen",
  tabs: "cockpit.ui.tabs",
  active: "cockpit.ui.activeTab",
  pinned: "cockpit.ui.pinned",
  archived: "cockpit.ui.archived",
  hideInvalidModels: "cockpit.ui.hideInvalidModels",
  notificationsEnabled: "cockpit.ui.notificationsEnabled",
  readAt: "cockpit.ui.readAt",
  sessionFilter: "cockpit.ui.sessionFilter",
};

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
function readSet(key: string): Record<string, true> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, true>) : {};
  } catch {
    return {};
  }
}
function readNumMap(key: string): Record<string, number> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, number>) : {};
  } catch {
    return {};
  }
}
function readSessionFilter(key: string): { statuses: Record<string, true>; unreadOnly: boolean } {
  if (typeof localStorage === "undefined") return { statuses: {}, unreadOnly: false };
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return { statuses: {}, unreadOnly: false };
    const v = JSON.parse(raw) as { statuses?: Record<string, true>; unreadOnly?: boolean };
    return { statuses: v.statuses ?? {}, unreadOnly: v.unreadOnly ?? false };
  } catch {
    return { statuses: {}, unreadOnly: false };
  }
}
export function toggleKey(map: Record<string, true>, key: string): Record<string, true> {
  const next = { ...map };
  if (next[key]) delete next[key];
  else next[key] = true;
  return next;
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
  pinned: Record<string, true>;
  archived: Record<string, true>;
  /** Per-session last-read epoch-ms cursor (unread = lastActive > cursor). */
  readAt: Record<string, number>;
  /** Sidebar session filters: status checkboxes (empty = all) + unread-only toggle. */
  sessionFilter: { statuses: Record<string, true>; unreadOnly: boolean };
  /** Hide models with a persisted "invalid" verdict app-wide: the Provider
   *  Models card rows AND every model picker (composers, route targets,
   *  runtime config). A picker's current selection always stays visible,
   *  flagged as invalid. */
  hideInvalidModels: boolean;
  /** Enables OS-level notifications (dock badge, native alerts) for session events. */
  notificationsEnabled: boolean;
  toggleLeft: () => void;
  toggleRight: () => void;
  setLeft: (open: boolean) => void;
  setRight: (open: boolean) => void;
  openFile: (path: string) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  setTabMode: (id: string, mode: ViewMode) => void;
  togglePin: (sessionPk: string) => void;
  toggleArchive: (sessionPk: string) => void;
  /** Idempotent write — archive flows must not race a pure toggle. */
  setArchived: (sessionPk: string, on: boolean) => void;
  toggleHideInvalidModels: () => void;
  toggleNotifications: () => void;
  markRead: (sessionPk: string, ts: number) => void;
  markAllRead: (sessions: Session[]) => void;
  seedReadState: (sessions: Session[]) => void;
  toggleStatusFilter: (status: string) => void;
  toggleUnreadOnly: () => void;
};

export const useUi = create<UiState>((set, get) => ({
  leftPanelOpen: readBool(KEY.left, true),
  rightPanelOpen: readBool(KEY.right, true),
  tabs: readTabs(),
  activeTabId: normalizeActive(typeof localStorage !== "undefined" ? localStorage.getItem(KEY.active) : null),
  pinned: readSet(KEY.pinned),
  archived: readSet(KEY.archived),
  readAt: readNumMap(KEY.readAt),
  sessionFilter: readSessionFilter(KEY.sessionFilter),
  hideInvalidModels: readBool(KEY.hideInvalidModels, false),
  notificationsEnabled: readBool(KEY.notificationsEnabled, true),
  toggleLeft: () =>
    set((s) => {
      const v = !s.leftPanelOpen;
      persist(KEY.left, v ? "1" : "0");
      return { leftPanelOpen: v };
    }),
  toggleRight: () =>
    set((s) => {
      const v = !s.rightPanelOpen;
      persist(KEY.right, v ? "1" : "0");
      return { rightPanelOpen: v };
    }),
  setLeft: (open) =>
    set((s) => {
      if (s.leftPanelOpen === open) return s;
      persist(KEY.left, open ? "1" : "0");
      return { leftPanelOpen: open };
    }),
  setRight: (open) =>
    set((s) => {
      if (s.rightPanelOpen === open) return s;
      persist(KEY.right, open ? "1" : "0");
      return { rightPanelOpen: open };
    }),
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
  setActiveTab: (id) => {
    persist(KEY.active, id);
    set({ activeTabId: id });
  },
  setTabMode: (id, mode) => {
    const tabs = setTabMode(get().tabs, id, mode);
    persist(KEY.tabs, JSON.stringify(tabs));
    set({ tabs });
  },
  togglePin: (sessionPk) => {
    const pinned = toggleKey(get().pinned, sessionPk);
    persist(KEY.pinned, JSON.stringify(pinned));
    set({ pinned });
  },
  toggleArchive: (sessionPk) => {
    const archived = toggleKey(get().archived, sessionPk);
    persist(KEY.archived, JSON.stringify(archived));
    set({ archived });
  },
  setArchived: (sessionPk, on) => {
    const archived = { ...get().archived };
    if (on) archived[sessionPk] = true;
    else delete archived[sessionPk];
    persist(KEY.archived, JSON.stringify(archived));
    set({ archived });
  },
  toggleHideInvalidModels: () =>
    set((s) => {
      const v = !s.hideInvalidModels;
      persist(KEY.hideInvalidModels, v ? "1" : "0");
      return { hideInvalidModels: v };
    }),
  toggleNotifications: () =>
    set((s) => {
      const v = !s.notificationsEnabled;
      persist(KEY.notificationsEnabled, v ? "1" : "0");
      return { notificationsEnabled: v };
    }),
  markRead: (sessionPk, ts) => {
    const readAt = { ...get().readAt, [sessionPk]: ts };
    persist(KEY.readAt, JSON.stringify(readAt));
    set({ readAt });
  },
  markAllRead: (sessions) => {
    const readAt = { ...get().readAt };
    for (const s of sessions) readAt[s.sessionPk] = s.lastActive ?? 0;
    persist(KEY.readAt, JSON.stringify(readAt));
    set({ readAt });
  },
  seedReadState: (sessions) => {
    const cur = get().readAt;
    let changed = false;
    const readAt = { ...cur };
    for (const s of sessions) {
      if (readAt[s.sessionPk] === undefined) {
        readAt[s.sessionPk] = s.lastActive ?? 0;
        changed = true;
      }
    }
    if (!changed) return; // idempotent no-op when nothing absent
    persist(KEY.readAt, JSON.stringify(readAt));
    set({ readAt });
  },
  toggleStatusFilter: (status) => {
    const cur = get().sessionFilter;
    const next = { ...cur, statuses: toggleKey(cur.statuses, status) };
    persist(KEY.sessionFilter, JSON.stringify(next));
    set({ sessionFilter: next });
  },
  toggleUnreadOnly: () => {
    const cur = get().sessionFilter;
    const next = { ...cur, unreadOnly: !cur.unreadOnly };
    persist(KEY.sessionFilter, JSON.stringify(next));
    set({ sessionFilter: next });
  },
}));
