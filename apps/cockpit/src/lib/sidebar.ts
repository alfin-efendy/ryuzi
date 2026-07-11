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

// Sessions shown under one project row: query-filtered, archived hidden unless
// revealed, status/unread filtered, pinned first, newest first within each
// group. `kind === "project"` is required too — chat/worker/review sessions
// carry `projectId: null` and must never leak into a project's bucket.
export function sessionsForProject(
  sessions: Session[],
  projectId: string,
  query: string,
  showArchived: boolean,
  pinned: Record<string, true>,
  archived: Record<string, true>,
  filter: SessionFilterCtx,
): Session[] {
  const q = query.trim().toLowerCase();
  const statusActive = Object.keys(filter.statuses).length > 0;
  return sessions
    .filter((s) => s.projectId === projectId && s.kind === "project")
    .filter((s) => !q || sessionTitle(s).toLowerCase().includes(q))
    .filter((s) => showArchived || !archived[s.sessionPk])
    .filter((s) => !statusActive || filter.statuses[s.status])
    .filter((s) => !filter.unreadOnly || isUnreadVisible(s, filter.readAt, filter.focusedSessionPk))
    .sort((a, b) => {
      const pin = (pinned[b.sessionPk] ? 1 : 0) - (pinned[a.sessionPk] ? 1 : 0);
      if (pin !== 0) return pin;
      return (b.lastActive ?? 0) - (a.lastActive ?? 0);
    });
}

// Chat-first sessions (no project attached) — the sidebar's own "Chat" bucket.
export function chatSessions(sessions: Session[]): Session[] {
  return sessions.filter((s) => s.kind === "chat");
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
