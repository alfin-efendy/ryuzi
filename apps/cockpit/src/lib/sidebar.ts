import type { Project, Session } from "../bindings";

export type Ordering = "updated" | "name";

export function orderProjects(projects: Project[], ordering: Ordering): Project[] {
  if (ordering === "name") return [...projects].sort((a, b) => a.name.localeCompare(b.name));
  return projects;
}

export function sessionTitle(s: Session): string {
  return s.title?.trim() || "Untitled session";
}

// Sessions shown under one project row: query-filtered, archived hidden unless
// revealed, pinned first, newest first within each group.
export function sessionsForProject(
  sessions: Session[],
  projectId: string,
  query: string,
  showArchived: boolean,
  pinned: Record<string, true>,
  archived: Record<string, true>,
): Session[] {
  const q = query.trim().toLowerCase();
  return sessions
    .filter((s) => s.projectId === projectId)
    .filter((s) => !q || sessionTitle(s).toLowerCase().includes(q))
    .filter((s) => showArchived || !archived[s.sessionPk])
    .sort((a, b) => {
      const pin = (pinned[b.sessionPk] ? 1 : 0) - (pinned[a.sessionPk] ? 1 : 0);
      if (pin !== 0) return pin;
      return (b.lastActive ?? 0) - (a.lastActive ?? 0);
    });
}

export function archivedCount(sessions: Session[], projectId: string, archived: Record<string, true>): number {
  return sessions.filter((s) => s.projectId === projectId && archived[s.sessionPk]).length;
}
