import { create } from "zustand";
import { basename } from "./lib/paths";
import type { ViewMode } from "./lib/preview";
import { reorder, type Ordering } from "@/lib/sidebar";
import { sessionKey, type UiSession } from "@/lib/session-key";

export type DockTab = { id: string; kind: "file"; path: string; title: string; mode?: ViewMode };

/** Shared bucket key for the top "Tasks" section's manual order. */
export const TASKS_BUCKET = "__tasks__";

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
  pinnedOrder: "cockpit.ui.pinnedOrder",
  archived: "cockpit.ui.archived",
  hideInvalidModels: "cockpit.ui.hideInvalidModels",
  notificationsEnabled: "cockpit.ui.notificationsEnabled",
  readAt: "cockpit.ui.readAt",
  organizeBy: "cockpit.ui.organizeBy",
  taskOrdering: "cockpit.ui.taskOrdering",
  projectOrdering: "cockpit.ui.projectOrdering",
  taskOrder: "cockpit.ui.taskOrder",
  projectOrder: "cockpit.ui.projectOrder",
  collapsed: "cockpit.ui.collapsed",
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
function readList(key: string): string[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
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
function readOrdering(key: string): Ordering {
  if (typeof localStorage === "undefined") return "updated";
  const v = localStorage.getItem(key);
  return v === "name" || v === "manual" ? v : "updated";
}
function readBoolMap(key: string): Record<string, boolean> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}
function readOrderMap(key: string): Record<string, string[]> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, string[]>) : {};
  } catch {
    return {};
  }
}
/** Reconcile a stored manual order to the current visible set: keep stored
 *  ids still present (in stored order), then append newly-seen ids. */
function reconcileOrder(stored: string[], currentIds: string[]): string[] {
  const present = stored.filter((id) => currentIds.includes(id));
  const added = currentIds.filter((id) => !present.includes(id));
  return [...present, ...added];
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
  /** Runner-safe: keyed by `sessKey(runnerId, pk)`. */
  pinned: Record<string, true>;
  pinnedOrder: string[];
  archived: Record<string, true>;
  /** Per-session last-read epoch-ms cursor (unread = lastActive > cursor),
   *  keyed by `sessKey(runnerId, pk)`. */
  readAt: Record<string, number>;
  /** Sidebar grouping: by project, or a flat task list. */
  organizeBy: "project" | "task";
  taskOrdering: Ordering;
  projectOrdering: Ordering;
  /** Manual task order, keyed by bucket (project id, chat, or `TASKS_BUCKET`). */
  taskOrder: Record<string, string[]>;
  projectOrder: string[];
  /** Sidebar section/group collapse state, keyed by composite section keys. */
  collapsed: Record<string, boolean>;
  /** Hide models with a persisted "invalid" verdict app-wide: the Provider
   *  Models card rows AND every model picker (composers, route targets,
   *  runtime config). A picker's current selection always stays visible,
   *  flagged as invalid. */
  hideInvalidModels: boolean;
  /** Gates OS/desktop notifications (native alerts) for session events. The
   *  dock badge always reflects attention regardless of this flag. */
  notificationsEnabled: boolean;
  toggleLeft: () => void;
  toggleRight: () => void;
  setLeft: (open: boolean) => void;
  setRight: (open: boolean) => void;
  openFile: (path: string) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  setTabMode: (id: string, mode: ViewMode) => void;
  /** All session args below are composite `sessKey(runnerId, pk)` strings. */
  togglePin: (key: string) => void;
  reorderPinned: (fromId: string, toId: string) => void;
  toggleArchive: (key: string) => void;
  /** Idempotent write — archive flows must not race a pure toggle. */
  setArchived: (key: string, on: boolean) => void;
  toggleHideInvalidModels: () => void;
  toggleNotifications: () => void;
  markRead: (key: string, ts: number) => void;
  seedReadState: (sessions: UiSession[]) => void;
  setOrganizeBy: (v: "project" | "task") => void;
  setTaskOrdering: (o: Ordering) => void;
  setProjectOrdering: (o: Ordering) => void;
  reorderTasks: (bucket: string, fromId: string, toId: string, currentIds: string[]) => void;
  reorderProjects: (fromId: string, toId: string, currentIds: string[]) => void;
  toggleCollapsed: (key: string) => void;
};

export const useUi = create<UiState>((set, get) => ({
  leftPanelOpen: readBool(KEY.left, true),
  rightPanelOpen: readBool(KEY.right, true),
  tabs: readTabs(),
  activeTabId: normalizeActive(typeof localStorage !== "undefined" ? localStorage.getItem(KEY.active) : null),
  pinned: readSet(KEY.pinned),
  pinnedOrder: readList(KEY.pinnedOrder),
  archived: readSet(KEY.archived),
  readAt: readNumMap(KEY.readAt),
  organizeBy: typeof localStorage !== "undefined" && localStorage.getItem(KEY.organizeBy) === "task" ? "task" : "project",
  taskOrdering: readOrdering(KEY.taskOrdering),
  projectOrdering: readOrdering(KEY.projectOrdering),
  taskOrder: readOrderMap(KEY.taskOrder),
  projectOrder: readList(KEY.projectOrder),
  collapsed: readBoolMap(KEY.collapsed),
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
  togglePin: (key) => {
    const pinned = toggleKey(get().pinned, key);
    const nowPinned = !!pinned[key];
    const cur = get().pinnedOrder;
    const pinnedOrder = nowPinned ? (cur.includes(key) ? cur : [...cur, key]) : cur.filter((id) => id !== key);
    persist(KEY.pinned, JSON.stringify(pinned));
    persist(KEY.pinnedOrder, JSON.stringify(pinnedOrder));
    set({ pinned, pinnedOrder });
  },
  reorderPinned: (fromId, toId) => {
    const pinnedOrder = reorder(get().pinnedOrder, fromId, toId);
    persist(KEY.pinnedOrder, JSON.stringify(pinnedOrder));
    set({ pinnedOrder });
  },
  toggleArchive: (key) => {
    const archived = toggleKey(get().archived, key);
    persist(KEY.archived, JSON.stringify(archived));
    set({ archived });
  },
  setArchived: (key, on) => {
    const archived = { ...get().archived };
    if (on) archived[key] = true;
    else delete archived[key];
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
  markRead: (key, ts) => {
    const readAt = { ...get().readAt, [key]: ts };
    persist(KEY.readAt, JSON.stringify(readAt));
    set({ readAt });
  },
  seedReadState: (sessions) => {
    const cur = get().readAt;
    let changed = false;
    const readAt = { ...cur };
    for (const s of sessions) {
      const key = sessionKey(s);
      if (readAt[key] === undefined) {
        readAt[key] = s.lastActive ?? 0;
        changed = true;
      }
    }
    if (!changed) return; // idempotent no-op when nothing absent
    persist(KEY.readAt, JSON.stringify(readAt));
    set({ readAt });
  },
  setOrganizeBy: (v) => {
    persist(KEY.organizeBy, v);
    set({ organizeBy: v });
  },
  setTaskOrdering: (o) => {
    persist(KEY.taskOrdering, o);
    set({ taskOrdering: o });
  },
  setProjectOrdering: (o) => {
    persist(KEY.projectOrdering, o);
    set({ projectOrdering: o });
  },
  reorderTasks: (bucket, fromId, toId, currentIds) => {
    const base = reconcileOrder(get().taskOrder[bucket] ?? [], currentIds);
    const next = { ...get().taskOrder, [bucket]: reorder(base, fromId, toId) };
    persist(KEY.taskOrder, JSON.stringify(next));
    set({ taskOrder: next });
  },
  reorderProjects: (fromId, toId, currentIds) => {
    const projectOrder = reorder(reconcileOrder(get().projectOrder, currentIds), fromId, toId);
    persist(KEY.projectOrder, JSON.stringify(projectOrder));
    set({ projectOrder });
  },
  toggleCollapsed: (key) => {
    const collapsed = { ...get().collapsed, [key]: !get().collapsed[key] };
    persist(KEY.collapsed, JSON.stringify(collapsed));
    set({ collapsed });
  },
}));
