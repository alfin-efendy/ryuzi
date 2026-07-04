use crate::domain::{Message, NewMessage, PermMode, Project, Session, SessionStatus};
use crate::paths::now_ms;
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
        M::up(
            "CREATE TABLE messages (\
                session_pk TEXT NOT NULL,\
                seq INTEGER NOT NULL,\
                role TEXT NOT NULL,\
                block_type TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                tool_call_id TEXT,\
                status TEXT,\
                tool_kind TEXT,\
                created_at INTEGER NOT NULL,\
                PRIMARY KEY (session_pk, seq)\
            );\
            CREATE INDEX idx_messages_session ON messages(session_pk, seq);\
            CREATE UNIQUE INDEX idx_messages_tool_call \
                ON messages(session_pk, tool_call_id) WHERE tool_call_id IS NOT NULL;",
        ),
        M::up(
            "CREATE TABLE tool_policies (\
                project_id TEXT NOT NULL,\
                tool TEXT NOT NULL,\
                decision TEXT NOT NULL,\
                PRIMARY KEY (project_id, tool)\
            );",
        ),
        // Live-backends batch: settings KV + agents/providers/scheduler/mcp/gateways
        // domain tables (design: docs/design/2026-07-03-cockpit-live-backends-design.md).
        M::up(
            "CREATE TABLE settings (\
                key TEXT PRIMARY KEY,\
                value TEXT NOT NULL\
            );\
            CREATE TABLE agents (\
                id TEXT PRIMARY KEY,\
                enabled INTEGER NOT NULL DEFAULT 0,\
                model TEXT,\
                perm_mode TEXT NOT NULL DEFAULT 'ask',\
                flags TEXT NOT NULL DEFAULT ''\
            );\
            CREATE TABLE agent_tiers (\
                agent_id TEXT NOT NULL,\
                tier_id TEXT NOT NULL,\
                value TEXT,\
                combo INTEGER NOT NULL DEFAULT 0,\
                PRIMARY KEY (agent_id, tier_id)\
            );\
            CREATE TABLE providers (\
                id TEXT PRIMARY KEY,\
                name TEXT NOT NULL,\
                kind TEXT NOT NULL DEFAULT '',\
                color TEXT NOT NULL DEFAULT '#8B8B8B',\
                enabled INTEGER NOT NULL DEFAULT 1,\
                strategy TEXT NOT NULL DEFAULT 'priority',\
                fail_auto INTEGER NOT NULL DEFAULT 0,\
                threshold INTEGER NOT NULL DEFAULT 95,\
                return_to_primary INTEGER NOT NULL DEFAULT 1,\
                created_at INTEGER\
            );\
            CREATE TABLE provider_accounts (\
                id TEXT PRIMARY KEY,\
                provider_id TEXT NOT NULL,\
                label TEXT NOT NULL,\
                email TEXT NOT NULL DEFAULT '',\
                plan TEXT NOT NULL DEFAULT '',\
                sort INTEGER NOT NULL DEFAULT 0,\
                active INTEGER NOT NULL DEFAULT 0,\
                session_limit_tokens INTEGER,\
                weekly_limit_tokens INTEGER\
            );\
            CREATE TABLE jobs (\
                id TEXT PRIMARY KEY,\
                name TEXT NOT NULL,\
                cron TEXT NOT NULL,\
                mode TEXT NOT NULL DEFAULT 'cron',\
                natural_text TEXT NOT NULL DEFAULT '',\
                project_id TEXT NOT NULL,\
                branch TEXT NOT NULL DEFAULT 'main',\
                agent TEXT NOT NULL DEFAULT 'claude',\
                gateway TEXT NOT NULL DEFAULT 'local',\
                enabled INTEGER NOT NULL DEFAULT 1,\
                prompt TEXT NOT NULL,\
                notify_success INTEGER NOT NULL DEFAULT 0,\
                notify_fail INTEGER NOT NULL DEFAULT 1,\
                created_at INTEGER\
            );\
            CREATE TABLE job_runs (\
                id TEXT PRIMARY KEY,\
                job_id TEXT NOT NULL,\
                status TEXT NOT NULL DEFAULT 'running',\
                started_at INTEGER NOT NULL,\
                finished_at INTEGER,\
                session_pk TEXT,\
                error TEXT,\
                add_lines INTEGER,\
                del_lines INTEGER,\
                note TEXT,\
                log TEXT\
            );\
            CREATE INDEX idx_job_runs_job ON job_runs(job_id, started_at);\
            CREATE TABLE mcp_servers (\
                id TEXT PRIMARY KEY,\
                name TEXT NOT NULL,\
                kind TEXT NOT NULL DEFAULT 'MCP server',\
                color TEXT NOT NULL DEFAULT '#8B8B8B',\
                description TEXT NOT NULL DEFAULT '',\
                transport TEXT NOT NULL DEFAULT 'stdio',\
                command TEXT,\
                args TEXT NOT NULL DEFAULT '[]',\
                env TEXT NOT NULL DEFAULT '{}',\
                url TEXT,\
                scope TEXT NOT NULL DEFAULT 'global',\
                scope_gateways TEXT NOT NULL DEFAULT '[]',\
                version TEXT,\
                publisher TEXT,\
                status TEXT NOT NULL DEFAULT 'unknown',\
                status_detail TEXT,\
                auth_kind TEXT NOT NULL DEFAULT 'none',\
                auth_detail TEXT,\
                created_at INTEGER\
            );\
            CREATE TABLE mcp_tools (\
                server_id TEXT NOT NULL,\
                name TEXT NOT NULL,\
                description TEXT NOT NULL DEFAULT '',\
                perm TEXT NOT NULL DEFAULT 'ask',\
                PRIMARY KEY (server_id, name)\
            );\
            CREATE TABLE mcp_agent_access (\
                server_id TEXT NOT NULL,\
                agent_id TEXT NOT NULL,\
                allowed INTEGER NOT NULL DEFAULT 1,\
                PRIMARY KEY (server_id, agent_id)\
            );\
            CREATE TABLE gateways (\
                id TEXT PRIMARY KEY,\
                name TEXT NOT NULL,\
                kind TEXT NOT NULL,\
                host TEXT,\
                port INTEGER,\
                username TEXT,\
                fs_mode TEXT NOT NULL DEFAULT 'projects',\
                paths TEXT NOT NULL DEFAULT '[]',\
                created_at INTEGER\
            );\
            CREATE TABLE gateway_events (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                gateway_id TEXT NOT NULL,\
                at INTEGER NOT NULL,\
                level TEXT NOT NULL DEFAULT 'info',\
                text TEXT NOT NULL\
            );\
            CREATE INDEX idx_gateway_events ON gateway_events(gateway_id, at);",
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

    /// Run a closure against a pooled connection. Domain modules (agents,
    /// providers, scheduler, mcp, gateways) keep their SQL next to their logic
    /// instead of ballooning this file with one accessor per query.
    pub async fn with_conn<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.pool.get().await?;
        let out = conn
            .interact(f)
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(out)
    }

    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let key = key.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
        .await
    }

    /// Update the user-editable project settings and return the fresh row.
    pub async fn update_project(
        &self,
        id: &str,
        model: Option<String>,
        perm_mode: PermMode,
        harness: &str,
    ) -> anyhow::Result<Option<Project>> {
        let id_owned = id.to_string();
        let harness = harness.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE projects SET model=?2, perm_mode=?3, harness=?4 WHERE project_id=?1",
                params![id_owned, model, perm_mode.as_str(), harness],
            )
            .map(|_| ())
        })
        .await?;
        self.get_project(id).await
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
                let mut stmt = c.prepare(&format!(
                    "SELECT {PROJECT_COLS} FROM projects ORDER BY created_at"
                ))?;
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

    /// Atomically demote `Running → Idle` only if the current status is still `Running`.
    /// A session already marked `Interrupted` or `Ended` is left untouched.
    pub async fn demote_if_running(&self, pk: &str, last_active: i64) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, last_active=?3 WHERE session_pk=?1 AND status=?4",
                params![
                    pk,
                    SessionStatus::Idle.as_str(),
                    last_active,
                    SessionStatus::Running.as_str()
                ],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// Forget a torn-down worktree: the path and branch are gone from disk, so
    /// later cold-resumes must fall back to the project workdir.
    pub async fn clear_session_worktree(&self, pk: &str) -> anyhow::Result<()> {
        let pk = pk.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET worktree_path=NULL, branch=NULL WHERE session_pk=?1",
                params![pk],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn update_agent_session_id(
        &self,
        pk: &str,
        agent_session_id: &str,
    ) -> anyhow::Result<()> {
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

    pub async fn insert_message(&self, m: NewMessage) -> anyhow::Result<i64> {
        let payload = serde_json::to_string(&m.payload)?;
        let created = now_ms();
        let conn = self.pool.get().await?;
        let seq = conn
            .interact(move |c| {
                c.query_row(
                    "INSERT INTO messages(session_pk,seq,role,block_type,payload,tool_call_id,status,tool_kind,created_at) \
                     SELECT ?1, COALESCE(MAX(seq),0)+1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 \
                     FROM messages WHERE session_pk=?1 \
                     RETURNING seq",
                    params![m.session_pk, m.role, m.block_type, payload,
                            m.tool_call_id, m.status, m.tool_kind, created],
                    |r| r.get::<_, i64>(0),
                )
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(seq)
    }

    pub async fn list_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        let session_pk = session_pk.to_string();
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(move |c| -> rusqlite::Result<Vec<Message>> {
                let mut stmt = c.prepare(
                    "SELECT session_pk,seq,role,block_type,payload,tool_call_id,status,tool_kind,created_at \
                     FROM messages WHERE session_pk=?1 ORDER BY seq",
                )?;
                let items = stmt
                    .query_map(params![session_pk], |r| {
                        let payload: String = r.get(4)?;
                        Ok(Message {
                            session_pk: r.get(0)?,
                            seq: r.get(1)?,
                            role: r.get(2)?,
                            block_type: r.get(3)?,
                            payload: serde_json::from_str(&payload).map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
                            })?,
                            tool_call_id: r.get(5)?,
                            status: r.get(6)?,
                            tool_kind: r.get(7)?,
                            created_at: r.get(8)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(items)
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
    }

    /// Return the persisted decision for `(project_id, tool)`, or `None` if no
    /// policy has been set.
    pub async fn get_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
    ) -> anyhow::Result<Option<String>> {
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        let conn = self.pool.get().await?;
        let result = conn
            .interact(move |c| {
                c.query_row(
                    "SELECT decision FROM tool_policies WHERE project_id=?1 AND tool=?2",
                    params![project_id, tool],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(result)
    }

    /// Upsert a tool policy: set `decision` for `(project_id, tool)`.
    /// On conflict (same project+tool already has a policy), update the decision.
    pub async fn set_tool_policy(
        &self,
        project_id: &str,
        tool: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        let decision = decision.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO tool_policies(project_id, tool, decision) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(project_id, tool) DO UPDATE SET decision=excluded.decision",
                params![project_id, tool, decision],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// Merge `patch` into the tool_call row's payload (SQLite `json_patch`,
    /// so the original `{name, input}` survives an `{output: …}` update),
    /// optionally flip status, and return the row's seq, the merged payload,
    /// and its persisted tool_kind — the caller re-emits all three.
    pub async fn update_tool_call(
        &self,
        session_pk: &str,
        tool_call_id: &str,
        status: Option<&str>,
        patch: &serde_json::Value,
    ) -> anyhow::Result<(i64, serde_json::Value, Option<String>)> {
        let session_pk = session_pk.to_string();
        let tool_call_id = tool_call_id.to_string();
        let status = status.map(|s| s.to_string());
        let patch = serde_json::to_string(patch)?;
        let conn = self.pool.get().await?;
        let (seq, payload, tool_kind) = conn
            .interact(move |c| {
                c.query_row(
                    "UPDATE messages SET payload=json_patch(payload, ?3), status=COALESCE(?4, status) \
                     WHERE session_pk=?1 AND tool_call_id=?2 RETURNING seq, payload, tool_kind",
                    params![session_pk, tool_call_id, patch, status],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        Ok((seq, payload, tool_kind))
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
    use crate::domain::{NewMessage, PermMode, Project};
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
    async fn messages_get_monotonic_per_session_seq_and_list_in_order() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        let a1 = store.insert_message(NewMessage::block("s1", "user", "text",
            serde_json::json!({"text": "hi"}))).await.unwrap();
        let a2 = store.insert_message(NewMessage::block("s1", "assistant", "text",
            serde_json::json!({"text": "hello"}))).await.unwrap();
        // A different session has an INDEPENDENT seq sequence starting at 1.
        let b1 = store.insert_message(NewMessage::block("s2", "user", "text",
            serde_json::json!({"text": "yo"}))).await.unwrap();

        assert_eq!((a1, a2, b1), (1, 2, 1));

        let s1 = store.list_messages("s1").await.unwrap();
        assert_eq!(s1.len(), 2);
        assert_eq!(s1[0].seq, 1);
        assert_eq!(s1[0].role, "user");
        assert_eq!(s1[0].payload["text"], "hi");
        assert_eq!(s1[1].seq, 2);
        assert_eq!(s1[1].payload["text"], "hello");
        assert!(store.list_messages("missing").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tool_call_update_merges_output_and_returns_kind() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        store.insert_message(NewMessage {
            session_pk: "s1".into(), role: "assistant".into(), block_type: "tool_call".into(),
            payload: serde_json::json!({"name": "Bash", "input": {"command": "ls"}}),
            tool_call_id: Some("tc-1".into()), status: Some("pending".into()),
            tool_kind: Some("execute".into()),
        }).await.unwrap();

        // The caller now sends ONLY the update patch; the store merges it.
        let (seq, merged, kind) = store.update_tool_call("s1", "tc-1", Some("completed"),
            &serde_json::json!({"output": "file.txt"}))
            .await.unwrap();
        assert_eq!(seq, 1, "update_tool_call must return the row's real seq");
        assert_eq!(merged["name"], "Bash", "merge must preserve the original name");
        assert_eq!(merged["input"]["command"], "ls", "merge must preserve the original input");
        assert_eq!(merged["output"], "file.txt", "merge must add the output");
        assert_eq!(kind.as_deref(), Some("execute"), "must return the row's persisted tool_kind");

        let rows = store.list_messages("s1").await.unwrap();
        assert_eq!(rows.len(), 1, "update must not insert a new row");
        assert_eq!(rows[0].status.as_deref(), Some("completed"));
        assert_eq!(rows[0].payload["name"], "Bash");
        assert_eq!(rows[0].payload["output"], "file.txt");

        // An empty patch (ToolCallDone with no raw_output) must leave payload intact.
        let (_, merged2, _) = store.update_tool_call("s1", "tc-1", None,
            &serde_json::json!({})).await.unwrap();
        assert_eq!(merged2["name"], "Bash");
        assert_eq!(merged2["output"], "file.txt");
    }

    #[tokio::test]
    async fn update_tool_call_errors_when_row_absent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let res = store
            .update_tool_call("s1", "missing-tc", Some("completed"), &serde_json::json!({}))
            .await;
        assert!(res.is_err(), "updating a nonexistent tool_call_id must error");
    }

    #[tokio::test]
    async fn tool_policy_is_per_project_and_upserts() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // initially no policy
        assert!(store.get_tool_policy("p1", "Bash").await.unwrap().is_none());
        // set a policy
        store.set_tool_policy("p1", "Bash", "allowAlways").await.unwrap();
        assert_eq!(
            store.get_tool_policy("p1", "Bash").await.unwrap().as_deref(),
            Some("allowAlways")
        );
        // different project is independent
        assert!(store.get_tool_policy("p2", "Bash").await.unwrap().is_none());
        // upsert (update) the existing policy
        store.set_tool_policy("p1", "Bash", "rejectAlways").await.unwrap();
        assert_eq!(
            store.get_tool_policy("p1", "Bash").await.unwrap().as_deref(),
            Some("rejectAlways")
        );
    }

    #[tokio::test]
    async fn settings_kv_upserts_and_reads_back() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(store.get_setting("default_agent").await.unwrap().is_none());
        store.set_setting("default_agent", "claude").await.unwrap();
        assert_eq!(
            store.get_setting("default_agent").await.unwrap().as_deref(),
            Some("claude")
        );
        store.set_setting("default_agent", "codex").await.unwrap();
        assert_eq!(
            store.get_setting("default_agent").await.unwrap().as_deref(),
            Some("codex")
        );
    }

    #[tokio::test]
    async fn update_project_persists_settings() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();

        let updated = store
            .update_project(
                "p1",
                Some("claude-opus-4-5".into()),
                PermMode::AcceptEdits,
                "claude-code",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.model.as_deref(), Some("claude-opus-4-5"));
        assert_eq!(updated.perm_mode, PermMode::AcceptEdits);

        // Unknown project → Ok(None), not an error.
        assert!(store
            .update_project("missing", None, PermMode::Default, "claude-code")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn clear_session_worktree_forgets_path_and_branch() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();

        store.clear_session_worktree("s1").await.unwrap();
        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.worktree_path, None);
        assert_eq!(got.branch, None);
    }

    #[tokio::test]
    async fn session_insert_get_list_and_updates() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();

        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.status, SessionStatus::Running);

        store
            .update_agent_session_id("s1", "agent-xyz")
            .await
            .unwrap();
        store
            .update_status("s1", SessionStatus::Idle, Some(99))
            .await
            .unwrap();

        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.agent_session_id.as_deref(), Some("agent-xyz"));
        assert_eq!(got.status, SessionStatus::Idle);
        assert_eq!(got.last_active, Some(99));

        assert_eq!(store.list_sessions(Some("p1")).await.unwrap().len(), 1);
        assert_eq!(store.list_sessions(None).await.unwrap().len(), 1);
    }
}
