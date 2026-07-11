import type { Project, Session } from "../bindings";
import { basename } from "./paths";

export type Ordering = "updated" | "name";

// Some existing projects carry their full path as the display name; show the
// final segment either way.
export function projectLabel(p: Pick<Project, "name">): string {
  return basename(p.name) || p.name;
}

export function orderProjects(projects: Project[], ordering: Ordering): Project[] {
  if (ordering === "name") return [...projects].sort((a, b) => a.name.localeCompare(b.name));
  return projects;
}

export function sessionTitle(s: Session): string {
  return s.title?.trim() || "Untitled session";
}

export type SessionFilterCtx = {
  statuses: Record<string, true>;
  unreadOnly: boolean;
  readAt: Record<string, number>;
  focusedSessionPk: string | null;
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
// revealed, status/unread filtered, pinned first, newest first within each group.
export function sessionsForProject(
  sessions: Session[],
  projectId: string,
  query: string,
  showArchived: boolean,
  pinned: Record<string, true>,
  archived: Record<string, true>,
  filter: SessionFilterCtx,
  pinnedOrder: string[] = [],
): Session[] {
  const q = query.trim().toLowerCase();
  const statusActive = Object.keys(filter.statuses).length > 0;
  return sessions
    .filter((s) => s.projectId === projectId)
    .filter((s) => !q || sessionTitle(s).toLowerCase().includes(q))
    .filter((s) => showArchived || !archived[s.sessionPk])
    .filter((s) => !statusActive || filter.statuses[s.status])
    .filter((s) => !filter.unreadOnly || isUnreadVisible(s, filter.readAt, filter.focusedSessionPk))
    .sort((a, b) => {
      const ap = pinned[a.sessionPk] ? 1 : 0;
      const bp = pinned[b.sessionPk] ? 1 : 0;
      if (ap !== bp) return bp - ap; // pinned first
      if (ap === 1) {
        const ai = pinnedOrder.indexOf(a.sessionPk);
        const bi = pinnedOrder.indexOf(b.sessionPk);
        const av = ai === -1 ? Number.POSITIVE_INFINITY : ai;
        const bv = bi === -1 ? Number.POSITIVE_INFINITY : bi;
        if (av !== bv) return av - bv; // by manual order; unordered → after
      }
      return (b.lastActive ?? 0) - (a.lastActive ?? 0); // recency within group
    });
}

export function archivedCount(sessions: Session[], projectId: string, archived: Record<string, true>): number {
  return sessions.filter((s) => s.projectId === projectId && archived[s.sessionPk]).length;
}

/** A session has unseen activity iff its last-active timestamp is newer than
 *  the stored read cursor. Absent cursor → not unread (seeded on first sight);
 *  the currently-focused session is never unread — you are looking at it. */
export function isUnreadVisible(session: Session, readAt: Record<string, number>, focusedSessionPk: string | null): boolean {
  if (session.sessionPk === focusedSessionPk) return false;
  const cursor = readAt[session.sessionPk];
  return cursor != null && session.lastActive != null && session.lastActive > cursor;
}
