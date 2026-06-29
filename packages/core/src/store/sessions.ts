import type { Database } from "bun:sqlite";
import type { Session, SessionStatus, Surface } from "@harness/protocol";

interface Row {
  session_pk: string;
  project_id: string;
  agent_session_id: string | null;
  worktree_path: string | null;
  branch: string | null;
  title: string | null;
  status: string;
  started_by: string | null;
  created_at: number | null;
  last_active: number | null;
}

function toSession(r: Row): Session {
  return {
    sessionPk: r.session_pk,
    projectId: r.project_id,
    agentSessionId: r.agent_session_id ?? undefined,
    worktreePath: r.worktree_path ?? undefined,
    branch: r.branch ?? undefined,
    title: r.title ?? undefined,
    status: r.status as SessionStatus,
    startedBy: r.started_by ?? undefined,
    createdAt: r.created_at ?? undefined,
    lastActive: r.last_active ?? undefined,
  };
}

const COLUMN: Record<keyof Session, string> = {
  sessionPk: "session_pk",
  projectId: "project_id",
  agentSessionId: "agent_session_id",
  worktreePath: "worktree_path",
  branch: "branch",
  title: "title",
  status: "status",
  startedBy: "started_by",
  createdAt: "created_at",
  lastActive: "last_active",
};

export class SessionsStore {
  constructor(private db: Database) {}

  insert(s: Session): void {
    this.db.run(
      `INSERT INTO sessions(session_pk, project_id, agent_session_id, worktree_path, branch, title, status, started_by, created_at, last_active)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      [
        s.sessionPk,
        s.projectId,
        s.agentSessionId ?? null,
        s.worktreePath ?? null,
        s.branch ?? null,
        s.title ?? null,
        s.status,
        s.startedBy ?? null,
        s.createdAt ?? null,
        s.lastActive ?? null,
      ],
    );
  }

  get(sessionPk: string): Session | undefined {
    const r = this.db.query<Row, [string]>("SELECT * FROM sessions WHERE session_pk = ?").get(sessionPk);
    return r ? toSession(r) : undefined;
  }

  list(projectId?: string): Session[] {
    const rows = projectId
      ? this.db.query<Row, [string]>("SELECT * FROM sessions WHERE project_id = ? ORDER BY created_at").all(projectId)
      : this.db.query<Row, []>("SELECT * FROM sessions ORDER BY created_at").all();
    return rows.map(toSession);
  }

  update(sessionPk: string, patch: Partial<Session>): void {
    const entries = Object.entries(patch).filter(([k]) => k !== "sessionPk" && k in COLUMN);
    if (entries.length === 0) return;
    const sets = entries.map(([k]) => `${COLUMN[k as keyof Session]} = ?`).join(", ");
    const values = entries.map(([, v]) => (v === undefined ? null : v));
    this.db.run(`UPDATE sessions SET ${sets} WHERE session_pk = ?`, [...values, sessionPk]);
  }

  addSurface(gateway: string, conversationId: string, sessionPk: string): void {
    this.db.run(
      `INSERT INTO session_surfaces(gateway, conversation_id, session_pk) VALUES (?, ?, ?)
       ON CONFLICT(gateway, conversation_id) DO UPDATE SET session_pk = excluded.session_pk`,
      [gateway, conversationId, sessionPk],
    );
  }

  resolveByConversation(gateway: string, conversationId: string): Session | undefined {
    const r = this.db
      .query<Row, [string, string]>(
        `SELECT s.* FROM sessions s
         JOIN session_surfaces sf ON sf.session_pk = s.session_pk
         WHERE sf.gateway = ? AND sf.conversation_id = ?`,
      )
      .get(gateway, conversationId);
    return r ? toSession(r) : undefined;
  }

  surfaces(sessionPk: string): Surface[] {
    return this.db
      .query<{ gateway: string; conversation_id: string }, [string]>(
        "SELECT gateway, conversation_id FROM session_surfaces WHERE session_pk = ?",
      )
      .all(sessionPk)
      .map((r) => ({ gateway: r.gateway, conversationId: r.conversation_id }));
  }
}
