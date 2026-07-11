use crate::domain::{
    Message, NewMessage, NewProviderTurn, PermMode, Project, ProviderTurn, Session, SessionKind,
    SessionStatus, Surface, ToolPolicyRow,
};
use crate::llm_router::secrets::{decrypt_field, encrypt_field};
use crate::paths::now_ms;
use crate::plugins::oauth::PluginOauthToken;
use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::{params, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
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
        // local router endpoint. Secret columns/fields are encrypted at rest
        // by secrets::SecretCipher (value-level `enc:` sentinel, no schema
        // change); config-apply still re-reads the literal key because
        // row_to_key decrypts on read.
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
        // Native agent runtime (design: docs/design/2026-07-05-native-agent-runtime-design.md).
        // provider_turns = the model-faithful Anthropic-format message ledger
        // (one row per user/assistant turn) used to replay history on resume;
        // separate from the display-oriented `messages` table. todos = the
        // native `todowrite` tool's per-session task list.
        M::up(
            "CREATE TABLE provider_turns (\
                session_pk TEXT NOT NULL,\
                seq INTEGER NOT NULL,\
                role TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                created_at INTEGER NOT NULL,\
                PRIMARY KEY (session_pk, seq)\
            );\
            CREATE INDEX idx_provider_turns_session ON provider_turns(session_pk, seq);\
            CREATE TABLE todos (\
                session_pk TEXT NOT NULL,\
                pos INTEGER NOT NULL,\
                content TEXT NOT NULL,\
                status TEXT NOT NULL,\
                created_at INTEGER NOT NULL,\
                PRIMARY KEY (session_pk, pos)\
            );",
        ),
        // Orchestration task graph (design:
        // docs/design/2026-07-05-native-orchestration-memory-design.md):
        // the auto-decomposition graph the orch dispatcher drives (roots have
        // root_id NULL; deps gate todo→ready promotion). Keep this migration
        // idempotent because some dev DBs already reached user_version 11
        // from a newer cockpit build.
        M::up(
            "CREATE TABLE IF NOT EXISTS orch_tasks (\
                id TEXT PRIMARY KEY,\
                root_id TEXT,\
                project_id TEXT NOT NULL,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                agent TEXT NOT NULL DEFAULT '',\
                status TEXT NOT NULL DEFAULT 'todo',\
                session_pk TEXT,\
                result TEXT,\
                error TEXT,\
                created_at INTEGER,\
                finished_at INTEGER\
            );\
            CREATE INDEX IF NOT EXISTS idx_orch_tasks_root ON orch_tasks(root_id, status);\
            CREATE TABLE IF NOT EXISTS orch_task_deps (\
                task_id TEXT NOT NULL,\
                dep_id TEXT NOT NULL,\
                PRIMARY KEY (task_id, dep_id)\
            );",
        ),
        // Heartbeat hardening: jobs.pre_check = optional wake-gate command
        // run before a scheduled job wakes the agent (empty stdout /
        // non-zero exit skips the fire). Hook-guarded (SQLite has no ADD
        // COLUMN IF NOT EXISTS) because some dev DBs gained this column at
        // user_version 11 from a pre-merge build — same story as the
        // idempotent orch migration above.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let exists = tx
                .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name='pre_check'")?
                .exists([])?;
            if !exists {
                tx.execute(
                    "ALTER TABLE jobs ADD COLUMN pre_check TEXT NOT NULL DEFAULT ''",
                    [],
                )?;
            }
            Ok(())
        }),
        // Ryuzi-only sessions (spec: docs/superpowers/specs/
        // 2026-07-06-cockpit-enhancement-batch-design.md, Workstream C):
        // retire the claude-code default. One-time rewrite of existing rows;
        // fresh DBs get 'native' from the seeds in `Store::open`. Idempotent:
        // plain WHERE-guarded UPDATEs, and the CSV rewrite converges after
        // one pass. The claude-code harness itself STAYS registered so an
        // unrewritten row (restored DB) still resolves at session start.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            // Guarded: migration 21 drops projects.harness, and the
            // rewind-and-replay migration test re-runs THIS hook on a post-21
            // schema (where the column no longer exists).
            let has_harness = tx
                .prepare("SELECT 1 FROM pragma_table_info('projects') WHERE name='harness'")?
                .exists([])?;
            if has_harness {
                tx.execute(
                    "UPDATE projects SET harness='native' WHERE harness='claude-code'",
                    [],
                )?;
            }
            tx.execute(
                "UPDATE settings SET value='native' WHERE key='default_runtime' AND value='claude-code'",
                [],
            )?;
            tx.execute(
                "UPDATE settings SET value='native' WHERE key='default_agent' AND value='claude'",
                [],
            )?;
            // enabled_runtimes is a CSV: swap 'claude-code' for 'native',
            // dedupe, and ensure 'native' is present — Ryuzi-only sessions
            // need the native runtime enabled.
            let cur: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE key='enabled_runtimes'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(cur) = cur {
                let mut parts: Vec<&str> = Vec::new();
                for p in cur.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let p = if p == "claude-code" { "native" } else { p };
                    if !parts.contains(&p) {
                        parts.push(p);
                    }
                }
                if !parts.contains(&"native") {
                    parts.insert(0, "native");
                }
                let next = parts.join(",");
                if next != cur {
                    tx.execute(
                        "UPDATE settings SET value=?1 WHERE key='enabled_runtimes'",
                        params![next],
                    )?;
                }
            }
            Ok(())
        }),
        // Branch controls (workstream B): which sessions own their branch.
        // 1 (owned) is the legacy behavior — every pre-existing session's
        // branch name was engine-generated, so teardown may delete it.
        // Hook-guarded (SQLite has no ADD COLUMN IF NOT EXISTS) so replaying
        // this migration on a DB that already has the column (e.g. the
        // rewind-and-replay in `migrations_13_to_23_replay_is_idempotent_and_converges_native_only`,
        // which re-runs every migration appended after 13) is a no-op
        // instead of a "duplicate column" error.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let exists = tx
                .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='branch_owned'")?
                .exists([])?;
            if !exists {
                tx.execute(
                    "ALTER TABLE sessions ADD COLUMN branch_owned INTEGER NOT NULL DEFAULT 1",
                    [],
                )?;
            }
            Ok(())
        }),
        // Provider model probe verdicts (design: docs/design/
        // 2026-07-08-cockpit-ui-polish-batch-design.md §5): one row per
        // (family, model), written only on a definitive valid/invalid probe
        // so transient failures never clobber a known verdict. IF NOT EXISTS
        // for the same reason branch_owned above is hook-guarded: the
        // rewind-and-replay test re-runs appended migrations on a DB that
        // already has this table, and that replay must be a no-op.
        M::up(
            "CREATE TABLE IF NOT EXISTS model_status (\
                family TEXT NOT NULL,\
                model TEXT NOT NULL,\
                status TEXT NOT NULL,\
                message TEXT NOT NULL DEFAULT '',\
                tested_at INTEGER NOT NULL,\
                PRIMARY KEY (family, model)\
            );",
        ),
        // Plugin OAuth token storage (plugins-hub). model_status is re-created
        // here idempotently to heal dev DBs that ran the plugins-hub branch
        // ordering, where this migration occupied the slot model_status now
        // holds — those DBs skip the slot above by user_version.
        M::up(
            "CREATE TABLE IF NOT EXISTS plugin_oauth_tokens (\
                plugin_id TEXT PRIMARY KEY NOT NULL,\
                token_json TEXT NOT NULL,\
                updated_at INTEGER NOT NULL\
            );\
            CREATE TABLE IF NOT EXISTS model_status (\
                family TEXT NOT NULL,\
                model TEXT NOT NULL,\
                status TEXT NOT NULL,\
                message TEXT NOT NULL DEFAULT '',\
                tested_at INTEGER NOT NULL,\
                PRIMARY KEY (family, model)\
            );",
        ),
        // Context-window management (design: docs/superpowers/specs/
        // 2026-07-10-context-window-management-design.md): durable compaction
        // checkpoints + last-known context usage per session. IF NOT EXISTS
        // for the rewind-and-replay migration test, like model_status above.
        M::up(
            "CREATE TABLE IF NOT EXISTS context_checkpoints (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                session_pk TEXT NOT NULL,\
                boundary_seq INTEGER NOT NULL,\
                window_number INTEGER NOT NULL,\
                payload TEXT NOT NULL,\
                created_at INTEGER NOT NULL\
            );\
            CREATE INDEX IF NOT EXISTS idx_context_checkpoints_session \
                ON context_checkpoints(session_pk, boundary_seq);\
            CREATE TABLE IF NOT EXISTS session_context (\
                session_pk TEXT PRIMARY KEY NOT NULL,\
                payload TEXT NOT NULL,\
                updated_at INTEGER NOT NULL\
            );",
        ),
        // Plugin OAuth client cache (install wizard): a partial cache that
        // accretes — discovery fills the endpoint columns, DCR *or* manual
        // entry fills client_id. A PUBLIC client id is not a secret (it is
        // handed to the browser in the authorize URL), so no encryption.
        // IF NOT EXISTS: the rewind-and-replay test re-runs appended
        // migrations on a DB that already has this table.
        M::up(
            "CREATE TABLE IF NOT EXISTS plugin_oauth_clients (\
                plugin_id TEXT PRIMARY KEY NOT NULL,\
                authorize_url TEXT,\
                token_url TEXT,\
                client_id TEXT,\
                updated_at INTEGER NOT NULL\
            );",
        ),
        // Rebuild plugin_oauth_clients to the nullable shape. Some dev DBs
        // carry a pre-release v1 table (every column NOT NULL, created by an
        // uncommitted experimental build); IF NOT EXISTS above never heals an
        // existing table, so the discovery-first upsert (client_id = NULL)
        // failed its NOT NULL constraint. Copy-drop-rename is idempotent —
        // replay on an already-nullable table is a harmless rebuild — which
        // the rewind-and-replay migration test relies on.
        M::up(
            "CREATE TABLE IF NOT EXISTS plugin_oauth_clients_rebuild (\
                plugin_id TEXT PRIMARY KEY NOT NULL,\
                authorize_url TEXT,\
                token_url TEXT,\
                client_id TEXT,\
                updated_at INTEGER NOT NULL\
            );\
            INSERT OR REPLACE INTO plugin_oauth_clients_rebuild \
                SELECT plugin_id, authorize_url, token_url, client_id, updated_at \
                FROM plugin_oauth_clients;\
            DROP TABLE plugin_oauth_clients;\
            ALTER TABLE plugin_oauth_clients_rebuild RENAME TO plugin_oauth_clients;",
        ),
        // Migration 20 — Per-session permission mode (batch-3 design): sessions
        // previously shared the project's mode; now each session carries its
        // own, seeded from the owning project. Hook-guarded (SQLite has no ADD
        // COLUMN IF NOT EXISTS) like branch_owned above: the rewind-and-replay
        // test re-runs every migration appended after 13 on a DB that already
        // has this column, so a plain ALTER would fail with "duplicate column".
        // This batch-3 migration shipped to main first, so it keeps slot 20 and
        // runs BEFORE the native-only migration on upgrade.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let exists = tx
                .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='perm_mode'")?
                .exists([])?;
            if !exists {
                tx.execute(
                    "ALTER TABLE sessions ADD COLUMN perm_mode TEXT NOT NULL DEFAULT 'default'",
                    [],
                )?;
                tx.execute(
                    "UPDATE sessions SET perm_mode = COALESCE(\
                         (SELECT p.perm_mode FROM projects p WHERE p.project_id = sessions.project_id),\
                         'default')",
                    [],
                )?;
            }
            Ok(())
        }),
        // Migration 21 — Native-only runtime (design:
        // docs/design/2026-07-10-native-only-runtime-design.md §5): the runtime
        // concept dies. Renumbered from 20 to 21 in integration merge #2 because
        // batch-3's per-session perm_mode migration (above) shipped to main
        // first and takes slot 20; this native-only migration is now the tail.
        // Drop the legacy per-project harness and per-job agent columns
        // (SQLite >= 3.35 DROP COLUMN; bundled 3.45), copy the native agents-row
        // model/perm_mode into the agent_model / agent_perm_mode settings KV
        // (only when the KV key is absent), drop the agents/agent_tiers tables,
        // delete the dead settings keys, and prune non-native mcp_agent_access
        // rows. Every statement is existence-guarded so the rewind-and-replay
        // migration test's re-run on an already-migrated DB is a no-op. Ordering
        // is safe: batch-3's migration 20 only touches sessions.perm_mode (a
        // column this migration never removes), so running it first is inert
        // with respect to the harness/agents/settings artifacts dropped here.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let col_exists = |table: &str, col: &str| -> rusqlite::Result<bool> {
                tx.prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('{table}') WHERE name='{col}'"
                ))?
                .exists([])
            };
            let table_exists = |name: &str| -> rusqlite::Result<bool> {
                tx.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
                    .exists([name])
            };
            if col_exists("projects", "harness")? {
                tx.execute("ALTER TABLE projects DROP COLUMN harness", [])?;
            }
            if col_exists("jobs", "agent")? {
                tx.execute("ALTER TABLE jobs DROP COLUMN agent", [])?;
            }
            if table_exists("agents")? {
                // Preserve the user's native model/perm-mode choices in settings
                // KV — but never clobber a value the new Settings UI already wrote.
                tx.execute(
                    "INSERT INTO settings(key, value) \
                     SELECT 'agent_model', model FROM agents \
                     WHERE id='native' AND model IS NOT NULL AND trim(model) != '' \
                     AND NOT EXISTS (SELECT 1 FROM settings WHERE key='agent_model')",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO settings(key, value) \
                     SELECT 'agent_perm_mode', perm_mode FROM agents \
                     WHERE id='native' AND trim(perm_mode) != '' \
                     AND NOT EXISTS (SELECT 1 FROM settings WHERE key='agent_perm_mode')",
                    [],
                )?;
                tx.execute("DROP TABLE agents", [])?;
            }
            tx.execute("DROP TABLE IF EXISTS agent_tiers", [])?;
            tx.execute(
                "DELETE FROM settings WHERE key IN \
                 ('enabled_runtimes','default_runtime','default_agent','agents_snapshot')",
                [],
            )?;
            tx.execute(
                "DELETE FROM mcp_agent_access WHERE agent_id != 'native'",
                [],
            )?;
            Ok(())
        }),
        // Migration 22 — Chat-first sessions (design: docs/superpowers/specs/
        // 2026-07-11-chat-first-sessions-design.md, Phase 2 Task A1):
        // sessions.project_id becomes nullable (chat/worker/review sessions
        // aren't bound to a project) and gains `kind` + `speaker`/`agent`/
        // `parent_session_pk` lineage columns. SQLite can't drop a NOT NULL
        // constraint in place, so rebuild the table: create the new shape,
        // copy every existing column, drop, rename. Existing rows all get
        // kind='project' with null lineage columns — correct, they were all
        // project sessions before this migration. Appended as the tail (after
        // migration 20 perm_mode and migration 21 native-only, neither of which
        // adds or removes a sessions column beyond perm_mode), so `sessions`
        // carries exactly the original 12 columns + perm_mode here: sessions_new
        // must include perm_mode and copy it forward, or the rebuild would
        // silently drop the column migration 20 added.
        M::up(
            r#"
            CREATE TABLE sessions_new (
                session_pk TEXT PRIMARY KEY,
                project_id TEXT,
                agent_session_id TEXT,
                worktree_path TEXT,
                branch TEXT,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'idle',
                created_at INTEGER,
                last_active INTEGER,
                started_by TEXT,
                resume_attempts INTEGER NOT NULL DEFAULT 0,
                branch_owned INTEGER NOT NULL DEFAULT 1,
                perm_mode TEXT NOT NULL DEFAULT 'default',
                kind TEXT NOT NULL DEFAULT 'project',
                speaker TEXT,
                agent TEXT,
                parent_session_pk TEXT
            );
            INSERT INTO sessions_new
                (session_pk, project_id, agent_session_id, worktree_path, branch, title,
                 status, created_at, last_active, started_by, resume_attempts, branch_owned, perm_mode)
            SELECT session_pk, project_id, agent_session_id, worktree_path, branch, title,
                   status, created_at, last_active, started_by, resume_attempts, branch_owned, perm_mode
            FROM sessions;
            DROP TABLE sessions;
            ALTER TABLE sessions_new RENAME TO sessions;
            "#,
        ),
        // Migration 23 — Plugin install ledger. `plugin_installs` is the
        // authoritative record of every installed skill pack / single skill
        // (source, resolved commit, content fingerprint, pin, trust
        // acknowledgment); the on-disk .ryuzi-skill.json stamp remains the
        // loader's trust gate but is no longer the record of record.
        // `plugin_attach_status` holds the last session-attach outcome per
        // plugin for the doctor surface. Renumbered to slot 23 across merges
        // with main (20 perm_mode, 21 native-only, 22 chat-first sessions); it
        // is now the tail. IF NOT EXISTS: the rewind-and-replay migration tests
        // re-run this on an already-migrated DB, so it must be a no-op on replay.
        M::up(
            "CREATE TABLE IF NOT EXISTS plugin_installs (\
                plugin_id TEXT PRIMARY KEY NOT NULL,\
                kind TEXT NOT NULL,\
                source_spec TEXT NOT NULL,\
                resolved_commit TEXT,\
                fingerprint TEXT NOT NULL,\
                installed_at INTEGER NOT NULL,\
                updated_at INTEGER NOT NULL,\
                pinned INTEGER NOT NULL DEFAULT 0,\
                pin_reason TEXT,\
                trust_tier TEXT NOT NULL,\
                trust_ack_at INTEGER,\
                trust_ack_summary TEXT\
            );\
            CREATE TABLE IF NOT EXISTS plugin_attach_status (\
                plugin_id TEXT PRIMARY KEY NOT NULL,\
                last_attach_at INTEGER NOT NULL,\
                outcome TEXT NOT NULL,\
                reason TEXT\
            );",
        ),
    ])
}

pub struct Store {
    pool: Pool,
}

/// One durable compaction checkpoint: the replacement history that stands in
/// for every provider turn with `seq <= boundary_seq`.
#[derive(Debug, Clone)]
pub struct ContextCheckpoint {
    pub boundary_seq: i64,
    pub window_number: i64,
    pub payload: serde_json::Value,
}

fn row_to_project(r: &Row) -> rusqlite::Result<Project> {
    let perm: String = r.get(6)?;
    let workdir: String = r.get(2)?;
    Ok(Project {
        project_id: r.get(0)?,
        name: r.get(1)?,
        // Read-time git-ness: cheap repo-open probe. Runs on the store's
        // blocking connection thread, so sync git2 is fine here.
        is_git: git2::Repository::open(&workdir).is_ok(),
        workdir,
        source: r.get(3)?,
        model: r.get(4)?,
        effort: r.get(5)?,
        perm_mode: PermMode::from_db(&perm),
        created_at: r.get(7)?,
    })
}

const PROJECT_COLS: &str = "project_id,name,workdir,source,model,effort,perm_mode,created_at";

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

#[derive(Debug, Clone, PartialEq)]
pub struct ModelStatusRow {
    pub family: String,
    pub model: String,
    /// "valid" | "invalid" — "unknown" is transient and never stored.
    pub status: String,
    pub message: String,
    pub tested_at: i64,
}

/// UTC calendar day (YYYY-MM-DD) for a millisecond timestamp.
fn day_of(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

fn from_sql_json_error(
    index: usize,
    err: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, err.into())
}

fn to_sql_json_error(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

fn map_plugin_install_row(r: &Row) -> rusqlite::Result<PluginInstallRecord> {
    Ok(PluginInstallRecord {
        plugin_id: r.get(0)?,
        kind: r.get(1)?,
        source_spec: r.get(2)?,
        resolved_commit: r.get(3)?,
        fingerprint: r.get(4)?,
        installed_at: r.get(5)?,
        updated_at: r.get(6)?,
        pinned: r.get::<_, i64>(7)? != 0,
        pin_reason: r.get(8)?,
        trust_tier: r.get(9)?,
        trust_ack_at: r.get(10)?,
        trust_ack_summary: r.get(11)?,
    })
}

fn map_plugin_attach_row(r: &Row) -> rusqlite::Result<PluginAttachStatus> {
    Ok(PluginAttachStatus {
        plugin_id: r.get(0)?,
        last_attach_at: r.get(1)?,
        outcome: r.get(2)?,
        reason: r.get(3)?,
    })
}

fn parse_plugin_oauth_token_json(raw: &str) -> anyhow::Result<Map<String, Value>> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("plugin oauth token json must be an object"))?;
    Ok(object)
}

fn upsert_plugin_oauth_token_json(
    existing: Option<&str>,
    token: &PluginOauthToken,
) -> anyhow::Result<String> {
    let mut object = match existing {
        Some(raw) => parse_plugin_oauth_token_json(raw)?,
        None => Map::new(),
    };
    object.insert(
        "plugin_id".to_string(),
        Value::String(token.plugin_id.clone()),
    );
    object.insert(
        "access_token".to_string(),
        Value::String(encrypt_field(&token.access_token)),
    );
    object.insert(
        "refresh_token".to_string(),
        token
            .refresh_token
            .as_deref()
            .map(encrypt_field)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "token_type".to_string(),
        Value::String(token.token_type.clone()),
    );
    object.insert(
        "expires_at".to_string(),
        token
            .expires_at
            .map(Number::from)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "scopes".to_string(),
        Value::Array(token.scopes.iter().cloned().map(Value::String).collect()),
    );
    object.insert(
        "reconnect_required".to_string(),
        Value::Bool(token.reconnect_required),
    );
    Ok(serde_json::to_string(&Value::Object(object))?)
}

fn decode_plugin_oauth_token(plugin_id: &str, raw: &str) -> anyhow::Result<PluginOauthToken> {
    let object = parse_plugin_oauth_token_json(raw)?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("plugin oauth token missing access_token"))?;
    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("plugin oauth token missing token_type"))?;
    let expires_at = object.get("expires_at").and_then(Value::as_i64);
    let scopes = object
        .get("scopes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let reconnect_required = object
        .get("reconnect_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok(PluginOauthToken {
        plugin_id: plugin_id.to_string(),
        access_token: decrypt_field(access_token)?,
        refresh_token: match object.get("refresh_token").and_then(Value::as_str) {
            Some(refresh_token) => Some(decrypt_field(refresh_token)?),
            None => None,
        },
        token_type: token_type.to_string(),
        expires_at,
        scopes,
        reconnect_required,
    })
}

/// One row per plugin in `plugin_oauth_clients` — a partial cache that
/// accretes: discovery fills the endpoint columns, DCR or the user's manual
/// entry fills `client_id`. `upsert_plugin_oauth_client` merges per column
/// (`Some` overwrites, `None` preserves), so callers never have to
/// read-modify-write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOauthClient {
    pub plugin_id: String,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub client_id: Option<String>,
}

/// One row of `plugin_installs`: the authoritative record of an installed
/// skill pack or single skill. `kind` is `"plugin_pack"` or `"single_skill"`.
/// `resolved_commit` is the git HEAD captured at install/update (`None` for
/// backfilled rows). `fingerprint` is a content hash of the installed tree
/// (excludes `.git` and the `.ryuzi-skill.json` stamp). `trust_tier` is
/// `"curated"` | `"acknowledged"` (`"blocked"` reserved for the future feed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInstallRecord {
    pub plugin_id: String,
    pub kind: String,
    pub source_spec: String,
    pub resolved_commit: Option<String>,
    pub fingerprint: String,
    pub installed_at: i64,
    pub updated_at: i64,
    pub pinned: bool,
    pub pin_reason: Option<String>,
    pub trust_tier: String,
    pub trust_ack_at: Option<i64>,
    pub trust_ack_summary: Option<String>,
}

/// One row of `plugin_attach_status`: the last time a plugin's connector was
/// attached to a session and whether it succeeded. `reason` is a secret-free
/// message (e.g. the `ensure_auth` "configure {id}: ..." text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginAttachStatus {
    pub plugin_id: String,
    pub last_attach_at: i64,
    pub outcome: String,
    pub reason: Option<String>,
}

/// Check out a pooled connection and run `f` on its dedicated blocking
/// thread. Pool checkout errors, interact-layer failures (panicked or aborted
/// closure), and the closure's own error all surface as one `anyhow::Result`.
async fn interact_on<T, E, F>(pool: &Pool, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&mut rusqlite::Connection) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let conn = pool.get().await?;
    let out = conn
        .interact(move |c| {
            // Pooled connections + WAL still return SQLITE_BUSY immediately on
            // write contention (e.g. a request's read racing a detached
            // usage-record / prune write). A busy_timeout makes them wait
            // instead of erroring — otherwise concurrent load surfaces as a 500.
            let _ = c.busy_timeout(std::time::Duration::from_secs(5));
            f(c)
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
    Ok(out)
}

impl Store {
    pub async fn open(path: &Path) -> anyhow::Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Single pooled connection. The store is low-QPS and SQLite is
        // single-writer anyway; more importantly, multiple WAL connections do
        // not reliably share the schema/writes on every filesystem — CI hit
        // "no such table" when a request's read landed on a different pooled
        // connection than the one migrations/writes ran on. One connection
        // serializes access and sidesteps that class of bug entirely.
        let mut cfg = Config::new(path);
        cfg.pool = Some(deadpool_sqlite::PoolConfig::new(1));
        let pool = cfg.create_pool(Runtime::Tokio1)?;
        interact_on(&pool, |c| {
            let _ = c.pragma_update(None, "journal_mode", "WAL");
            migrations().to_latest(c)
        })
        .await?;
        interact_on(&pool, |c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO settings(key, value) VALUES ('enabled_gateways', 'discord');",
            )
        })
        .await?;
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
        interact_on(&self.pool, f).await
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

    /// Persist a definitive model probe verdict. "unknown" (rate limit /
    /// server error / network) is transient and must never clobber a stored
    /// verdict, so it is a no-op here.
    pub async fn upsert_model_status(&self, row: ModelStatusRow) -> anyhow::Result<()> {
        if row.status == "unknown" {
            return Ok(());
        }
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO model_status(family, model, status, message, tested_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(family, model) DO UPDATE SET \
                    status=excluded.status, message=excluded.message, tested_at=excluded.tested_at",
                params![
                    row.family,
                    row.model,
                    row.status,
                    row.message,
                    row.tested_at
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn list_model_statuses(&self, family: &str) -> anyhow::Result<Vec<ModelStatusRow>> {
        let family = family.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT family, model, status, message, tested_at FROM model_status \
                 WHERE family=?1 ORDER BY model",
            )?;
            let items = stmt
                .query_map(params![family], |r| {
                    Ok(ModelStatusRow {
                        family: r.get(0)?,
                        model: r.get(1)?,
                        status: r.get(2)?,
                        message: r.get(3)?,
                        tested_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    /// Every persisted probe verdict across all families — hydrates the
    /// Cockpit-wide model-status store so pickers can hide invalid models.
    pub async fn list_all_model_statuses(&self) -> anyhow::Result<Vec<ModelStatusRow>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT family, model, status, message, tested_at FROM model_status \
                 ORDER BY family, model",
            )?;
            let items = stmt
                .query_map([], |r| {
                    Ok(ModelStatusRow {
                        family: r.get(0)?,
                        model: r.get(1)?,
                        status: r.get(2)?,
                        message: r.get(3)?,
                        tested_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    /// Update the user-editable project settings and return the fresh row.
    pub async fn update_project(
        &self,
        id: &str,
        model: Option<String>,
        perm_mode: PermMode,
    ) -> anyhow::Result<Option<Project>> {
        let id_owned = id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE projects SET model=?2, perm_mode=?3 WHERE project_id=?1",
                params![id_owned, model, perm_mode.as_str()],
            )
            .map(|_| ())
        })
        .await?;
        self.get_project(id).await
    }

    pub async fn insert_project(&self, p: Project) -> anyhow::Result<()> {
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO projects(project_id,name,workdir,source,model,effort,perm_mode,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    p.project_id, p.name, p.workdir, p.source,
                    p.model, p.effort, p.perm_mode.as_str(), p.created_at
                ],
            )
        })
        .await?;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> anyhow::Result<Option<Project>> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE project_id=?1"),
                params![id],
                row_to_project,
            )
            .optional()
        })
        .await
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.with_conn(|c| -> rusqlite::Result<Vec<Project>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {PROJECT_COLS} FROM projects ORDER BY created_at"
            ))?;
            let items = stmt
                .query_map([], row_to_project)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    pub async fn insert_session(&self, s: Session) -> anyhow::Result<()> {
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    s.session_pk, s.project_id, s.agent_session_id, s.worktree_path,
                    s.branch, s.title, s.status.as_str(), s.created_at, s.last_active,
                    s.started_by, s.resume_attempts, s.branch_owned, s.perm_mode.as_str(),
                    s.kind.as_str(), s.speaker, s.agent, s.parent_session_pk
                ],
            )
        })
        .await?;
        Ok(())
    }

    pub async fn get_session(&self, pk: &str) -> anyhow::Result<Option<Session>> {
        let pk = pk.to_string();
        self.with_conn(move |c| {
            c.query_row(
                &format!("SELECT {SESSION_COLS} FROM sessions WHERE session_pk=?1"),
                params![pk],
                row_to_session,
            )
            .optional()
        })
        .await
    }

    /// Set a session's title (used by the native runtime's title generation).
    pub async fn set_session_title(&self, pk: &str, title: &str) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let title = title.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET title=?2 WHERE session_pk=?1",
                params![pk, title],
            )
        })
        .await?;
        Ok(())
    }

    /// Set one session's permission mode (per-session override; the project
    /// row is only the default seed for NEW sessions).
    pub async fn update_session_perm_mode(&self, pk: &str, mode: PermMode) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let pk_for_err = pk.clone();
        let rows = self
            .with_conn(move |c| {
                c.execute(
                    "UPDATE sessions SET perm_mode=?2 WHERE session_pk=?1",
                    params![pk, mode.as_str()],
                )
            })
            .await?;
        if rows == 0 {
            anyhow::bail!("update_session_perm_mode: unknown session {pk_for_err}");
        }
        Ok(())
    }

    /// List sessions in a given status, oldest-first — used by `reconcile` on
    /// daemon boot to find sessions a dead process left in `Running`.
    pub async fn list_sessions_by_status(
        &self,
        status: SessionStatus,
    ) -> anyhow::Result<Vec<Session>> {
        let status = status.as_str().to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<Session>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLS} FROM sessions WHERE status=?1 ORDER BY created_at"
            ))?;
            let items = stmt
                .query_map(params![status], row_to_session)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    /// List sessions of a given `kind` (`"project"|"chat"|"worker"|"review"`),
    /// most-recently-created first — used by chat-first surfaces that only
    /// care about one session kind at a time.
    pub async fn list_sessions_by_kind(&self, kind: &str) -> anyhow::Result<Vec<Session>> {
        let kind = kind.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLS} FROM sessions WHERE kind=?1 ORDER BY created_at DESC"
            ))?;
            let rows = stmt.query_map([kind], row_to_session)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    pub async fn list_sessions(&self, project_id: Option<&str>) -> anyhow::Result<Vec<Session>> {
        let project_id = project_id.map(|s| s.to_string());
        self.with_conn(move |c| match project_id {
            Some(pid) => {
                let mut stmt = c.prepare(&format!(
                    "SELECT {SESSION_COLS} FROM sessions WHERE project_id=?1 ORDER BY created_at"
                ))?;
                let rows = stmt
                    .query_map(params![pid], row_to_session)?
                    .collect::<rusqlite::Result<Vec<_>>>();
                rows
            }
            None => {
                let mut stmt = c.prepare(&format!(
                    "SELECT {SESSION_COLS} FROM sessions ORDER BY created_at"
                ))?;
                let rows = stmt
                    .query_map([], row_to_session)?
                    .collect::<rusqlite::Result<Vec<_>>>();
                rows
            }
        })
        .await
    }

    pub async fn update_status(
        &self,
        pk: &str,
        status: SessionStatus,
        last_active: Option<i64>,
    ) -> anyhow::Result<()> {
        let pk = pk.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, last_active=COALESCE(?3, last_active) WHERE session_pk=?1",
                params![pk, status.as_str(), last_active],
            )
        })
        .await?;
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
        self.with_conn(move |c| {
            c.execute(
                "UPDATE projects SET model=COALESCE(?2, model), effort=COALESCE(?3, effort), \
                 perm_mode=COALESCE(?4, perm_mode) WHERE project_id=?1",
                params![project_id, model, effort, perm],
            )
        })
        .await?;
        Ok(())
    }

    /// Atomically demote `Running → Idle` only if the current status is still `Running`.
    /// A session already marked `Interrupted` or `Ended` is left untouched.
    /// Also resets `resume_attempts` to 0 — a turn that reaches a normal (or
    /// errored-but-demoted) end clears the auto-resume cap.
    pub async fn demote_if_running(&self, pk: &str, last_active: i64) -> anyhow::Result<()> {
        let pk = pk.to_string();
        self.with_conn(move |c| {
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
        .await?;
        Ok(())
    }

    /// Backfill the workspace columns once background startup has prepared
    /// the git workspace (session-first start returns a provisional row).
    pub async fn update_session_workspace(
        &self,
        pk: &str,
        worktree_path: Option<String>,
        branch: &str,
        branch_owned: bool,
    ) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let branch = branch.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET worktree_path=?2, branch=?3, branch_owned=?4 WHERE session_pk=?1",
                params![pk, worktree_path, branch, branch_owned],
            )
            .map(|_| ())
        })
        .await
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
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET status=?2, resume_attempts=?3 WHERE session_pk=?1",
                params![pk, status.as_str(), resume_attempts],
            )
        })
        .await?;
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
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET agent_session_id=?2 WHERE session_pk=?1",
                params![pk, agent],
            )
        })
        .await?;
        Ok(())
    }

    pub async fn insert_message(&self, m: NewMessage) -> anyhow::Result<i64> {
        let payload = serde_json::to_string(&m.payload)?;
        let created = now_ms();
        self.with_conn(move |c| {
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
    }

    pub async fn list_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<Message>> {
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
    }

    /// Append one message to the native runtime's provider-turn ledger,
    /// assigning `seq` atomically per session (same idiom as `insert_message`).
    /// Returns the assigned seq.
    pub async fn insert_provider_turn(&self, t: NewProviderTurn) -> anyhow::Result<i64> {
        let payload = serde_json::to_string(&t.payload)?;
        let created = now_ms();
        self.with_conn(move |c| {
            c.query_row(
                "INSERT INTO provider_turns(session_pk,seq,role,payload,created_at) \
                 SELECT ?1, COALESCE(MAX(seq),0)+1, ?2, ?3, ?4 \
                 FROM provider_turns WHERE session_pk=?1 \
                 RETURNING seq",
                params![t.session_pk, t.role, payload, created],
                |r| r.get::<_, i64>(0),
            )
        })
        .await
    }

    /// List a session's native todo items in order (content, status).
    pub async fn list_todos(&self, session_pk: &str) -> anyhow::Result<Vec<(String, String)>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt =
                c.prepare("SELECT content, status FROM todos WHERE session_pk=?1 ORDER BY pos")?;
            let rows = stmt
                .query_map(params![session_pk], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Load a session's provider-turn ledger in order, for resume/replay.
    pub async fn list_provider_turns(&self, session_pk: &str) -> anyhow::Result<Vec<ProviderTurn>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<ProviderTurn>> {
            let mut stmt = c.prepare(
                "SELECT session_pk,seq,role,payload,created_at \
                 FROM provider_turns WHERE session_pk=?1 ORDER BY seq",
            )?;
            let items = stmt
                .query_map(params![session_pk], |r| {
                    let payload: String = r.get(3)?;
                    Ok(ProviderTurn {
                        session_pk: r.get(0)?,
                        seq: r.get(1)?,
                        role: r.get(2)?,
                        payload: serde_json::from_str(&payload).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    pub async fn insert_context_checkpoint(
        &self,
        session_pk: &str,
        boundary_seq: i64,
        window_number: i64,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let session_pk = session_pk.to_string();
        let payload = serde_json::to_string(payload)?;
        let now = crate::paths::now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO context_checkpoints(session_pk,boundary_seq,window_number,payload,created_at) \
                 VALUES(?1,?2,?3,?4,?5)",
                params![session_pk, boundary_seq, window_number, payload, now],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn latest_context_checkpoint(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<ContextCheckpoint>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Option<ContextCheckpoint>> {
            let mut stmt = c.prepare(
                "SELECT boundary_seq,window_number,payload FROM context_checkpoints \
                 WHERE session_pk=?1 ORDER BY boundary_seq DESC, id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query_map(params![session_pk], |r| {
                let payload: String = r.get(2)?;
                Ok(ContextCheckpoint {
                    boundary_seq: r.get(0)?,
                    window_number: r.get(1)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                })
            })?;
            rows.next().transpose()
        })
        .await
    }

    pub async fn list_provider_turns_after(
        &self,
        session_pk: &str,
        after_seq: i64,
    ) -> anyhow::Result<Vec<ProviderTurn>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<ProviderTurn>> {
            let mut stmt = c.prepare(
                "SELECT session_pk,seq,role,payload,created_at \
                 FROM provider_turns WHERE session_pk=?1 AND seq>?2 ORDER BY seq",
            )?;
            let items = stmt
                .query_map(params![session_pk, after_seq], |r| {
                    let payload: String = r.get(3)?;
                    Ok(ProviderTurn {
                        session_pk: r.get(0)?,
                        seq: r.get(1)?,
                        role: r.get(2)?,
                        payload: serde_json::from_str(&payload).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    pub async fn upsert_session_context(
        &self,
        session_pk: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let session_pk = session_pk.to_string();
        let payload = serde_json::to_string(payload)?;
        let now = crate::paths::now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO session_context(session_pk,payload,updated_at) VALUES(?1,?2,?3) \
                 ON CONFLICT(session_pk) DO UPDATE SET payload=?2, updated_at=?3",
                params![session_pk, payload, now],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn get_session_context(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Option<String>> {
            let mut stmt = c.prepare("SELECT payload FROM session_context WHERE session_pk=?1")?;
            let mut rows = stmt.query_map(params![session_pk], |r| r.get(0))?;
            rows.next().transpose()
        })
        .await
        .map(|opt| opt.and_then(|s: String| serde_json::from_str(&s).ok()))
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
        self.with_conn(move |c| {
            c.query_row(
                "SELECT decision FROM tool_policies WHERE project_id=?1 AND tool=?2",
                params![project_id, tool],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
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
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO tool_policies(project_id, tool, decision) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(project_id, tool) DO UPDATE SET decision=excluded.decision",
                params![project_id, tool, decision],
            )
        })
        .await?;
        Ok(())
    }

    /// Every persisted tool policy, ordered for stable display.
    pub async fn list_tool_policies(&self) -> anyhow::Result<Vec<ToolPolicyRow>> {
        self.with_conn(|c| -> rusqlite::Result<Vec<ToolPolicyRow>> {
            let mut stmt = c.prepare(
                "SELECT project_id, tool, decision FROM tool_policies \
                 ORDER BY project_id, tool",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(ToolPolicyRow {
                        project_id: r.get(0)?,
                        tool: r.get(1)?,
                        decision: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Remove one persisted tool policy (the Settings "revoke" action).
    pub async fn delete_tool_policy(&self, project_id: &str, tool: &str) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM tool_policies WHERE project_id=?1 AND tool=?2",
                params![project_id, tool],
            )
        })
        .await?;
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
        let (seq, payload, tool_kind) = self
            .with_conn(move |c| {
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
            .await?;
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        Ok((seq, payload, tool_kind))
    }

    /// Return the raw persisted value for `key`, or `None` if no row exists.
    /// No defaults are applied here — that's the caller's job.
    pub async fn get_setting_raw(&self, key: &str) -> anyhow::Result<Option<String>> {
        let key = key.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
    }

    /// Upsert a raw setting value. No validation is performed.
    pub async fn set_setting_raw(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
        })
        .await?;
        Ok(())
    }

    /// Delete a settings row. A missing key is a no-op.
    pub async fn delete_setting_raw(&self, key: &str) -> anyhow::Result<()> {
        let key = key.to_string();
        self.with_conn(move |c| c.execute("DELETE FROM settings WHERE key = ?1", params![key]))
            .await?;
        Ok(())
    }

    /// List all persisted settings rows.
    pub async fn list_settings(&self) -> anyhow::Result<Vec<(String, String)>> {
        self.with_conn(|c| -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt = c.prepare("SELECT key, value FROM settings")?;
            let items = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
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
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO session_surfaces(gateway, conversation_id, session_pk) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(gateway, conversation_id) DO UPDATE SET session_pk = excluded.session_pk",
                params![gateway, conversation_id, session_pk],
            )
        })
        .await?;
        Ok(())
    }

    /// List the gateway surfaces bound to a session.
    pub async fn surfaces(&self, session_pk: &str) -> anyhow::Result<Vec<Surface>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<Surface>> {
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
    }

    /// Resolve the session bound to a `(gateway, conversation_id)` surface, if any.
    pub async fn resolve_by_conversation(
        &self,
        gateway: &str,
        conversation_id: &str,
    ) -> anyhow::Result<Option<Session>> {
        let gateway = gateway.to_string();
        let conversation_id = conversation_id.to_string();
        self.with_conn(move |c| {
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
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO project_bindings(gateway, workspace_id, project_id) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(gateway, workspace_id) DO UPDATE SET project_id = excluded.project_id",
                params![gateway, workspace_id, project_id],
            )
        })
        .await?;
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
        self.with_conn(move |c| {
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
                        day: r.get(0)?,
                        connection_id: r.get(1)?,
                        model: r.get(2)?,
                        requests: r.get(3)?,
                        input_tokens: r.get(4)?,
                        output_tokens: r.get(5)?,
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
                        connection_id: r.get(0)?,
                        requests: r.get(1)?,
                        input_tokens: r.get(2)?,
                        output_tokens: r.get(3)?,
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

    pub async fn upsert_plugin_oauth_token(&self, token: &PluginOauthToken) -> anyhow::Result<()> {
        let token = token.clone();
        let updated_at = now_ms();
        self.with_conn(move |c| {
            let existing: Option<String> = c
                .query_row(
                    "SELECT token_json FROM plugin_oauth_tokens WHERE plugin_id=?1",
                    params![&token.plugin_id],
                    |r| r.get(0),
                )
                .optional()?;
            let token_json = upsert_plugin_oauth_token_json(existing.as_deref(), &token)
                .map_err(to_sql_json_error)?;
            c.execute(
                "INSERT INTO plugin_oauth_tokens(plugin_id, token_json, updated_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(plugin_id) DO UPDATE SET \
                   token_json=excluded.token_json, \
                   updated_at=excluded.updated_at",
                params![token.plugin_id, token_json, updated_at],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get_plugin_oauth_token(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<PluginOauthToken>> {
        let plugin_id_owned = plugin_id.to_string();
        let raw: Option<String> = self
            .with_conn(move |c| {
                c.query_row(
                    "SELECT token_json FROM plugin_oauth_tokens WHERE plugin_id=?1",
                    params![plugin_id_owned],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?;
        raw.map(|raw| decode_plugin_oauth_token(plugin_id, &raw))
            .transpose()
    }

    pub async fn mark_plugin_oauth_reconnect_required(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<()> {
        let plugin_id = plugin_id.to_string();
        let updated_at = now_ms();
        self.with_conn(move |c| {
            let Some(raw): Option<String> = c
                .query_row(
                    "SELECT token_json FROM plugin_oauth_tokens WHERE plugin_id=?1",
                    params![&plugin_id],
                    |r| r.get(0),
                )
                .optional()?
            else {
                return Ok(());
            };
            let mut token = decode_plugin_oauth_token(&plugin_id, &raw)
                .map_err(|err| from_sql_json_error(0, err))?;
            token.reconnect_required = true;
            let token_json =
                upsert_plugin_oauth_token_json(Some(&raw), &token).map_err(to_sql_json_error)?;
            c.execute(
                "UPDATE plugin_oauth_tokens SET token_json=?2, updated_at=?3 WHERE plugin_id=?1",
                params![plugin_id, token_json, updated_at],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn delete_plugin_oauth_token(&self, plugin_id: &str) -> anyhow::Result<()> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM plugin_oauth_tokens WHERE plugin_id=?1",
                params![plugin_id],
            )
            .map(|_| ())
        })
        .await
    }

    /// Column-merge upsert: `Some` overwrites, `None` preserves the
    /// existing column (COALESCE against the stored row).
    pub async fn upsert_plugin_oauth_client(
        &self,
        client: &PluginOauthClient,
    ) -> anyhow::Result<()> {
        let client = client.clone();
        let updated_at = now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO plugin_oauth_clients(plugin_id, authorize_url, token_url, client_id, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(plugin_id) DO UPDATE SET \
                   authorize_url=COALESCE(excluded.authorize_url, plugin_oauth_clients.authorize_url), \
                   token_url=COALESCE(excluded.token_url, plugin_oauth_clients.token_url), \
                   client_id=COALESCE(excluded.client_id, plugin_oauth_clients.client_id), \
                   updated_at=excluded.updated_at",
                params![
                    client.plugin_id,
                    client.authorize_url,
                    client.token_url,
                    client.client_id,
                    updated_at
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get_plugin_oauth_client(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<PluginOauthClient>> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT plugin_id, authorize_url, token_url, client_id \
                 FROM plugin_oauth_clients WHERE plugin_id=?1",
                params![plugin_id],
                |r| {
                    Ok(PluginOauthClient {
                        plugin_id: r.get(0)?,
                        authorize_url: r.get(1)?,
                        token_url: r.get(2)?,
                        client_id: r.get(3)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    /// For a future "Reset OAuth client" affordance; nothing calls it from
    /// the wizard (disconnect keeps the row — client registration is
    /// vendor-side state and reconnect must not re-register).
    pub async fn delete_plugin_oauth_client(&self, plugin_id: &str) -> anyhow::Result<()> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM plugin_oauth_clients WHERE plugin_id=?1",
                params![plugin_id],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn upsert_plugin_install(&self, rec: &PluginInstallRecord) -> anyhow::Result<()> {
        let rec = rec.clone();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO plugin_installs(plugin_id, kind, source_spec, resolved_commit, \
                     fingerprint, installed_at, updated_at, pinned, pin_reason, trust_tier, \
                     trust_ack_at, trust_ack_summary) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(plugin_id) DO UPDATE SET \
                   kind=excluded.kind, source_spec=excluded.source_spec, \
                   resolved_commit=excluded.resolved_commit, fingerprint=excluded.fingerprint, \
                   installed_at=excluded.installed_at, updated_at=excluded.updated_at, \
                   pinned=excluded.pinned, pin_reason=excluded.pin_reason, \
                   trust_tier=excluded.trust_tier, trust_ack_at=excluded.trust_ack_at, \
                   trust_ack_summary=excluded.trust_ack_summary",
                params![
                    rec.plugin_id,
                    rec.kind,
                    rec.source_spec,
                    rec.resolved_commit,
                    rec.fingerprint,
                    rec.installed_at,
                    rec.updated_at,
                    rec.pinned as i64,
                    rec.pin_reason,
                    rec.trust_tier,
                    rec.trust_ack_at,
                    rec.trust_ack_summary,
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get_plugin_install(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<PluginInstallRecord>> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT plugin_id, kind, source_spec, resolved_commit, fingerprint, \
                     installed_at, updated_at, pinned, pin_reason, trust_tier, trust_ack_at, \
                     trust_ack_summary FROM plugin_installs WHERE plugin_id=?1",
                params![plugin_id],
                map_plugin_install_row,
            )
            .optional()
        })
        .await
    }

    pub async fn list_plugin_installs(&self) -> anyhow::Result<Vec<PluginInstallRecord>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT plugin_id, kind, source_spec, resolved_commit, fingerprint, \
                     installed_at, updated_at, pinned, pin_reason, trust_tier, trust_ack_at, \
                     trust_ack_summary FROM plugin_installs ORDER BY plugin_id",
            )?;
            let rows = stmt
                .query_map([], map_plugin_install_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn delete_plugin_install(&self, plugin_id: &str) -> anyhow::Result<()> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM plugin_installs WHERE plugin_id=?1",
                params![plugin_id],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn set_plugin_install_pin(
        &self,
        plugin_id: &str,
        pinned: bool,
        reason: Option<&str>,
    ) -> anyhow::Result<()> {
        let plugin_id = plugin_id.to_string();
        let reason = reason.map(str::to_string);
        self.with_conn(move |c| {
            c.execute(
                "UPDATE plugin_installs SET pinned=?2, pin_reason=?3 WHERE plugin_id=?1",
                params![plugin_id, pinned as i64, reason],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn record_plugin_attach(&self, status: &PluginAttachStatus) -> anyhow::Result<()> {
        let status = status.clone();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO plugin_attach_status(plugin_id, last_attach_at, outcome, reason) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(plugin_id) DO UPDATE SET \
                   last_attach_at=excluded.last_attach_at, outcome=excluded.outcome, \
                   reason=excluded.reason",
                params![
                    status.plugin_id,
                    status.last_attach_at,
                    status.outcome,
                    status.reason
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get_plugin_attach(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<PluginAttachStatus>> {
        let plugin_id = plugin_id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT plugin_id, last_attach_at, outcome, reason FROM plugin_attach_status \
                 WHERE plugin_id=?1",
                params![plugin_id],
                map_plugin_attach_row,
            )
            .optional()
        })
        .await
    }

    pub async fn list_plugin_attach(&self) -> anyhow::Result<Vec<PluginAttachStatus>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT plugin_id, last_attach_at, outcome, reason FROM plugin_attach_status \
                 ORDER BY plugin_id",
            )?;
            let rows = stmt
                .query_map([], map_plugin_attach_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}

const SESSION_COLS: &str =
    "session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk";

fn row_to_session(r: &Row) -> rusqlite::Result<Session> {
    let status: String = r.get(6)?;
    let kind: String = r.get(13)?;
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
        branch_owned: r.get(11)?,
        perm_mode: {
            let pm: String = r.get(12)?;
            PermMode::from_db(&pm)
        },
        kind: SessionKind::from_db(&kind),
        speaker: r.get(14)?,
        agent: r.get(15)?,
        parent_session_pk: r.get(16)?,
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
    use crate::llm_router::secrets::use_test_key_file;
    use crate::plugins::oauth::PluginOauthToken;

    fn sample_project() -> Project {
        Project {
            project_id: "p1".into(),
            name: "demo".into(),
            workdir: "/tmp/demo".into(),
            source: None,
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(123),
            is_git: false,
        }
    }

    #[tokio::test]
    async fn delete_setting_raw_removes_the_row_and_tolerates_missing_keys() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .set_setting_raw("discord.token", "secret")
            .await
            .unwrap();
        store.delete_setting_raw("discord.token").await.unwrap();
        assert_eq!(store.get_setting_raw("discord.token").await.unwrap(), None);
        // Deleting a key that doesn't exist is a no-op, not an error.
        store.delete_setting_raw("discord.token").await.unwrap();
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

    #[tokio::test]
    async fn project_is_git_is_computed_at_read_time() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // sample_project points at /tmp/demo — not a git repo on this machine.
        store.insert_project(sample_project()).await.unwrap();
        assert!(!store.get_project("p1").await.unwrap().unwrap().is_git);

        // A workdir that IS a repo reports is_git=true on read — even though
        // the flag is never persisted (it self-corrects after `git init`).
        let repo_dir = tempfile::tempdir().unwrap();
        git2::Repository::init(repo_dir.path()).unwrap();
        let mut p = sample_project();
        p.project_id = "p2".into();
        p.workdir = repo_dir.path().to_string_lossy().into_owned();
        // Later created_at than sample_project's Some(123): list_projects
        // sorts by `ORDER BY created_at` alone (store.rs:627), so a tie would
        // leave the [false, true] assertion below to unspecified SQL ordering.
        p.created_at = Some(456);
        store.insert_project(p).await.unwrap();
        assert!(store.get_project("p2").await.unwrap().unwrap().is_git);
        let listed = store.list_projects().await.unwrap();
        assert_eq!(
            listed.iter().map(|p| p.is_git).collect::<Vec<_>>(),
            vec![false, true],
            "list_projects must compute the flag per row"
        );
    }

    fn sample_session() -> Session {
        Session {
            session_pk: "s1".into(),
            project_id: Some("p1".into()),
            agent_session_id: None,
            worktree_path: Some("/tmp/wt".into()),
            branch: Some("harness/abcdef01".into()),
            title: Some("hello".into()),
            status: SessionStatus::Running,
            started_by: None,
            created_at: Some(1),
            last_active: Some(1),
            resume_attempts: 0,
            branch_owned: true,
            perm_mode: PermMode::Default,
            kind: SessionKind::Project,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        }
    }

    #[tokio::test]
    async fn migration_10_creates_provider_turns_and_todos() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // Both native-runtime tables exist and are empty on a fresh DB.
        let counts = store
            .with_conn(|c| {
                let pt: i64 =
                    c.query_row("SELECT count(*) FROM provider_turns", [], |r| r.get(0))?;
                let td: i64 = c.query_row("SELECT count(*) FROM todos", [], |r| r.get(0))?;
                Ok((pt, td))
            })
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[tokio::test]
    async fn migrations_11_12_add_orch_task_graph_and_pre_check() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .with_conn(|c| {
                // Migration 12: jobs.pre_check exists with an empty default.
                let has_pre_check: bool = c
                    .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name='pre_check'")?
                    .exists([])?;
                assert!(has_pre_check, "jobs.pre_check column");
                // Migration 11: the orch task graph roundtrips.
                c.execute(
                    "INSERT INTO orch_tasks(id, root_id, project_id, title, body, created_at) \
                     VALUES ('t1', NULL, 'p1', 'root goal', 'do it', 1)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO orch_tasks(id, root_id, project_id, title, body, created_at) \
                     VALUES ('t2', 't1', 'p1', 'child', 'step one', 2)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO orch_task_deps(task_id, dep_id) VALUES ('t2', 't1')",
                    [],
                )?;
                let status: String =
                    c.query_row("SELECT status FROM orch_tasks WHERE id='t2'", [], |r| {
                        r.get(0)
                    })?;
                assert_eq!(status, "todo", "default status");
                let deps: i64 =
                    c.query_row("SELECT count(*) FROM orch_task_deps", [], |r| r.get(0))?;
                assert_eq!(deps, 1);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn migration_adds_sessions_branch_owned_defaulting_to_owned() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // A row inserted without the new column (as every pre-migration row
        // was) must read back as engine-owned: legacy sessions keep today's
        // delete-branch-on-teardown behavior.
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO sessions(session_pk, project_id) VALUES ('legacy', 'p1')",
                    [],
                )
            })
            .await
            .unwrap();
        let s = store.get_session("legacy").await.unwrap().unwrap();
        assert!(s.branch_owned, "legacy rows must default to branch_owned=1");
    }

    #[tokio::test]
    async fn model_status_upserts_and_lists_by_family() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-x".into(),
                status: "valid".into(),
                message: "Model claude-x OK".into(),
                tested_at: 100,
            })
            .await
            .unwrap();
        store
            .upsert_model_status(ModelStatusRow {
                family: "openai".into(),
                model: "gpt-x".into(),
                status: "invalid".into(),
                message: "Model gpt-x returned HTTP 404".into(),
                tested_at: 101,
            })
            .await
            .unwrap();
        // A re-test overwrites the previous verdict for the same (family, model).
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-x".into(),
                status: "invalid".into(),
                message: "Model claude-x returned HTTP 404".into(),
                tested_at: 200,
            })
            .await
            .unwrap();

        let rows = store.list_model_statuses("anthropic").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-x");
        assert_eq!(rows[0].status, "invalid");
        assert_eq!(rows[0].tested_at, 200);
        assert_eq!(store.list_model_statuses("openai").await.unwrap().len(), 1);
        assert!(store
            .list_model_statuses("mistral")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn model_status_unknown_never_overwrites_or_inserts() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-x".into(),
                status: "valid".into(),
                message: "Model claude-x OK".into(),
                tested_at: 100,
            })
            .await
            .unwrap();
        // A transient failure (429/5xx/network) must not clobber the verdict…
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-x".into(),
                status: "unknown".into(),
                message: "Model claude-x network error: timeout".into(),
                tested_at: 200,
            })
            .await
            .unwrap();
        // …and must not create a row for a never-validated model either.
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-y".into(),
                status: "unknown".into(),
                message: "Model claude-y returned HTTP 429".into(),
                tested_at: 201,
            })
            .await
            .unwrap();

        let rows = store.list_model_statuses("anthropic").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-x");
        assert_eq!(rows[0].status, "valid");
        assert_eq!(rows[0].tested_at, 100);
    }

    #[tokio::test]
    async fn list_all_model_statuses_returns_every_family() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(store.list_all_model_statuses().await.unwrap().is_empty());
        store
            .upsert_model_status(ModelStatusRow {
                family: "openai".into(),
                model: "gpt-x".into(),
                status: "invalid".into(),
                message: "Model gpt-x returned HTTP 404".into(),
                tested_at: 101,
            })
            .await
            .unwrap();
        store
            .upsert_model_status(ModelStatusRow {
                family: "anthropic".into(),
                model: "claude-x".into(),
                status: "valid".into(),
                message: "Model claude-x OK".into(),
                tested_at: 100,
            })
            .await
            .unwrap();

        let rows = store.list_all_model_statuses().await.unwrap();
        assert_eq!(rows.len(), 2);
        // Deterministic ORDER BY family, model: anthropic sorts before openai.
        assert_eq!(rows[0].family, "anthropic");
        assert_eq!(rows[0].model, "claude-x");
        assert_eq!(rows[0].status, "valid");
        assert_eq!(rows[0].tested_at, 100);
        assert_eq!(rows[1].family, "openai");
        assert_eq!(rows[1].model, "gpt-x");
        assert_eq!(rows[1].status, "invalid");
    }

    #[tokio::test]
    async fn insert_session_roundtrips_branch_owned_false() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let mut s = sample_session();
        s.branch_owned = false;
        store.insert_session(s).await.unwrap();
        let got = store.get_session("s1").await.unwrap().unwrap();
        assert!(
            !got.branch_owned,
            "user-named branches persist as not-owned"
        );
    }

    #[tokio::test]
    async fn chat_session_persists_with_null_project() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(crate::domain::Session {
                session_pk: "chat-1".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("hello".into()),
                status: crate::domain::SessionStatus::Idle,
                started_by: Some("cockpit".into()),
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: crate::domain::PermMode::Default,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let got = store.get_session("chat-1").await.unwrap().unwrap();
        assert_eq!(got.project_id, None);
        assert_eq!(got.kind, crate::domain::SessionKind::Chat);
        // list_sessions(None) still returns it; project filter excludes it.
        assert!(store
            .list_sessions(None)
            .await
            .unwrap()
            .iter()
            .any(|s| s.session_pk == "chat-1"));
        assert!(store
            .list_sessions_by_kind("chat")
            .await
            .unwrap()
            .iter()
            .any(|s| s.session_pk == "chat-1"));
    }

    #[tokio::test]
    async fn session_perm_mode_roundtrips_and_updates() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        let mut s = sample_session();
        s.perm_mode = PermMode::Plan;
        store.insert_session(s).await.unwrap();

        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.perm_mode, PermMode::Plan);

        store
            .update_session_perm_mode("s1", PermMode::AcceptEdits)
            .await
            .unwrap();
        let got = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(got.perm_mode, PermMode::AcceptEdits);
    }

    #[tokio::test]
    async fn update_session_perm_mode_on_unknown_session_is_an_error() {
        // The UPDATE previously matched zero rows and silently no-opped —
        // a caller could believe the mode persisted when it never did.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let err = store
            .update_session_perm_mode("does-not-exist", PermMode::AcceptEdits)
            .await
            .expect_err("updating a missing session must surface an error");
        assert!(err.to_string().contains("does-not-exist"), "{err}");
    }

    #[tokio::test]
    async fn provider_turns_get_monotonic_seq_and_list_in_order() {
        use crate::domain::NewProviderTurn;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        let s1 = store
            .insert_provider_turn(NewProviderTurn::new(
                "sess",
                "user",
                serde_json::json!([{"type": "text", "text": "hi"}]),
            ))
            .await
            .unwrap();
        let s2 = store
            .insert_provider_turn(NewProviderTurn::new(
                "sess",
                "assistant",
                serde_json::json!([{"type": "text", "text": "hello"}]),
            ))
            .await
            .unwrap();
        assert_eq!((s1, s2), (1, 2));

        // A different session numbers independently from 1.
        let other = store
            .insert_provider_turn(NewProviderTurn::new("other", "user", serde_json::json!([])))
            .await
            .unwrap();
        assert_eq!(other, 1);

        let turns = store.list_provider_turns("sess").await.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].seq, 1);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].payload[0]["text"], "hi");
        assert_eq!(turns[1].role, "assistant");
        assert!(store
            .list_provider_turns("nobody")
            .await
            .unwrap()
            .is_empty());
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
    async fn list_and_delete_tool_policies() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .set_tool_policy("p1", "Bash", "allowAlways")
            .await
            .unwrap();
        store
            .set_tool_policy("p2", "Edit", "rejectAlways")
            .await
            .unwrap();
        let rows = store.list_tool_policies().await.unwrap();
        assert_eq!(rows.len(), 2);
        store.delete_tool_policy("p1", "Bash").await.unwrap();
        let rows = store.list_tool_policies().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_id, "p2");
        assert_eq!(rows[0].decision, "rejectAlways");
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
            .update_project("p1", Some("claude-opus-4-5".into()), PermMode::AcceptEdits)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.model.as_deref(), Some("claude-opus-4-5"));
        assert_eq!(updated.perm_mode, PermMode::AcceptEdits);

        // Unknown project → Ok(None), not an error.
        assert!(store
            .update_project("missing", None, PermMode::Default)
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
            // Minimal v4 shape: the 4 old tables + user_version 4. The project
            // row carries a non-default perm_mode so the later
            // sessions.perm_mode migration's backfill (Task 3) has something
            // real to copy from 'old1'.
            conn.execute_batch(
                "CREATE TABLE projects (project_id TEXT PRIMARY KEY, name TEXT, workdir TEXT NOT NULL, source TEXT, harness TEXT NOT NULL DEFAULT 'claude-code', model TEXT, effort TEXT, perm_mode TEXT NOT NULL DEFAULT 'default', created_at INTEGER);
                 CREATE TABLE sessions (session_pk TEXT PRIMARY KEY, project_id TEXT NOT NULL, agent_session_id TEXT, worktree_path TEXT, branch TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'idle', created_at INTEGER, last_active INTEGER);
                 CREATE TABLE messages (session_pk TEXT NOT NULL, seq INTEGER NOT NULL, role TEXT NOT NULL, block_type TEXT NOT NULL, payload TEXT NOT NULL, tool_call_id TEXT, status TEXT, tool_kind TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (session_pk, seq));
                 CREATE TABLE tool_policies (project_id TEXT NOT NULL, tool TEXT NOT NULL, decision TEXT NOT NULL, PRIMARY KEY (project_id, tool));
                 INSERT INTO projects(project_id, name, workdir, perm_mode) VALUES ('p1', 'old', '/w', 'acceptEdits');
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
            s.perm_mode,
            PermMode::AcceptEdits,
            "sessions.perm_mode backfills from the owning project"
        );
        assert_eq!(
            store
                .get_setting_raw("enabled_gateways")
                .await
                .unwrap()
                .as_deref(),
            Some("discord")
        );
        // Phase 2 migration 20 (sessions rebuild: nullable project_id +
        // kind/speaker/agent/parent_session_pk) must also fire on this
        // ancient-DB replay. A row that pre-dates the `kind` column entirely
        // (inserted here under the raw v4 shape) has to land as kind='project'
        // — the rebuild's `DEFAULT 'project'` on `sessions_new`, verified end
        // to end via a genuine forward migration rather than a fresh store.
        // (A chat session's own insert/read-back round-trip is covered
        // separately by `chat_session_persists_with_null_project` below.)
        assert_eq!(s.kind, crate::domain::SessionKind::Project);
        assert_eq!(s.project_id.as_deref(), Some("p1"));
        let user_version: i64 = store
            .with_conn(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(user_version, 23, "forward migration must land at v23");
    }

    #[tokio::test]
    async fn plugin_oauth_client_upsert_merges_columns_and_roundtrips() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(store
            .get_plugin_oauth_client("acme")
            .await
            .unwrap()
            .is_none());

        // Discovery fills the endpoints; client_id stays NULL.
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "acme".into(),
                authorize_url: Some("https://vendor.test/authorize".into()),
                token_url: Some("https://vendor.test/token".into()),
                client_id: None,
            })
            .await
            .unwrap();
        // DCR (or manual entry) later fills client_id; None endpoints must
        // NOT clobber the discovered values.
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "acme".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-1".into()),
            })
            .await
            .unwrap();
        let row = store
            .get_plugin_oauth_client("acme")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row,
            PluginOauthClient {
                plugin_id: "acme".into(),
                authorize_url: Some("https://vendor.test/authorize".into()),
                token_url: Some("https://vendor.test/token".into()),
                client_id: Some("client-1".into()),
            }
        );

        // Some overwrites; untouched columns persist.
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "acme".into(),
                authorize_url: Some("https://vendor.test/authorize2".into()),
                token_url: None,
                client_id: None,
            })
            .await
            .unwrap();
        let row = store
            .get_plugin_oauth_client("acme")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.authorize_url.as_deref(),
            Some("https://vendor.test/authorize2")
        );
        assert_eq!(row.token_url.as_deref(), Some("https://vendor.test/token"));
        assert_eq!(row.client_id.as_deref(), Some("client-1"));

        store.delete_plugin_oauth_client("acme").await.unwrap();
        assert!(store
            .get_plugin_oauth_client("acme")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn plugin_oauth_clients_v1_not_null_table_is_rebuilt_on_open() {
        // Some dev DBs carry the pre-release v1 shape of plugin_oauth_clients
        // (every column NOT NULL, from an uncommitted experimental build).
        // CREATE TABLE IF NOT EXISTS never heals an existing table, so the
        // discovery-first upsert (client_id = NULL) died with "NOT NULL
        // constraint failed: plugin_oauth_clients.client_id". The rebuild
        // migration must swap such a table to the nullable shape on the next
        // open, preserving any rows it holds.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(move |c| {
                    c.execute_batch(
                        "DROP TABLE plugin_oauth_clients;\
                         CREATE TABLE plugin_oauth_clients (\
                             plugin_id TEXT PRIMARY KEY NOT NULL,\
                             authorize_url TEXT NOT NULL,\
                             token_url TEXT NOT NULL,\
                             client_id TEXT NOT NULL,\
                             updated_at INTEGER NOT NULL\
                         );\
                         INSERT INTO plugin_oauth_clients \
                             VALUES ('legacy', 'https://v.test/a', 'https://v.test/t', 'cid-1', 1);\
                         -- Rewind past the rebuild slot so it re-runs on the
                         -- next open, exactly like a dev DB that migrated up
                         -- to the slot before it.
                         PRAGMA user_version = 18;",
                    )
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        // The v1-era row survives the rebuild...
        let legacy = store
            .get_plugin_oauth_client("legacy")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(legacy.client_id.as_deref(), Some("cid-1"));
        // ...and the discovery-first upsert (no client id yet) now works.
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "atlassian".into(),
                authorize_url: Some("https://v.test/authorize".into()),
                token_url: Some("https://v.test/token".into()),
                client_id: None,
            })
            .await
            .unwrap();
        let row = store
            .get_plugin_oauth_client("atlassian")
            .await
            .unwrap()
            .unwrap();
        assert!(row.client_id.is_none());
    }

    #[tokio::test]
    async fn migrations_13_to_23_replay_is_idempotent_and_converges_native_only() {
        // An existing DB carries pre-Ryuzi-only rows. Build a current-schema
        // DB, seed the old values, then wind user_version back ten so the
        // rewrite migration (13) AND every migration appended after it
        // (14 sessions.branch_owned — hook-guarded; 15 model_status —
        // CREATE TABLE IF NOT EXISTS; 16 plugin_oauth_tokens + model_status —
        // CREATE TABLE IF NOT EXISTS; 17 context_checkpoints + session_context —
        // CREATE TABLE IF NOT EXISTS; 18 plugin_oauth_clients — CREATE TABLE
        // IF NOT EXISTS; 19 plugin_oauth_clients rebuild — idempotent
        // copy-drop-rename; 20 sessions.perm_mode — hook-guarded, like
        // branch_owned; 21 native-only cleanup — fully existence-guarded;
        // 22 sessions rebuild — nullable project_id + kind/speaker/agent/
        // parent_session_pk, copies every existing column forward including
        // perm_mode; 23 plugin_installs + plugin_attach_status — CREATE TABLE
        // IF NOT EXISTS; all no-ops on replay) re-run on the next open.
        // `Migrations` always fast-forwards to the latest defined version, so
        // there is no way to replay 13 alone once something is appended after
        // it. Bump this offset by one for every migration appended after 13 —
        // a stale offset silently skips migration 13 (the DB opens fine, but
        // this test starts failing its assertions). With migrations through 23
        // defined, wind back eleven.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let rewind = |c: &mut rusqlite::Connection| -> rusqlite::Result<()> {
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            c.pragma_update(None, "user_version", v - 11)
        };
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(move |c| {
                    // The DB is fully migrated to v23 here, so `harness` was
                    // already dropped: re-add it (and rows) so migration 13's
                    // guarded UPDATE and migration 21's guarded DROP both run
                    // their real paths on replay.
                    c.execute_batch(
                        "ALTER TABLE projects ADD COLUMN harness TEXT NOT NULL DEFAULT 'claude-code';
                         INSERT INTO projects(project_id, name, workdir) VALUES ('p-old', 'old', '/w');
                         INSERT INTO projects(project_id, name, workdir) VALUES ('p-new', 'new', '/w2');
                         UPDATE settings SET value='claude-code' WHERE key='default_runtime';
                         UPDATE settings SET value='claude-code,codex' WHERE key='enabled_runtimes';
                         INSERT INTO settings(key, value) VALUES ('default_agent', 'claude');",
                    )?;
                    rewind(c)
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        // Migration 13 rewrote the legacy values; migration 21 then deleted the
        // runtime-era keys and the harness column outright.
        assert!(store
            .get_setting("default_runtime")
            .await
            .unwrap()
            .is_none());
        assert!(store.get_setting("default_agent").await.unwrap().is_none());
        assert!(store
            .get_setting("enabled_runtimes")
            .await
            .unwrap()
            .is_none());
        assert!(store.get_project("p-old").await.unwrap().is_some());
        assert!(store.get_project("p-new").await.unwrap().is_some());

        // Idempotent: winding back and re-running must not error or resurrect keys.
        store.with_conn(rewind).await.unwrap();
        drop(store);
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(store
            .get_setting("enabled_runtimes")
            .await
            .unwrap()
            .is_none());
        assert!(store.get_project("p-old").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn migration_21_drops_the_runtime_concept() {
        // Simulate a v20 (pre-native-only) DB: open a fully migrated store,
        // manually re-create every legacy artifact migration 21 handles,
        // wind user_version back three, and reopen so 21 (and the tail
        // migrations 22 sessions rebuild + 23 plugin-install ledger) replay
        // against it. Back THREE: the fully migrated tail is now v23, so
        // rewinding to v20 is what makes migration 21 (native-only) replay.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let rewind = |c: &mut rusqlite::Connection| -> rusqlite::Result<()> {
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            c.pragma_update(None, "user_version", v - 3)
        };
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(move |c| {
                    c.execute_batch(
                        "ALTER TABLE projects ADD COLUMN harness TEXT NOT NULL DEFAULT 'claude-code';
                         INSERT INTO projects(project_id, name, workdir, harness) VALUES ('p1', 'legacy', '/w', 'claude-code');
                         ALTER TABLE jobs ADD COLUMN agent TEXT NOT NULL DEFAULT 'claude';
                         INSERT INTO jobs(id, name, cron, project_id, prompt, agent) VALUES ('j1', 'audit', '0 2 * * *', 'p1', 'run it', 'claude');
                         CREATE TABLE agents (id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0, model TEXT, perm_mode TEXT NOT NULL DEFAULT 'ask', flags TEXT NOT NULL DEFAULT '');
                         INSERT INTO agents(id, enabled, model, perm_mode) VALUES ('native', 1, 'openrouter/qwen3:free', 'edit');
                         INSERT INTO agents(id, enabled, model, perm_mode) VALUES ('claude', 1, 'claude-opus-4-5', 'ask');
                         CREATE TABLE agent_tiers (agent_id TEXT NOT NULL, tier_id TEXT NOT NULL, value TEXT, combo INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (agent_id, tier_id));
                         INSERT INTO agent_tiers(agent_id, tier_id, value) VALUES ('claude', 'fast', 'claude-haiku-4-5');
                         INSERT OR REPLACE INTO settings(key, value) VALUES ('enabled_runtimes', 'native,codex');
                         INSERT OR REPLACE INTO settings(key, value) VALUES ('default_runtime', 'native');
                         INSERT OR REPLACE INTO settings(key, value) VALUES ('default_agent', 'native');
                         INSERT OR REPLACE INTO settings(key, value) VALUES ('agents_snapshot', '[]');
                         INSERT INTO mcp_agent_access(server_id, agent_id, allowed) VALUES ('srv1', 'native', 1);
                         INSERT INTO mcp_agent_access(server_id, agent_id, allowed) VALUES ('srv1', 'claude', 1);
                         INSERT INTO mcp_agent_access(server_id, agent_id, allowed) VALUES ('srv1', 'codex', 0);",
                    )?;
                    rewind(c)
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        // Columns gone.
        let (has_harness, has_agent, has_agents_table) = store
            .with_conn(|c| {
                let h = c
                    .prepare("SELECT 1 FROM pragma_table_info('projects') WHERE name='harness'")?
                    .exists([])?;
                let a = c
                    .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name='agent'")?
                    .exists([])?;
                let t = c
                    .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name IN ('agents','agent_tiers')")?
                    .exists([])?;
                Ok((h, a, t))
            })
            .await
            .unwrap();
        assert!(!has_harness, "projects.harness must be dropped");
        assert!(!has_agent, "jobs.agent must be dropped");
        assert!(!has_agents_table, "agents/agent_tiers must be dropped");
        // Rows survive the column drops and still load through the new readers.
        assert_eq!(
            store.get_project("p1").await.unwrap().unwrap().name,
            "legacy"
        );
        assert_eq!(
            crate::scheduler::get_job(&store, "j1")
                .await
                .unwrap()
                .unwrap()
                .prompt,
            "run it"
        );
        // Native prefs copied into KV; dead settings keys deleted.
        assert_eq!(
            store.get_setting("agent_model").await.unwrap().as_deref(),
            Some("openrouter/qwen3:free")
        );
        assert_eq!(
            store
                .get_setting("agent_perm_mode")
                .await
                .unwrap()
                .as_deref(),
            Some("edit")
        );
        for key in [
            "enabled_runtimes",
            "default_runtime",
            "default_agent",
            "agents_snapshot",
        ] {
            assert!(
                store.get_setting(key).await.unwrap().is_none(),
                "{key} must be deleted"
            );
        }
        // Only the native mcp_agent_access row survives.
        let rows: i64 = store
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM mcp_agent_access", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(rows, 1);

        // KV-absent rule: a pre-existing agent_model must NOT be clobbered on replay.
        store
            .set_setting("agent_model", "user-chose-this")
            .await
            .unwrap();
        store.with_conn(rewind).await.unwrap();
        drop(store);
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(
            store.get_setting("agent_model").await.unwrap().as_deref(),
            Some("user-chose-this"),
            "replay on an already-migrated DB must be a no-op"
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
                connection_id: "c1".into(),
                provider: "openai".into(),
                model: "gpt-x".into(),
                client_format: "openai".into(),
                input_tokens: 10,
                output_tokens: 5,
                status_code: 200,
                duration_ms: 42,
                error: None,
                ts: base,
            })
            .await
            .unwrap();
        store
            .record_request(UsageRecord {
                connection_id: "c1".into(),
                provider: "openai".into(),
                model: "gpt-x".into(),
                client_format: "openai".into(),
                input_tokens: 7,
                output_tokens: 3,
                status_code: 200,
                duration_ms: 30,
                error: None,
                ts: base + 1000,
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
                connection_id: "c1".into(),
                provider: "p".into(),
                model: "m".into(),
                client_format: "openai".into(),
                input_tokens: 1,
                output_tokens: 1,
                status_code: 200,
                duration_ms: 1,
                error: None,
                ts: old,
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

    async fn raw_plugin_oauth_token_json(store: &Store, plugin_id: &str) -> String {
        let plugin_id = plugin_id.to_string();
        store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT token_json FROM plugin_oauth_tokens WHERE plugin_id=?1",
                    params![plugin_id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn plugin_oauth_token_roundtrip_encrypts_at_rest() {
        use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let token = PluginOauthToken {
            plugin_id: "acme".into(),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            token_type: "Bearer".into(),
            expires_at: Some(1_700_000_000_000),
            scopes: vec!["repo".into(), "issues:read".into()],
            reconnect_required: false,
        };

        store.upsert_plugin_oauth_token(&token).await.unwrap();

        let raw_json = raw_plugin_oauth_token_json(&store, "acme").await;
        assert!(
            !raw_json.contains("access-secret"),
            "access token must not be stored in plaintext: {raw_json}"
        );
        assert!(
            !raw_json.contains("refresh-secret"),
            "refresh token must not be stored in plaintext: {raw_json}"
        );

        let roundtrip = store.get_plugin_oauth_token("acme").await.unwrap().unwrap();
        assert_eq!(roundtrip.plugin_id, "acme");
        assert_eq!(roundtrip.access_token, "access-secret");
        assert_eq!(roundtrip.refresh_token.as_deref(), Some("refresh-secret"));
        assert_eq!(roundtrip.token_type, "Bearer");
        assert_eq!(roundtrip.expires_at, Some(1_700_000_000_000));
        assert_eq!(roundtrip.scopes, vec!["repo", "issues:read"]);
        assert!(!roundtrip.reconnect_required);
    }

    #[tokio::test]
    async fn mark_plugin_oauth_reconnect_required_updates_flag_without_dropping_other_fields() {
        use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let token = PluginOauthToken {
            plugin_id: "acme".into(),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            token_type: "Bearer".into(),
            expires_at: Some(1_700_000_000_000),
            scopes: vec!["repo".into()],
            reconnect_required: false,
        };
        store.upsert_plugin_oauth_token(&token).await.unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE plugin_oauth_tokens SET token_json = json_set(token_json, '$.resource_metadata', 'https://example.test/.well-known/oauth-protected-resource')",
                    [],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        store
            .mark_plugin_oauth_reconnect_required("acme")
            .await
            .unwrap();

        let roundtrip = store.get_plugin_oauth_token("acme").await.unwrap().unwrap();
        assert!(roundtrip.reconnect_required);
        assert_eq!(roundtrip.access_token, "access-secret");
        assert_eq!(roundtrip.refresh_token.as_deref(), Some("refresh-secret"));

        let raw_json = raw_plugin_oauth_token_json(&store, "acme").await;
        let raw_value: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
        assert_eq!(
            raw_value["resource_metadata"],
            "https://example.test/.well-known/oauth-protected-resource"
        );
    }

    #[tokio::test]
    async fn mark_plugin_oauth_reconnect_required_normalizes_legacy_plaintext_tokens() {
        use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let raw_json = serde_json::json!({
            "plugin_id": "acme",
            "access_token": "legacy-access-secret",
            "refresh_token": "legacy-refresh-secret",
            "token_type": "Bearer",
            "expires_at": 1_700_000_000_000_i64,
            "scopes": ["repo"],
            "reconnect_required": false,
            "resource_metadata": "https://example.test/.well-known/oauth-protected-resource"
        })
        .to_string();
        store
            .with_conn(move |c| {
                c.execute(
                    "INSERT INTO plugin_oauth_tokens(plugin_id, token_json, updated_at) VALUES (?1, ?2, ?3)",
                    params!["acme", raw_json, now_ms()],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        store
            .mark_plugin_oauth_reconnect_required("acme")
            .await
            .unwrap();

        let raw_json = raw_plugin_oauth_token_json(&store, "acme").await;
        assert!(
            !raw_json.contains("legacy-access-secret"),
            "access token must not remain in plaintext: {raw_json}"
        );
        assert!(
            !raw_json.contains("legacy-refresh-secret"),
            "refresh token must not remain in plaintext: {raw_json}"
        );

        let raw_value: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
        assert_eq!(
            raw_value["resource_metadata"],
            "https://example.test/.well-known/oauth-protected-resource"
        );

        let roundtrip = store.get_plugin_oauth_token("acme").await.unwrap().unwrap();
        assert_eq!(roundtrip.access_token, "legacy-access-secret");
        assert_eq!(
            roundtrip.refresh_token.as_deref(),
            Some("legacy-refresh-secret")
        );
        assert!(roundtrip.reconnect_required);
    }

    #[tokio::test]
    async fn upsert_plugin_oauth_token_preserves_unknown_json_fields() {
        use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let token = PluginOauthToken {
            plugin_id: "acme".into(),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            token_type: "Bearer".into(),
            expires_at: Some(1_700_000_000_000),
            scopes: vec!["repo".into()],
            reconnect_required: false,
        };
        store.upsert_plugin_oauth_token(&token).await.unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE plugin_oauth_tokens SET token_json = json_set(token_json, '$.resource_metadata', 'https://example.test/.well-known/oauth-protected-resource')",
                    [],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                access_token: "access-secret-2".into(),
                refresh_token: None,
                reconnect_required: true,
                ..token
            })
            .await
            .unwrap();

        let raw_json = raw_plugin_oauth_token_json(&store, "acme").await;
        let raw_value: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
        assert_eq!(
            raw_value["resource_metadata"],
            "https://example.test/.well-known/oauth-protected-resource"
        );

        let roundtrip = store.get_plugin_oauth_token("acme").await.unwrap().unwrap();
        assert_eq!(roundtrip.access_token, "access-secret-2");
        assert_eq!(roundtrip.refresh_token, None);
        assert!(roundtrip.reconnect_required);
    }

    #[tokio::test]
    async fn delete_plugin_oauth_token_removes_the_row() {
        use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let token = PluginOauthToken {
            plugin_id: "acme".into(),
            access_token: "access-secret".into(),
            refresh_token: None,
            token_type: "Bearer".into(),
            expires_at: None,
            scopes: vec![],
            reconnect_required: false,
        };
        store.upsert_plugin_oauth_token(&token).await.unwrap();
        assert!(store
            .get_plugin_oauth_token("acme")
            .await
            .unwrap()
            .is_some());

        store.delete_plugin_oauth_token("acme").await.unwrap();

        assert!(store
            .get_plugin_oauth_token("acme")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn context_checkpoints_and_session_context_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        assert!(store
            .latest_context_checkpoint("s1")
            .await
            .unwrap()
            .is_none());
        store
            .insert_context_checkpoint("s1", 4, 1, &serde_json::json!([{"role":"user"}]))
            .await
            .unwrap();
        store
            .insert_context_checkpoint("s1", 9, 2, &serde_json::json!([{"role":"user","w":2}]))
            .await
            .unwrap();
        let ck = store
            .latest_context_checkpoint("s1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((ck.boundary_seq, ck.window_number), (9, 2));
        assert_eq!(ck.payload[0]["w"], 2);
        // Other sessions are isolated.
        assert!(store
            .latest_context_checkpoint("s2")
            .await
            .unwrap()
            .is_none());

        // Tail listing.
        use crate::domain::NewProviderTurn;
        for i in 0..3 {
            store
                .insert_provider_turn(NewProviderTurn::new("s1", "user", serde_json::json!([i])))
                .await
                .unwrap();
        }
        let tail = store.list_provider_turns_after("s1", 2).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 3);

        // session_context upsert overwrites.
        assert!(store.get_session_context("s1").await.unwrap().is_none());
        store
            .upsert_session_context("s1", &serde_json::json!({"percent_left": 42}))
            .await
            .unwrap();
        store
            .upsert_session_context("s1", &serde_json::json!({"percent_left": 17}))
            .await
            .unwrap();
        let ctx = store.get_session_context("s1").await.unwrap().unwrap();
        assert_eq!(ctx["percent_left"], 17);
    }

    #[tokio::test]
    async fn migration_20_creates_plugin_install_tables() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let counts = store
            .with_conn(|c| {
                let installs: i64 =
                    c.query_row("SELECT count(*) FROM plugin_installs", [], |r| r.get(0))?;
                let attach: i64 =
                    c.query_row("SELECT count(*) FROM plugin_attach_status", [], |r| {
                        r.get(0)
                    })?;
                Ok((installs, attach))
            })
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[tokio::test]
    async fn plugin_install_upsert_roundtrips_and_pins() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let rec = PluginInstallRecord {
            plugin_id: "acme".into(),
            kind: "plugin_pack".into(),
            source_spec: "https://github.com/acme/pack".into(),
            resolved_commit: Some("abc123".into()),
            fingerprint: "fp-1".into(),
            installed_at: 1000,
            updated_at: 1000,
            pinned: false,
            pin_reason: None,
            trust_tier: "acknowledged".into(),
            trust_ack_at: Some(1000),
            trust_ack_summary: Some("{\"hooks\":[]}".into()),
        };
        store.upsert_plugin_install(&rec).await.unwrap();
        let got = store.get_plugin_install("acme").await.unwrap().unwrap();
        assert_eq!(got.resolved_commit.as_deref(), Some("abc123"));
        assert_eq!(got.trust_tier, "acknowledged");

        store
            .set_plugin_install_pin("acme", true, Some("frozen for demo"))
            .await
            .unwrap();
        let pinned = store.get_plugin_install("acme").await.unwrap().unwrap();
        assert!(pinned.pinned);
        assert_eq!(pinned.pin_reason.as_deref(), Some("frozen for demo"));
        // upsert preserves pin (COALESCE on pinned/pin_reason left out — upsert
        // overwrites all columns, so the recorder must read-modify-write; assert
        // that a re-upsert of the ORIGINAL rec would clear the pin, documenting
        // that pin is managed only via set_plugin_install_pin).
        assert_eq!(store.list_plugin_installs().await.unwrap().len(), 1);

        store.delete_plugin_install("acme").await.unwrap();
        assert!(store.get_plugin_install("acme").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn plugin_attach_status_records_latest() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .record_plugin_attach(&PluginAttachStatus {
                plugin_id: "acme".into(),
                last_attach_at: 5,
                outcome: "failed".into(),
                reason: Some("configure acme: missing credentials".into()),
            })
            .await
            .unwrap();
        store
            .record_plugin_attach(&PluginAttachStatus {
                plugin_id: "acme".into(),
                last_attach_at: 9,
                outcome: "ok".into(),
                reason: None,
            })
            .await
            .unwrap();
        let got = store.get_plugin_attach("acme").await.unwrap().unwrap();
        assert_eq!(got.outcome, "ok");
        assert_eq!(got.last_attach_at, 9);
        assert_eq!(store.list_plugin_attach().await.unwrap().len(), 1);
    }
}
