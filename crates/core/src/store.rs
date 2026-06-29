use crate::domain::{PermMode, Project, Session, SessionStatus};
use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::{params, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};
use std::path::Path;

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            "CREATE TABLE projects (\
                project_id TEXT PRIMARY KEY,\
                name TEXT,\
                workdir TEXT NOT NULL,\
                source TEXT,\
                harness TEXT NOT NULL DEFAULT 'claude-code',\
                model TEXT,\
                effort TEXT,\
                perm_mode TEXT NOT NULL DEFAULT 'default',\
                created_at INTEGER\
            );",
        ),
        M::up(
            "CREATE TABLE sessions (\
                session_pk TEXT PRIMARY KEY,\
                project_id TEXT NOT NULL,\
                agent_session_id TEXT,\
                worktree_path TEXT,\
                branch TEXT,\
                title TEXT,\
                status TEXT NOT NULL DEFAULT 'idle',\
                created_at INTEGER,\
                last_active INTEGER\
            );",
        ),
    ])
}

pub struct Store {
    pool: Pool,
}

fn row_to_project(r: &Row) -> rusqlite::Result<Project> {
    let perm: String = r.get(7)?;
    Ok(Project {
        project_id: r.get(0)?,
        name: r.get(1)?,
        workdir: r.get(2)?,
        source: r.get(3)?,
        harness: r.get(4)?,
        model: r.get(5)?,
        effort: r.get(6)?,
        perm_mode: PermMode::from_db(&perm),
        created_at: r.get(8)?,
    })
}

const PROJECT_COLS: &str =
    "project_id,name,workdir,source,harness,model,effort,perm_mode,created_at";

impl Store {
    pub async fn open(path: &Path) -> anyhow::Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let cfg = Config::new(path);
        let pool = cfg.create_pool(Runtime::Tokio1)?;
        let conn = pool.get().await?;
        conn.interact(|c| {
            let _ = c.pragma_update(None, "journal_mode", "WAL");
            migrations().to_latest(c)
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(Store { pool })
    }

    pub async fn insert_project(&self, p: Project) -> anyhow::Result<()> {
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO projects(project_id,name,workdir,source,harness,model,effort,perm_mode,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    p.project_id, p.name, p.workdir, p.source, p.harness,
                    p.model, p.effort, p.perm_mode.as_str(), p.created_at
                ],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> anyhow::Result<Option<Project>> {
        let id = id.to_string();
        let conn = self.pool.get().await?;
        let p = conn
            .interact(move |c| {
                c.query_row(
                    &format!("SELECT {PROJECT_COLS} FROM projects WHERE project_id=?1"),
                    params![id],
                    row_to_project,
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(p)
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(|c| -> rusqlite::Result<Vec<Project>> {
                let mut stmt =
                    c.prepare(&format!("SELECT {PROJECT_COLS} FROM projects ORDER BY created_at"))?;
                let items = stmt
                    .query_map([], row_to_project)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(items)
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
    }

    pub async fn insert_session(&self, s: Session) -> anyhow::Result<()> {
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    s.session_pk, s.project_id, s.agent_session_id, s.worktree_path,
                    s.branch, s.title, s.status.as_str(), s.created_at, s.last_active
                ],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    pub async fn get_session(&self, pk: &str) -> anyhow::Result<Option<Session>> {
        let pk = pk.to_string();
        let conn = self.pool.get().await?;
        let s = conn
            .interact(move |c| {
                c.query_row(
                    &format!("SELECT {SESSION_COLS} FROM sessions WHERE session_pk=?1"),
                    params![pk],
                    row_to_session,
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(s)
    }

    pub async fn list_sessions(&self, project_id: Option<&str>) -> anyhow::Result<Vec<Session>> {
        let project_id = project_id.map(|s| s.to_string());
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(move |c| {
                match project_id {
                    Some(pid) => {
                        let mut stmt = c.prepare(&format!(
                            "SELECT {SESSION_COLS} FROM sessions WHERE project_id=?1 ORDER BY created_at"
                        ))?;
                        let rows = stmt.query_map(params![pid], row_to_session)?
                            .collect::<rusqlite::Result<Vec<_>>>();
                        rows
                    }
                    None => {
                        let mut stmt = c.prepare(&format!(
                            "SELECT {SESSION_COLS} FROM sessions ORDER BY created_at"
                        ))?;
                        let rows = stmt.query_map([], row_to_session)?
                            .collect::<rusqlite::Result<Vec<_>>>();
                        rows
                    }
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
    }

    pub async fn update_status(
        &self,
        pk: &str,
        status: SessionStatus,
        last_active: Option<i64>,
    ) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, last_active=COALESCE(?3, last_active) WHERE session_pk=?1",
                params![pk, status.as_str(), last_active],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    pub async fn update_agent_session_id(&self, pk: &str, agent_session_id: &str) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let agent = agent_session_id.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE sessions SET agent_session_id=?2 WHERE session_pk=?1",
                params![pk, agent],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }
}

const SESSION_COLS: &str =
    "session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active";

fn row_to_session(r: &Row) -> rusqlite::Result<Session> {
    let status: String = r.get(6)?;
    Ok(Session {
        session_pk: r.get(0)?,
        project_id: r.get(1)?,
        agent_session_id: r.get(2)?,
        worktree_path: r.get(3)?,
        branch: r.get(4)?,
        title: r.get(5)?,
        status: SessionStatus::from_db(&status),
        created_at: r.get(7)?,
        last_active: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PermMode, Project};
    use crate::domain::{Session, SessionStatus};

    fn sample_project() -> Project {
        Project {
            project_id: "p1".into(),
            name: "demo".into(),
            workdir: "/tmp/demo".into(),
            source: None,
            harness: "claude-code".into(),
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(123),
        }
    }

    #[tokio::test]
    async fn insert_then_get_and_list_projects() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        store.insert_project(sample_project()).await.unwrap();

        let got = store.get_project("p1").await.unwrap().unwrap();
        assert_eq!(got.name, "demo");
        assert_eq!(got.perm_mode, PermMode::Default);

        assert!(store.get_project("missing").await.unwrap().is_none());
        assert_eq!(store.list_projects().await.unwrap().len(), 1);
    }

    fn sample_session() -> Session {
        Session {
            session_pk: "s1".into(),
            project_id: "p1".into(),
            agent_session_id: None,
            worktree_path: Some("/tmp/wt".into()),
            branch: Some("harness/abcdef01".into()),
            title: Some("hello".into()),
            status: SessionStatus::Running,
            created_at: Some(1),
            last_active: Some(1),
        }
    }

    #[tokio::test]
    async fn session_insert_get_list_and_updates() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();

        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.status, SessionStatus::Running);

        store.update_agent_session_id("s1", "agent-xyz").await.unwrap();
        store.update_status("s1", SessionStatus::Idle, Some(99)).await.unwrap();

        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.agent_session_id.as_deref(), Some("agent-xyz"));
        assert_eq!(got.status, SessionStatus::Idle);
        assert_eq!(got.last_active, Some(99));

        assert_eq!(store.list_sessions(Some("p1")).await.unwrap().len(), 1);
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
    }
}
