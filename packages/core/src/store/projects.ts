import type { Database } from "bun:sqlite";
import type { Project, PermMode } from "@harness/protocol";

interface Row {
  project_id: string;
  name: string;
  workdir: string;
  source: string | null;
  harness: string;
  model: string | null;
  effort: string | null;
  perm_mode: string;
  created_by: string | null;
  created_at: number | null;
}

function toProject(r: Row): Project {
  return {
    projectId: r.project_id,
    name: r.name,
    workdir: r.workdir,
    source: r.source ?? undefined,
    harness: r.harness,
    model: r.model ?? undefined,
    effort: r.effort ?? undefined,
    permMode: r.perm_mode as PermMode,
    createdBy: r.created_by ?? undefined,
    createdAt: r.created_at ?? undefined,
  };
}

export class ProjectsStore {
  constructor(private db: Database) {}

  insert(p: Project): void {
    this.db.run(
      `INSERT INTO projects(project_id, name, workdir, source, harness, model, effort, perm_mode, created_by, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      [
        p.projectId,
        p.name,
        p.workdir,
        p.source ?? null,
        p.harness,
        p.model ?? null,
        p.effort ?? null,
        p.permMode,
        p.createdBy ?? null,
        p.createdAt ?? null,
      ],
    );
  }

  get(projectId: string): Project | undefined {
    const r = this.db.query<Row, [string]>("SELECT * FROM projects WHERE project_id = ?").get(projectId);
    return r ? toProject(r) : undefined;
  }

  list(): Project[] {
    return this.db.query<Row, []>("SELECT * FROM projects ORDER BY created_at").all().map(toProject);
  }

  bind(gateway: string, workspaceId: string, projectId: string): void {
    this.db.run(
      `INSERT INTO project_bindings(gateway, workspace_id, project_id) VALUES (?, ?, ?)
       ON CONFLICT(gateway, workspace_id) DO UPDATE SET project_id = excluded.project_id`,
      [gateway, workspaceId, projectId],
    );
  }

  resolveByWorkspace(gateway: string, workspaceId: string): Project | undefined {
    const r = this.db
      .query<Row, [string, string]>(
        `SELECT p.* FROM projects p
         JOIN project_bindings b ON b.project_id = p.project_id
         WHERE b.gateway = ? AND b.workspace_id = ?`,
      )
      .get(gateway, workspaceId);
    return r ? toProject(r) : undefined;
  }
}
