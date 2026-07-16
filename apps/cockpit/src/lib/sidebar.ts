import type { Project, Session } from "../bindings";
import { basename } from "./paths";
import { sessionKey, isSession, type SessionRef, type UiSession } from "./session-key";

export type Ordering = "updated" | "name" | "manual";

/** Task-list scope: the chat bucket, all tasks (chat + project), or one project. */
export type TaskScope = "chat" | "all" | { projectId: string };

const orderIndex = (order: string[], id: string): number => {
  const i = order.indexOf(id);
  return i === -1 ? Number.POSITIVE_INFINITY : i;
};

// Some existing projects carry their full path as the display name; show the
// final segment either way.
export function projectLabel(p: Pick<Project, "name">): string {
  return basename(p.name) || p.name;
}

export function orderProjects(projects: Project[], ordering: Ordering, order: string[] = []): Project[] {
  if (ordering === "name") return [...projects].sort((a, b) => a.name.localeCompare(b.name));
  if (ordering === "manual") return [...projects].sort((a, b) => orderIndex(order, a.projectId) - orderIndex(order, b.projectId));
  return projects; // "updated" — store order (as-fetched)
}

export function sessionTitle(s: Session): string {
  return s.title?.trim() || "Untitled session";
}

export type SessionFilterCtx = {
  statuses: Record<string, true>;
  unreadOnly: boolean;
  /** Keyed by `sessKey(runnerId, pk)`. */
  readAt: Record<string, number>;
  focusedSession: SessionRef | null;
};

/** Move `fromId` to occupy `toId`'s position (immutably). No-op if either id is
 *  absent or they are equal. */
export function reorder(list: string[], fromId: string, toId: string): string[] {
  if (fromId === toId) return list;
  const from = list.indexOf(fromId);
  const to = list.indexOf(toId);
  if (from === -1 || to === -1) return list;
  const next = [...list];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

// Sessions shown under one project row: query-filtered, archived hidden unless
// revealed, status/unread filtered, pinned first, newest first within each
// group. `kind === "project"` is required too — chat/worker/review sessions
// carry `projectId: null` and must never leak into a project's bucket.
export function sessionsForProject(
  sessions: UiSession[],
  projectId: string,
  query: string,
  showArchived: boolean,
  pinned: Record<string, true>,
  archived: Record<string, true>,
  filter: SessionFilterCtx,
  pinnedOrder: string[] = [],
): UiSession[] {
  const q = query.trim().toLowerCase();
  const statusActive = Object.keys(filter.statuses).length > 0;
  return sessions
    .filter((s) => s.projectId === projectId && s.kind === "project")
    .filter((s) => !q || sessionTitle(s).toLowerCase().includes(q))
    .filter((s) => showArchived || !archived[sessionKey(s)])
    .filter((s) => !statusActive || filter.statuses[s.status])
    .filter((s) => !filter.unreadOnly || isUnreadVisible(s, filter.readAt, filter.focusedSession))
    .sort((a, b) => {
      const ap = pinned[sessionKey(a)] ? 1 : 0;
      const bp = pinned[sessionKey(b)] ? 1 : 0;
      if (ap !== bp) return bp - ap; // pinned first
      if (ap === 1) {
        const ai = pinnedOrder.indexOf(sessionKey(a));
        const bi = pinnedOrder.indexOf(sessionKey(b));
        const av = ai === -1 ? Number.POSITIVE_INFINITY : ai;
        const bv = bi === -1 ? Number.POSITIVE_INFINITY : bi;
        if (av !== bv) return av - bv; // by manual order; unordered → after
      }
      return (b.lastActive ?? 0) - (a.lastActive ?? 0); // recency within group
    });
}

/** Pure filter for a task list — no sorting. `scope` picks the bucket:
 *  "chat" = chat-first tasks, "all" = chat + project tasks (worker/review
 *  never leak), { projectId } = one project's project-kind tasks. */
export function visibleTasks(
  sessions: UiSession[],
  scope: TaskScope,
  query: string,
  showArchived: boolean,
  archived: Record<string, true>,
): UiSession[] {
  const q = query.trim().toLowerCase();
  return sessions.filter((s) => {
    if (scope === "chat") {
      if (s.kind !== "chat") return false;
    } else if (scope === "all") {
      if (s.kind !== "chat" && s.kind !== "project") return false;
    } else {
      if (s.kind !== "project" || s.projectId !== scope.projectId) return false;
    }
    if (q && !sessionTitle(s).toLowerCase().includes(q)) return false;
    if (!showArchived && archived[sessionKey(s)]) return false;
    return true;
  });
}

/** Pure sort for an already-filtered task list. Pinned float to top by
 *  `pinnedOrder`; unpinned sort by `ordering` ("manual" uses `taskOrder`,
 *  unknown ids fall to recency). Stable within equal keys. */
export function orderTasks(
  tasks: UiSession[],
  pinned: Record<string, true>,
  pinnedOrder: string[],
  ordering: Ordering,
  taskOrder: string[],
): UiSession[] {
  return [...tasks].sort((a, b) => {
    const ka = sessionKey(a);
    const kb = sessionKey(b);
    const ap = pinned[ka] ? 1 : 0;
    const bp = pinned[kb] ? 1 : 0;
    if (ap !== bp) return bp - ap; // pinned first
    if (ap === 1) {
      const d = orderIndex(pinnedOrder, ka) - orderIndex(pinnedOrder, kb);
      if (d !== 0) return d;
      return (b.lastActive ?? 0) - (a.lastActive ?? 0);
    }
    if (ordering === "name") return sessionTitle(a).localeCompare(sessionTitle(b));
    if (ordering === "manual") {
      const d = orderIndex(taskOrder, ka) - orderIndex(taskOrder, kb);
      if (d !== 0) return d;
    }
    return (b.lastActive ?? 0) - (a.lastActive ?? 0); // "updated" & manual fallback
  });
}

// Chat-first sessions (no project attached) — the sidebar's own "Chat" bucket.
export function chatSessions(sessions: UiSession[]): UiSession[] {
  return sessions.filter((s) => s.kind === "chat");
}

export function archivedCount(sessions: UiSession[], projectId: string, archived: Record<string, true>): number {
  return sessions.filter((s) => s.projectId === projectId && archived[sessionKey(s)]).length;
}

/** A session has unseen activity iff its last-active timestamp is newer than
 *  the stored read cursor. Absent cursor → not unread (seeded on first sight);
 *  the currently-focused session is never unread — you are looking at it. */
export function isUnreadVisible(session: UiSession, readAt: Record<string, number>, focusedSession: SessionRef | null): boolean {
  if (isSession(session, focusedSession)) return false;
  const cursor = readAt[sessionKey(session)];
  return cursor != null && session.lastActive != null && session.lastActive > cursor;
}
