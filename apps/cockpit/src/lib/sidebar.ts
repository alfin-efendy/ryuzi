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
  if (ordering === "manual") {
    return [...projects].sort((a, b) => {
      const ia = orderIndex(order, a.projectId);
      const ib = orderIndex(order, b.projectId);
      // Compare indices before subtracting: two absent ids are both +Infinity, and
      // `Infinity - Infinity` is NaN (treated as "equal" by sort, but unsafe). Equal
      // indices (both absent → both Infinity, or same position) sort as 0, avoiding NaN.
      return ia === ib ? 0 : ia - ib;
    });
  }
  return projects; // "updated" — store order (as-fetched)
}

export function sessionTitle(s: Session): string {
  return s.title?.trim() || "Untitled session";
}

/** Given two composite keys and whether the dragged row is pinned, decide the
 *  reorder target. Returns null when the drop crosses partitions (pinned vs
 *  unpinned) or is a no-op. */
export function dropTarget(activeId: string, overId: string, isActivePinned: boolean, isOverPinned: boolean): "pinned" | "manual" | null {
  if (activeId === overId) return null;
  if (isActivePinned !== isOverPinned) return null;
  return isActivePinned ? "pinned" : "manual";
}

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
      // Compare indices before subtracting so two unordered pins (both +Infinity)
      // fall through to recency instead of returning `Infinity - Infinity` = NaN.
      const ia = orderIndex(pinnedOrder, ka);
      const ib = orderIndex(pinnedOrder, kb);
      if (ia !== ib) return ia - ib;
      return (b.lastActive ?? 0) - (a.lastActive ?? 0);
    }
    if (ordering === "name") return sessionTitle(a).localeCompare(sessionTitle(b));
    if (ordering === "manual") {
      // Same NaN trap: two ids absent from `taskOrder` are both +Infinity, so only
      // return the delta when the indices differ; otherwise fall through to recency.
      const ia = orderIndex(taskOrder, ka);
      const ib = orderIndex(taskOrder, kb);
      if (ia !== ib) return ia - ib;
    }
    return (b.lastActive ?? 0) - (a.lastActive ?? 0); // "updated" & manual fallback
  });
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
