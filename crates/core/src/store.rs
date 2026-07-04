use crate::domain::{Message, NewMessage, PermMode, Project, Session, SessionStatus, Surface};
use crate::paths::now_ms;
use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::{params, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};
use std::path::{Path, PathBuf};

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
        M::up(
            "CREATE TABLE settings (\
                key TEXT PRIMARY KEY,\
                value TEXT\
            );\
            CREATE TABLE session_surfaces (\
                gateway TEXT NOT NULL,\
                conversation_id TEXT NOT NULL,\
                session_pk TEXT NOT NULL,\
                PRIMARY KEY (gateway, conversation_id)\
            );\
            CREATE TABLE project_bindings (\
                gateway TEXT NOT NULL,\
                workspace_id TEXT NOT NULL,\
                project_id TEXT NOT NULL,\
                PRIMARY KEY (gateway, workspace_id)\
            );\
            CREATE TABLE audit (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                gateway TEXT,\
                conversation_id TEXT,\
                actor TEXT,\
                action TEXT,\
                tool TEXT,\
                decision TEXT,\
                at INTEGER\
            );\
            ALTER TABLE sessions ADD COLUMN started_by TEXT;\
            ALTER TABLE sessions ADD COLUMN resume_attempts INTEGER NOT NULL DEFAULT 0;",
        ),
        // Live-backends batch: settings KV + agents/providers/scheduler/mcp/gateways
        // domain tables (design: docs/design/2026-07-03-cockpit-live-backends-design.md).
        M::up(
            "CREATE TABLE agents (\
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
        // Models & Runtime batch (design: docs/design/2026-07-04-models-runtime-design.md):
        // provider connections carry real credentials; endpoint_keys gate the
        // local router endpoint. Keys are plaintext by design (config-apply
        // must re-read them; OS-keychain encryption is future work).
        M::up(
            "CREATE TABLE provider_connections (\
                id TEXT PRIMARY KEY,\
                provider TEXT NOT NULL,\
                auth_type TEXT NOT NULL DEFAULT 'api_key',\
                label TEXT NOT NULL DEFAULT '',\
                priority INTEGER NOT NULL DEFAULT 0,\
                enabled INTEGER NOT NULL DEFAULT 1,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at INTEGER,\
                updated_at INTEGER\
            );\
            CREATE TABLE endpoint_keys (\
                id TEXT PRIMARY KEY,\
                name TEXT NOT NULL DEFAULT '',\
                key TEXT NOT NULL UNIQUE,\
                created_at INTEGER,\
                last_used_at INTEGER\
            );",
        ),
        // Legacy providers/accounts (label + transcript-estimated quota only,
        // no credentials) superseded by provider_connections — approved
        // destructive drop, spec §4.2.
        M::up("DROP TABLE providers; DROP TABLE provider_accounts;"),
        // F2a usage tracking (design: docs/design/2026-07-04-models-runtime-f2-design.md §3.2):
        // request_log = one row per served request (pruned to 30 days);
        // usage_daily = permanent rollup for charts.
        M::up(
            "CREATE TABLE request_log (\
                id TEXT PRIMARY KEY,\
                ts INTEGER NOT NULL,\
                connection_id TEXT NOT NULL,\
                provider TEXT NOT NULL,\
                model TEXT NOT NULL,\
                client_format TEXT NOT NULL,\
                input_tokens INTEGER NOT NULL DEFAULT 0,\
                output_tokens INTEGER NOT NULL DEFAULT 0,\
                status_code INTEGER NOT NULL,\
                duration_ms INTEGER NOT NULL,\
                error TEXT\
            );\
            CREATE INDEX idx_request_log_ts ON request_log(ts);\
            CREATE INDEX idx_request_log_conn ON request_log(connection_id, ts);\
            CREATE TABLE usage_daily (\
                day TEXT NOT NULL,\
                connection_id TEXT NOT NULL,\
                model TEXT NOT NULL,\
                requests INTEGER NOT NULL DEFAULT 0,\
                input_tokens INTEGER NOT NULL DEFAULT 0,\
                output_tokens INTEGER NOT NULL DEFAULT 0,\
                PRIMARY KEY (day, connection_id, model)\
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

#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub connection_id: String,
    pub provider: String,
    pub model: String,
    pub client_format: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub status_code: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageDayRow {
    pub day: String,
    pub connection_id: String,
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageTotalRow {
    pub connection_id: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// UTC calendar day (YYYY-MM-DD) for a millisecond timestamp.
fn day_of(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

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
        conn.interact(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO settings(key, value) VALUES ('enabled_gateways', 'discord');\
                 INSERT OR IGNORE INTO settings(key, value) VALUES ('enabled_runtimes', 'claude-code');\
                 INSERT OR IGNORE INTO settings(key, value) VALUES ('default_runtime', 'claude-code');",
            )
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
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    s.session_pk, s.project_id, s.agent_session_id, s.worktree_path,
                    s.branch, s.title, s.status.as_str(), s.created_at, s.last_active,
                    s.started_by, s.resume_attempts
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

    /// List sessions in a given status, oldest-first — used by `reconcile` on
    /// daemon boot to find sessions a dead process left in `Running`.
    pub async fn list_sessions_by_status(
        &self,
        status: SessionStatus,
    ) -> anyhow::Result<Vec<Session>> {
        let status = status.as_str().to_string();
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(move |c| -> rusqlite::Result<Vec<Session>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {SESSION_COLS} FROM sessions WHERE status=?1 ORDER BY created_at"
                ))?;
                let items = stmt
                    .query_map(params![status], row_to_session)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(items)
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
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

    /// Update per-project preferences; `None` leaves the column untouched.
    pub async fn update_project_prefs(
        &self,
        project_id: &str,
        model: Option<&str>,
        effort: Option<&str>,
        perm_mode: Option<PermMode>,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let model = model.map(|s| s.to_string());
        let effort = effort.map(|s| s.to_string());
        let perm = perm_mode.map(|m| m.as_str().to_string());
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE projects SET model=COALESCE(?2, model), effort=COALESCE(?3, effort), \
                 perm_mode=COALESCE(?4, perm_mode) WHERE project_id=?1",
                params![project_id, model, effort, perm],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// Atomically demote `Running → Idle` only if the current status is still `Running`.
    /// A session already marked `Interrupted` or `Ended` is left untouched.
    /// Also resets `resume_attempts` to 0 — a turn that reaches a normal (or
    /// errored-but-demoted) end clears the auto-resume cap (TS `runHarness`
    /// finally parity).
    pub async fn demote_if_running(&self, pk: &str, last_active: i64) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, last_active=?3, resume_attempts = 0 WHERE session_pk=?1 AND status=?4",
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

    /// Set `status` and `resume_attempts` together — used by `resume_session`
    /// to atomically bump the attempt counter as it re-drives a turn.
    pub async fn update_resume(
        &self,
        pk: &str,
        status: SessionStatus,
        resume_attempts: i64,
    ) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, resume_attempts=?3 WHERE session_pk=?1",
                params![pk, status.as_str(), resume_attempts],
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

    /// Return the raw persisted value for `key`, or `None` if no row exists.
    /// No defaults are applied here — that's the caller's job.
    pub async fn get_setting_raw(&self, key: &str) -> anyhow::Result<Option<String>> {
        let key = key.to_string();
        let conn = self.pool.get().await?;
        let result = conn
            .interact(move |c| {
                c.query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(result)
    }

    /// Upsert a raw setting value. No validation is performed.
    pub async fn set_setting_raw(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// List all persisted settings rows.
    pub async fn list_settings(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(|c| -> rusqlite::Result<Vec<(String, String)>> {
                let mut stmt = c.prepare("SELECT key, value FROM settings")?;
                let items = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(items)
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
    }

    /// Bind a gateway conversation to a session (upsert on the `(gateway,
    /// conversation_id)` primary key).
    pub async fn add_surface(
        &self,
        gateway: &str,
        conversation_id: &str,
        session_pk: &str,
    ) -> anyhow::Result<()> {
        let gateway = gateway.to_string();
        let conversation_id = conversation_id.to_string();
        let session_pk = session_pk.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO session_surfaces(gateway, conversation_id, session_pk) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(gateway, conversation_id) DO UPDATE SET session_pk = excluded.session_pk",
                params![gateway, conversation_id, session_pk],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// List the gateway surfaces bound to a session.
    pub async fn surfaces(&self, session_pk: &str) -> anyhow::Result<Vec<Surface>> {
        let session_pk = session_pk.to_string();
        let conn = self.pool.get().await?;
        let rows = conn
            .interact(move |c| -> rusqlite::Result<Vec<Surface>> {
                let mut stmt = c.prepare(
                    "SELECT gateway, conversation_id FROM session_surfaces WHERE session_pk = ?1",
                )?;
                let items = stmt
                    .query_map(params![session_pk], |r| {
                        Ok(Surface {
                            gateway: r.get(0)?,
                            conversation_id: r.get(1)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(items)
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(rows)
    }

    /// Resolve the session bound to a `(gateway, conversation_id)` surface, if any.
    pub async fn resolve_by_conversation(
        &self,
        gateway: &str,
        conversation_id: &str,
    ) -> anyhow::Result<Option<Session>> {
        let gateway = gateway.to_string();
        let conversation_id = conversation_id.to_string();
        let conn = self.pool.get().await?;
        let s = conn
            .interact(move |c| {
                c.query_row(
                    &format!(
                        "SELECT s.{SESSION_COLS} FROM sessions s \
                         JOIN session_surfaces sf ON sf.session_pk = s.session_pk \
                         WHERE sf.gateway = ?1 AND sf.conversation_id = ?2"
                    ),
                    params![gateway, conversation_id],
                    row_to_session,
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(s)
    }

    /// Bind a gateway workspace to a project (upsert on the `(gateway,
    /// workspace_id)` primary key).
    pub async fn bind_project(
        &self,
        gateway: &str,
        workspace_id: &str,
        project_id: &str,
    ) -> anyhow::Result<()> {
        let gateway = gateway.to_string();
        let workspace_id = workspace_id.to_string();
        let project_id = project_id.to_string();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO project_bindings(gateway, workspace_id, project_id) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(gateway, workspace_id) DO UPDATE SET project_id = excluded.project_id",
                params![gateway, workspace_id, project_id],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(())
    }

    /// Resolve the project bound to a `(gateway, workspace_id)` binding, if any.
    pub async fn resolve_project_by_workspace(
        &self,
        gateway: &str,
        workspace_id: &str,
    ) -> anyhow::Result<Option<Project>> {
        let gateway = gateway.to_string();
        let workspace_id = workspace_id.to_string();
        let conn = self.pool.get().await?;
        let p = conn
            .interact(move |c| {
                c.query_row(
                    &format!(
                        "SELECT p.{PROJECT_COLS} FROM projects p \
                         JOIN project_bindings b ON b.project_id = p.project_id \
                         WHERE b.gateway = ?1 AND b.workspace_id = ?2"
                    ),
                    params![gateway, workspace_id],
                    row_to_project,
                )
                .optional()
            })
            .await
            .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
        Ok(p)
    }

    /// Insert one request_log row and upsert its usage_daily rollup atomically.
    pub async fn record_request(&self, r: UsageRecord) -> anyhow::Result<()> {
        let day = day_of(r.ts);
        let id = crate::paths::new_id();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO request_log(id,ts,connection_id,provider,model,client_format,\
                 input_tokens,output_tokens,status_code,duration_ms,error) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![id, r.ts, r.connection_id, r.provider, r.model, r.client_format,
                    r.input_tokens, r.output_tokens, r.status_code, r.duration_ms, r.error],
            )?;
            tx.execute(
                "INSERT INTO usage_daily(day,connection_id,model,requests,input_tokens,output_tokens) \
                 VALUES (?1,?2,?3,1,?4,?5) \
                 ON CONFLICT(day,connection_id,model) DO UPDATE SET \
                   requests=requests+1, \
                   input_tokens=input_tokens+excluded.input_tokens, \
                   output_tokens=output_tokens+excluded.output_tokens",
                params![day, r.connection_id, r.model, r.input_tokens, r.output_tokens],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Daily usage rollup rows, optionally filtered to one connection, from
    /// `since_day` (inclusive, `YYYY-MM-DD`) onward.
    pub async fn usage_daily(
        &self,
        connection_id: Option<&str>,
        since_day: &str,
    ) -> anyhow::Result<Vec<UsageDayRow>> {
        let conn_filter = connection_id.map(|s| s.to_string());
        let since = since_day.to_string();
        self.with_conn(move |c| {
            let sql = "SELECT day,connection_id,model,requests,input_tokens,output_tokens \
                       FROM usage_daily WHERE day >= ?1 \
                       AND (?2 IS NULL OR connection_id = ?2) \
                       ORDER BY day ASC";
            let mut stmt = c.prepare(sql)?;
            let rows = stmt
                .query_map(params![since, conn_filter], |r| {
                    Ok(UsageDayRow {
                        day: r.get(0)?, connection_id: r.get(1)?, model: r.get(2)?,
                        requests: r.get(3)?, input_tokens: r.get(4)?, output_tokens: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Per-connection request/token totals for one UTC day.
    pub async fn today_totals(&self, day: &str) -> anyhow::Result<Vec<UsageTotalRow>> {
        let day = day.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT connection_id, SUM(requests), SUM(input_tokens), SUM(output_tokens) \
                 FROM usage_daily WHERE day = ?1 GROUP BY connection_id",
            )?;
            let rows = stmt
                .query_map(params![day], |r| {
                    Ok(UsageTotalRow {
                        connection_id: r.get(0)?, requests: r.get(1)?,
                        input_tokens: r.get(2)?, output_tokens: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Count of request_log rows with `ts >= since_ms` — overall (F2a has no
    /// per-key routing identity, so this isn't per-key).
    pub async fn total_requests_since(&self, since_ms: i64) -> anyhow::Result<i64> {
        self.with_conn(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM request_log WHERE ts >= ?1",
                params![since_ms],
                |r| r.get::<_, i64>(0),
            )
        })
        .await
    }

    /// Delete request_log rows older than `older_than_ms`; usage_daily
    /// rollups are untouched (they're the permanent record for charts).
    pub async fn prune_request_log(&self, older_than_ms: i64) -> anyhow::Result<usize> {
        self.with_conn(move |c| {
            let n = c.execute(
                "DELETE FROM request_log WHERE ts < ?1",
                params![older_than_ms],
            )?;
            Ok(n)
        })
        .await
    }
}

const SESSION_COLS: &str =
    "session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts";

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
        started_by: r.get(9)?,
        resume_attempts: r.get(10)?,
    })
}

/// Spec 4 §6 clean break: a database written by the retired TypeScript stack
/// (marker: `settings` table present, `messages` absent) is moved aside to
/// `<name>.pre-rust.bak` so `Store::open` starts fresh. No migration.
pub fn quarantine_legacy_db(db_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_table = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
    };
    let legacy = has_table("settings")? && !has_table("messages")?;
    drop(conn);
    if !legacy {
        return Ok(None);
    }
    let backup = db_path.with_extension("sqlite.pre-rust.bak");
    std::fs::rename(db_path, &backup)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if side.exists() {
            let _ = std::fs::rename(&side, format!("{}{suffix}", backup.display()));
        }
    }
    Ok(Some(backup))
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
            started_by: None,
            created_at: Some(1),
            last_active: Some(1),
            resume_attempts: 0,
        }
    }

    #[tokio::test]
    async fn messages_get_monotonic_per_session_seq_and_list_in_order() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        let a1 = store
            .insert_message(NewMessage::block(
                "s1",
                "user",
                "text",
                serde_json::json!({"text": "hi"}),
            ))
            .await
            .unwrap();
        let a2 = store
            .insert_message(NewMessage::block(
                "s1",
                "assistant",
                "text",
                serde_json::json!({"text": "hello"}),
            ))
            .await
            .unwrap();
        // A different session has an INDEPENDENT seq sequence starting at 1.
        let b1 = store
            .insert_message(NewMessage::block(
                "s2",
                "user",
                "text",
                serde_json::json!({"text": "yo"}),
            ))
            .await
            .unwrap();

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

        store
            .insert_message(NewMessage {
                session_pk: "s1".into(),
                role: "assistant".into(),
                block_type: "tool_call".into(),
                payload: serde_json::json!({"name": "Bash", "input": {"command": "ls"}}),
                tool_call_id: Some("tc-1".into()),
                status: Some("pending".into()),
                tool_kind: Some("execute".into()),
            })
            .await
            .unwrap();

        // The caller now sends ONLY the update patch; the store merges it.
        let (seq, merged, kind) = store
            .update_tool_call(
                "s1",
                "tc-1",
                Some("completed"),
                &serde_json::json!({"output": "file.txt"}),
            )
            .await
            .unwrap();
        assert_eq!(seq, 1, "update_tool_call must return the row's real seq");
        assert_eq!(
            merged["name"], "Bash",
            "merge must preserve the original name"
        );
        assert_eq!(
            merged["input"]["command"], "ls",
            "merge must preserve the original input"
        );
        assert_eq!(merged["output"], "file.txt", "merge must add the output");
        assert_eq!(
            kind.as_deref(),
            Some("execute"),
            "must return the row's persisted tool_kind"
        );

        let rows = store.list_messages("s1").await.unwrap();
        assert_eq!(rows.len(), 1, "update must not insert a new row");
        assert_eq!(rows[0].status.as_deref(), Some("completed"));
        assert_eq!(rows[0].payload["name"], "Bash");
        assert_eq!(rows[0].payload["output"], "file.txt");

        // An empty patch (ToolCallDone with no raw_output) must leave payload intact.
        let (_, merged2, _) = store
            .update_tool_call("s1", "tc-1", None, &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(merged2["name"], "Bash");
        assert_eq!(merged2["output"], "file.txt");
    }

    #[tokio::test]
    async fn update_tool_call_errors_when_row_absent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let res = store
            .update_tool_call(
                "s1",
                "missing-tc",
                Some("completed"),
                &serde_json::json!({}),
            )
            .await;
        assert!(
            res.is_err(),
            "updating a nonexistent tool_call_id must error"
        );
    }

    #[tokio::test]
    async fn tool_policy_is_per_project_and_upserts() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // initially no policy
        assert!(store.get_tool_policy("p1", "Bash").await.unwrap().is_none());
        // set a policy
        store
            .set_tool_policy("p1", "Bash", "allowAlways")
            .await
            .unwrap();
        assert_eq!(
            store
                .get_tool_policy("p1", "Bash")
                .await
                .unwrap()
                .as_deref(),
            Some("allowAlways")
        );
        // different project is independent
        assert!(store.get_tool_policy("p2", "Bash").await.unwrap().is_none());
        // upsert (update) the existing policy
        store
            .set_tool_policy("p1", "Bash", "rejectAlways")
            .await
            .unwrap();
        assert_eq!(
            store
                .get_tool_policy("p1", "Bash")
                .await
                .unwrap()
                .as_deref(),
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

    #[tokio::test]
    async fn quarantine_moves_ts_schema_db_aside() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ryuzi.sqlite");
        // TS schema marker: `settings` table exists, `messages` does not.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        drop(conn);

        let moved = quarantine_legacy_db(&db).unwrap();
        let backup = dir.path().join("ryuzi.sqlite.pre-rust.bak");
        assert_eq!(moved, Some(backup.clone()));
        assert!(!db.exists());
        assert!(backup.exists());
    }

    #[tokio::test]
    async fn update_project_prefs_coalesces_none_fields() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let mut p = sample_project();
        p.model = Some("opus".into());
        store.insert_project(p).await.unwrap();

        store
            .update_project_prefs("p1", None, Some("high"), Some(PermMode::BypassPermissions))
            .await
            .unwrap();
        let got = store.get_project("p1").await.unwrap().unwrap();
        assert_eq!(got.model.as_deref(), Some("opus")); // None left it untouched
        assert_eq!(got.effort.as_deref(), Some("high"));
        assert_eq!(got.perm_mode, PermMode::BypassPermissions);
    }

    #[tokio::test]
    async fn quarantine_leaves_rust_schema_and_missing_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ryuzi.sqlite");
        assert_eq!(quarantine_legacy_db(&db).unwrap(), None); // missing file

        // Rust schema (has `messages`): untouched even if `settings` appears later (4B superset).
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE messages (session_pk TEXT, seq INTEGER);",
        )
        .unwrap();
        drop(conn);
        assert_eq!(quarantine_legacy_db(&db).unwrap(), None);
        assert!(db.exists());
    }

    #[tokio::test]
    async fn settings_raw_roundtrip_and_seeds() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // Seeds applied on open:
        assert_eq!(
            store
                .get_setting_raw("enabled_gateways")
                .await
                .unwrap()
                .as_deref(),
            Some("discord")
        );
        assert_eq!(
            store
                .get_setting_raw("enabled_runtimes")
                .await
                .unwrap()
                .as_deref(),
            Some("claude-code")
        );
        assert_eq!(
            store
                .get_setting_raw("default_runtime")
                .await
                .unwrap()
                .as_deref(),
            Some("claude-code")
        );
        // Upsert + empty string is a real value:
        store
            .set_setting_raw("workdir_root", "/repos")
            .await
            .unwrap();
        store.set_setting_raw("workdir_root", "").await.unwrap();
        assert_eq!(
            store
                .get_setting_raw("workdir_root")
                .await
                .unwrap()
                .as_deref(),
            Some("")
        );
        let listed = store.list_settings().await.unwrap();
        assert!(listed
            .iter()
            .any(|(k, v)| k == "workdir_root" && v.is_empty()));
    }

    #[tokio::test]
    async fn surfaces_and_bindings_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        let mut s = sample_session();
        s.started_by = Some("u42".into());
        store.insert_session(s).await.unwrap();

        store.add_surface("discord", "chan1", "s1").await.unwrap();
        store.add_surface("discord", "chan1", "s1").await.unwrap(); // upsert, no error
        let surfaces = store.surfaces("s1").await.unwrap();
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].gateway, "discord");
        assert_eq!(surfaces[0].conversation_id, "chan1");
        let resolved = store
            .resolve_by_conversation("discord", "chan1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.session_pk, "s1");
        assert_eq!(resolved.started_by.as_deref(), Some("u42"));
        assert_eq!(resolved.resume_attempts, 0);

        store.bind_project("discord", "guild1", "p1").await.unwrap();
        let proj = store
            .resolve_project_by_workspace("discord", "guild1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proj.project_id, "p1");
        assert!(store
            .resolve_project_by_workspace("discord", "nope")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn migration_5_upgrades_a_v4_database() {
        // A DB created before this task (user_version 4) must upgrade in place.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            // Minimal v4 shape: the 4 old tables + user_version 4.
            conn.execute_batch(
                "CREATE TABLE projects (project_id TEXT PRIMARY KEY, name TEXT, workdir TEXT NOT NULL, source TEXT, harness TEXT NOT NULL DEFAULT 'claude-code', model TEXT, effort TEXT, perm_mode TEXT NOT NULL DEFAULT 'default', created_at INTEGER);
                 CREATE TABLE sessions (session_pk TEXT PRIMARY KEY, project_id TEXT NOT NULL, agent_session_id TEXT, worktree_path TEXT, branch TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'idle', created_at INTEGER, last_active INTEGER);
                 CREATE TABLE messages (session_pk TEXT NOT NULL, seq INTEGER NOT NULL, role TEXT NOT NULL, block_type TEXT NOT NULL, payload TEXT NOT NULL, tool_call_id TEXT, status TEXT, tool_kind TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (session_pk, seq));
                 CREATE TABLE tool_policies (project_id TEXT NOT NULL, tool TEXT NOT NULL, decision TEXT NOT NULL, PRIMARY KEY (project_id, tool));
                 INSERT INTO sessions(session_pk, project_id) VALUES ('old1', 'p1');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let store = Store::open(tmp.path()).await.unwrap();
        let s = store.get_session("old1").await.unwrap().unwrap();
        assert_eq!(s.resume_attempts, 0); // ALTER default applied
        assert_eq!(s.started_by, None);
        assert_eq!(
            store
                .get_setting_raw("enabled_gateways")
                .await
                .unwrap()
                .as_deref(),
            Some("discord")
        );
    }

    #[tokio::test]
    async fn usage_record_writes_log_and_rolls_up_daily() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let base = 1_700_000_000_000_i64; // fixed ms -> a stable UTC day
        let day = super::day_of(base);

        store
            .record_request(UsageRecord {
                connection_id: "c1".into(), provider: "openai".into(), model: "gpt-x".into(),
                client_format: "openai".into(), input_tokens: 10, output_tokens: 5,
                status_code: 200, duration_ms: 42, error: None, ts: base,
            })
            .await
            .unwrap();
        store
            .record_request(UsageRecord {
                connection_id: "c1".into(), provider: "openai".into(), model: "gpt-x".into(),
                client_format: "openai".into(), input_tokens: 7, output_tokens: 3,
                status_code: 200, duration_ms: 30, error: None, ts: base + 1000,
            })
            .await
            .unwrap();

        let rows = store.usage_daily(Some("c1"), &day).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 2);
        assert_eq!(rows[0].input_tokens, 17);
        assert_eq!(rows[0].output_tokens, 8);

        let totals = store.today_totals(&day).await.unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].connection_id, "c1");
        assert_eq!(totals[0].requests, 2);

        assert_eq!(store.total_requests_since(base).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn prune_deletes_old_request_log_but_keeps_daily() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let old = 1_000_000_000_000_i64;
        store
            .record_request(UsageRecord {
                connection_id: "c1".into(), provider: "p".into(), model: "m".into(),
                client_format: "openai".into(), input_tokens: 1, output_tokens: 1,
                status_code: 200, duration_ms: 1, error: None, ts: old,
            })
            .await
            .unwrap();
        let removed = store.prune_request_log(old + 1).await.unwrap();
        assert_eq!(removed, 1);
        // rollup survives the prune
        let rows = store
            .usage_daily(Some("c1"), &super::day_of(old))
            .await
            .unwrap();
        assert_eq!(rows[0].requests, 1);
    }
}
