use crate::artifacts::{
    ArtifactAccessRow, ArtifactCreator, ArtifactListRow, ArtifactRecord, ArtifactReference,
    ArtifactStatus,
};
use crate::domain::{
    AgentIdentitySnapshot, AgentRun, AgentRunKind, AgentRunStatus, Message, NewAgentRun,
    NewMessage, NewProviderTurn, PermMode, Project, ProviderTurn, QueuedSessionPrompt, Session,
    SessionKind, SessionStatus, Surface, ToolPolicyRow,
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
#[cfg(test)]
use std::sync::Arc;

fn migration_24_codex_models(
    tx: &rusqlite::Transaction<'_>,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut models: std::collections::HashSet<String> =
        crate::llm_router::registry::descriptor("openai-oauth")
            .map(|descriptor| {
                descriptor
                    .models
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect()
            })
            .unwrap_or_default();
    let snapshot: Value = serde_json::from_str(include_str!("llm_router/model_meta_snapshot.json"))
        .map_err(to_sql_json_error)?;
    if let Some(entries) = snapshot.as_object() {
        for key in entries.keys() {
            if let Some(model) = key.strip_prefix("provider::openai-oauth::model::") {
                models.insert(model.to_string());
            }
        }
    }
    let mut stmt =
        tx.prepare("SELECT data FROM provider_connections WHERE provider='openai-oauth'")?;
    for raw in stmt.query_map([], |row| row.get::<_, String>(0))? {
        let value: Value = serde_json::from_str(&raw?).unwrap_or(Value::Null);
        if let Some(stored) = value.get("modelsOverride").and_then(Value::as_array) {
            models.extend(stored.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(stored) = value.get("modelMetaOverrides").and_then(Value::as_object) {
            models.extend(stored.keys().cloned());
        }
    }
    Ok(models)
}

fn migration_24_known_codex_model(known: &std::collections::HashSet<String>, model: &str) -> bool {
    known.contains(model)
        || model
            .strip_suffix("-review")
            .is_some_and(|base| known.contains(base))
}

fn migration_24_parse_prefixed(
    value: &str,
    known: &std::collections::HashSet<String>,
) -> Option<(String, String, String)> {
    let (prefix, original_model) = value.split_once('/')?;
    if !matches!(prefix, "openai" | "openai-oauth") {
        return None;
    }
    if original_model.contains('/') || migration_24_known_codex_model(known, original_model) {
        return None;
    }
    let (parsed, effort) = crate::llm_router::model_effort::parse_legacy_codex_selection(value)?;
    let model = parsed.split_once('/')?.1.to_string();
    let canonical = format!("openai/{model}");
    migration_24_known_codex_model(known, &model).then_some((canonical, model, effort))
}

fn migration_24_normalize(tx: &rusqlite::Transaction<'_>) -> rusqlite_migration::HookResult {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_effort_preferences (\
            family TEXT NOT NULL,\
            model TEXT NOT NULL,\
            effort TEXT NOT NULL,\
            PRIMARY KEY (family, model)\
        );",
    )?;
    let known = migration_24_codex_models(tx)?;

    let projects = {
        let mut stmt =
            tx.prepare("SELECT project_id, model, effort FROM projects WHERE model IS NOT NULL")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, model, existing_effort) in projects {
        if let Some((canonical, _, suffix_effort)) = migration_24_parse_prefixed(&model, &known) {
            let effort = existing_effort.or(Some(suffix_effort));
            tx.execute(
                "UPDATE projects SET model=?2, effort=?3 WHERE project_id=?1",
                params![id, canonical, effort],
            )?;
        }
    }

    let default_model: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key='default_model'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(default_model) = default_model {
        if let Some((canonical, model, suffix_effort)) =
            migration_24_parse_prefixed(&default_model, &known)
        {
            tx.execute(
                "UPDATE settings SET value=?1 WHERE key='default_model'",
                params![canonical],
            )?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE key='default_effort'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if existing
                .as_deref()
                .is_none_or(|effort| effort.trim().is_empty())
            {
                tx.execute(
                    "INSERT OR IGNORE INTO model_effort_preferences(family,model,effort) VALUES ('openai',?1,?2)",
                    params![model, suffix_effort],
                )?;
            }
        }
    }

    let routes_raw: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key='llm_model_routes'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(routes_raw) = routes_raw {
        let mut routes: Vec<crate::llm_router::routes::ModelRouteInfo> =
            serde_json::from_str(&routes_raw).map_err(to_sql_json_error)?;
        let mut changed = false;
        for route in &mut routes {
            for target in &mut route.targets {
                if target.provider != "openai" {
                    continue;
                }
                let prefixed = format!("openai/{}", target.model);
                if let Some((canonical, _, suffix_effort)) =
                    migration_24_parse_prefixed(&prefixed, &known)
                {
                    target.model = canonical.split_once('/').unwrap().1.to_string();
                    if target
                        .effort
                        .as_deref()
                        .is_none_or(|effort| effort.trim().is_empty())
                    {
                        target.effort = Some(suffix_effort);
                    }
                    changed = true;
                }
            }
        }
        if changed {
            let serialized = serde_json::to_string(&routes).map_err(to_sql_json_error)?;
            tx.execute(
                "UPDATE settings SET value=?1 WHERE key='llm_model_routes'",
                params![serialized],
            )?;
        }
    }
    Ok(())
}

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
        // rewind-and-replay in `migrations_13_to_41_replay_is_idempotent_and_converges_native_only`,
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
        // Migration 24 — Typed model-effort preferences and normalization of
        // legacy Codex virtual model suffixes. Kept after main's migrations
        // 20–23 so existing released user_version slots retain their meaning.
        // The hook is transactional and convergent, so rewind/replay is safe.
        M::up_with_hook("", migration_24_normalize),
        // Migration 25 — Durable route-selection identity for switch notices.
        M::up(
            "CREATE TABLE IF NOT EXISTS session_route_state (\
                session_pk TEXT PRIMARY KEY NOT NULL,\
                requested_model TEXT NOT NULL,\
                resolved_provider TEXT NOT NULL,\
                resolved_family TEXT NOT NULL,\
                resolved_model TEXT NOT NULL,\
                effective_effort TEXT,\
                connection_id TEXT NOT NULL,\
                updated_at INTEGER NOT NULL\
            )",
        ),
        // Migration 26 — user-owned runtime selection for project-less chats.
        M::up(
            "CREATE TABLE IF NOT EXISTS session_runtime_settings (\
                session_pk TEXT PRIMARY KEY NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,\
                model TEXT,\
                effort TEXT,\
                updated_at INTEGER NOT NULL\
            )",
        ),
        // Migration 27 — Phase 3 background rail (spec §4/§6): durable re-entry
        // channel + jobs.model_override. Appended as the tail across merges with
        // main (24 model-effort normalize, 25 route-state, 26 runtime-settings —
        // none touch a sessions/jobs column this migration reads). Every
        // statement is existence-guarded so the rewind-and-replay migration
        // test's replay on an already-migrated DB is a no-op.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS background_events (\
                    id TEXT PRIMARY KEY NOT NULL,\
                    target_session_pk TEXT NOT NULL,\
                    kind TEXT NOT NULL,\
                    payload TEXT NOT NULL,\
                    created_at INTEGER NOT NULL,\
                    claimed_by TEXT,\
                    delivered_at INTEGER\
                );\
                CREATE INDEX IF NOT EXISTS idx_background_events_target \
                    ON background_events(target_session_pk, delivered_at);",
            )?;
            let has_override = tx
                .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name='model_override'")?
                .exists([])?;
            if !has_override {
                tx.execute("ALTER TABLE jobs ADD COLUMN model_override TEXT", [])?;
            }
            let has_origin_run_id = tx
                .prepare("SELECT 1 FROM pragma_table_info('background_events') WHERE name='origin_run_id'")?
                .exists([])?;
            if !has_origin_run_id {
                tx.execute("ALTER TABLE background_events ADD COLUMN origin_run_id TEXT", [])?;
            }
            tx.execute(
                "CREATE INDEX IF NOT EXISTS idx_background_events_origin \
                 ON background_events(origin_run_id, delivered_at)",
                [],
            )?;
            Ok(())
        }),
        // Migration 28 — Remote catalog cache (origin/main #113).
        // to the tail here since Phase 4/5/6 also appended #28/#29/#30).
        // `plugin_catalog_cache` holds the
        // entries of the last verified signed feed (id, manifest TOML, semver,
        // feed sequence, blocked flag+reason); `catalog_feed_state` is a single
        // KV row tracking the last-accepted sequence + fetch outcome for
        // anti-rollback and status. Appended as the tail after migration 30
        // (Phase 6 audit columns). IF NOT EXISTS: the rewind-and-replay test re-runs
        // this on an already-migrated DB, so it must be a no-op on replay.
        M::up(
            "CREATE TABLE IF NOT EXISTS plugin_catalog_cache (\
                id TEXT PRIMARY KEY NOT NULL,\
                manifest_toml TEXT NOT NULL,\
                version TEXT NOT NULL,\
                sequence INTEGER NOT NULL,\
                blocked INTEGER NOT NULL DEFAULT 0,\
                blocked_reason TEXT,\
                fetched_at INTEGER NOT NULL\
            );\
            CREATE TABLE IF NOT EXISTS catalog_feed_state (\
                id INTEGER PRIMARY KEY CHECK (id = 1),\
                sequence INTEGER NOT NULL,\
                updated_at INTEGER NOT NULL,\
                outcome TEXT NOT NULL\
            );",
        ),
        // Migration 29 — Phase 2 remote pairing: `devices` holds paired remote
        // clients (hashed bearer token); `pairing_codes` holds single-use,
        // TTL-bounded enrollment codes (hashed). IF NOT EXISTS so the replay
        // migration tests re-run this as a no-op. Renumbered to sit after
        // main's migration 28 (remote catalog cache) — this is the new tail
        // established by the merge with main's #110/#112/#113/#114/#115.
        M::up(
            "CREATE TABLE IF NOT EXISTS devices (\
                id TEXT PRIMARY KEY NOT NULL,\
                name TEXT NOT NULL,\
                token_hash TEXT NOT NULL UNIQUE,\
                created_at INTEGER NOT NULL,\
                last_seen INTEGER,\
                revoked INTEGER NOT NULL DEFAULT 0\
            );\
            CREATE TABLE IF NOT EXISTS pairing_codes (\
                code_hash TEXT PRIMARY KEY NOT NULL,\
                expires_at INTEGER NOT NULL\
            );",
        ),
        // Migration 30 — Phase 3 runner registry: extend the `gateways` table
        // (already used for local/wsl/ssh rows) with a `remote` kind that
        // carries a cert fingerprint (base64-std SHA-256 over the paired
        // runner's TLS cert DER, verbatim) and an encrypted device token
        // (`secrets::encrypt_field` — recoverable, NOT hashed, since Cockpit
        // replays it as a bearer; contrast with the `devices` table from
        // migration 29, which hashes). Hook-guarded (SQLite has no ADD
        // COLUMN IF NOT EXISTS) exactly like the jobs.pre_check migration
        // above: replaying this on a DB that already has the columns (the
        // rewind-and-replay migration tests) must be a no-op instead of a
        // "duplicate column" error.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let has_fingerprint = tx
                .prepare("SELECT 1 FROM pragma_table_info('gateways') WHERE name='fingerprint'")?
                .exists([])?;
            if !has_fingerprint {
                tx.execute("ALTER TABLE gateways ADD COLUMN fingerprint TEXT", [])?;
            }
            let has_device_token = tx
                .prepare("SELECT 1 FROM pragma_table_info('gateways') WHERE name='device_token'")?
                .exists([])?;
            if !has_device_token {
                tx.execute("ALTER TABLE gateways ADD COLUMN device_token TEXT", [])?;
            }
            Ok(())
        }),
        // Migration 31 — Phase 4 self-learning (spec §4/§7): cross-session
        // recall (messages_fts + sync triggers), skill telemetry, and curator
        // lifecycle state. All statements existence-guarded so the
        // rewind-and-replay migration tests re-run this as a no-op.
        // messages_fts is a standalone (not external-content) FTS5 table:
        // `messages` has a COMPOSITE PK (session_pk, seq) and no stable integer
        // rowid, so triggers key on that pair and store it UNINDEXED.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            // NOTE: this string uses Rust backslash line-continuation, which
            // deletes both the newline AND the following indentation — the
            // whole statement list becomes ONE line with no embedded `\n`.
            // Every continued line therefore ends with an explicit trailing
            // space (so keywords/identifiers never fuse, e.g. `messages` +
            // `WHEN` would otherwise merge into `messagesWHEN`), and any
            // explanatory text uses `/* */` block comments rather than `--`
            // line comments — a `--` comment has no real newline to stop at
            // here, so it would silently swallow every statement after it.
            tx.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5( \
                    text, \
                    session_pk UNINDEXED, \
                    seq UNINDEXED, \
                    tokenize = 'porter unicode61' \
                 ); \
                 /* Only user/assistant TEXT blocks are searchable; tool calls, \
                    results, notices, and thinking are excluded. payload is a \
                    JSON string, so pull the body with json_extract (JSON1 is \
                    in the bundled amalgamation). */ \
                 CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages \
                 WHEN new.role IN ('user','assistant') AND new.block_type='text' \
                      AND json_extract(new.payload,'$.text') IS NOT NULL \
                 BEGIN \
                     INSERT INTO messages_fts(text, session_pk, seq) \
                     VALUES (json_extract(new.payload,'$.text'), new.session_pk, new.seq); \
                 END; \
                 /* messages text is immutable once written (append-only \
                    ledger; only status/tool_kind mutate, never text), so no \
                    AFTER UPDATE trigger is needed. DELETE keeps the index \
                    consistent when a session is purged. */ \
                 CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages \
                 BEGIN \
                     DELETE FROM messages_fts \
                     WHERE session_pk = old.session_pk AND seq = old.seq; \
                 END; \
                 CREATE TABLE IF NOT EXISTS skill_usage ( \
                     name TEXT PRIMARY KEY NOT NULL, \
                     created_by TEXT, \
                     use_count INTEGER NOT NULL DEFAULT 0, \
                     view_count INTEGER NOT NULL DEFAULT 0, \
                     patch_count INTEGER NOT NULL DEFAULT 0, \
                     last_used_at INTEGER, \
                     last_viewed_at INTEGER, \
                     last_patched_at INTEGER, \
                     state TEXT NOT NULL DEFAULT 'active', \
                     pinned INTEGER NOT NULL DEFAULT 0, \
                     archived_at INTEGER, \
                     created_at INTEGER NOT NULL \
                 ); \
                 CREATE TABLE IF NOT EXISTS curator_state ( \
                     id INTEGER PRIMARY KEY CHECK (id = 1), \
                     last_run_at INTEGER, \
                     last_run_id TEXT \
                 ); \
                 CREATE TABLE IF NOT EXISTS curator_runs ( \
                     id TEXT PRIMARY KEY NOT NULL, \
                     started_at INTEGER NOT NULL, \
                     finished_at INTEGER, \
                     status TEXT NOT NULL, \
                     transitioned INTEGER NOT NULL DEFAULT 0, \
                     consolidated INTEGER NOT NULL DEFAULT 0, \
                     snapshot_path TEXT, \
                     error TEXT, \
                     log TEXT \
                 );",
            )?;
            Ok(())
        }),
        // 32: group-chat orchestration. `messages.speaker` (Phase 2 added
        // `speaker` only to `sessions`, never `messages`); `orch_tasks` gains
        // its home-chat binding, per-child circuit-breaker counters, and the
        // root's accumulated steer note. All additive columns — plain ALTERs,
        // hook-guarded (SQLite has no ADD COLUMN IF NOT EXISTS) so replaying
        // this migration on a DB that already has the columns (e.g. the
        // rewind-and-replay in `migrations_13_to_41_replay_is_idempotent_and_converges_native_only`,
        // which re-runs every migration appended after 13) is a no-op
        // instead of a "duplicate column" error. The orch_tasks block is also
        // guarded on the table's existence because migration 39 removes it.
        // A replay from an already-cleaned database must not try to resurrect
        // that legacy table just to add obsolete columns.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let has_messages_speaker = tx
                .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='speaker'")?
                .exists([])?;
            if !has_messages_speaker {
                tx.execute("ALTER TABLE messages ADD COLUMN speaker TEXT", [])?;
            }
            let has_orch_tasks_table = tx
                .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='orch_tasks'")?
                .exists([])?;
            if has_orch_tasks_table {
                let has_home_session_pk = tx
                    .prepare("SELECT 1 FROM pragma_table_info('orch_tasks') WHERE name='home_session_pk'")?
                    .exists([])?;
                if !has_home_session_pk {
                    tx.execute(
                        "ALTER TABLE orch_tasks ADD COLUMN home_session_pk TEXT",
                        [],
                    )?;
                }
                let has_consecutive_failures = tx
                    .prepare(
                        "SELECT 1 FROM pragma_table_info('orch_tasks') WHERE name='consecutive_failures'",
                    )?
                    .exists([])?;
                if !has_consecutive_failures {
                    tx.execute(
                        "ALTER TABLE orch_tasks ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0",
                        [],
                    )?;
                }
                let has_gave_up = tx
                    .prepare("SELECT 1 FROM pragma_table_info('orch_tasks') WHERE name='gave_up'")?
                    .exists([])?;
                if !has_gave_up {
                    tx.execute(
                        "ALTER TABLE orch_tasks ADD COLUMN gave_up INTEGER NOT NULL DEFAULT 0",
                        [],
                    )?;
                }
                let has_steer_note = tx
                    .prepare("SELECT 1 FROM pragma_table_info('orch_tasks') WHERE name='steer_note'")?
                    .exists([])?;
                if !has_steer_note {
                    tx.execute("ALTER TABLE orch_tasks ADD COLUMN steer_note TEXT", [])?;
                }
            }
            Ok(())
        }),
        // 33: app-control audit needs to record the initiating session and
        // the WriteOrigin. The `audit` table has existed (writerless) since
        // an early migration; these two nullable columns let Phase 6 write
        // app-control rows without overloading the gateway-oriented
        // `gateway`/`conversation_id` columns. Additive columns — plain
        // ALTERs, hook-guarded (SQLite has no ADD COLUMN IF NOT EXISTS) so
        // replaying this migration on a DB that already has the columns
        // (e.g. the rewind-and-replay in
        // `migrations_13_to_41_replay_is_idempotent_and_converges_native_only`,
        // which re-runs every migration appended after 13) is a no-op
        // instead of a "duplicate column" error.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let has_session_pk = tx
                .prepare("SELECT 1 FROM pragma_table_info('audit') WHERE name='session_pk'")?
                .exists([])?;
            if !has_session_pk {
                tx.execute("ALTER TABLE audit ADD COLUMN session_pk TEXT", [])?;
            }
            let has_origin = tx
                .prepare("SELECT 1 FROM pragma_table_info('audit') WHERE name='origin'")?
                .exists([])?;
            if !has_origin {
                tx.execute("ALTER TABLE audit ADD COLUMN origin TEXT", [])?;
            }
            Ok(())
        }),
        // 34: durable per-agent learning queue. `agent_learning_state` holds
        // the per-agent monotonic sequence allocator plus the enqueue-block
        // flag (set when an agent is being deleted); `agent_learning_queue`
        // holds one row per learning event with strict per-agent ordering
        // enforced by UNIQUE(agent_id, sequence). CREATE ... IF NOT EXISTS
        // keeps the rewind-and-replay tests convergent.
        M::up(
            "CREATE TABLE IF NOT EXISTS agent_learning_state (\
                agent_id TEXT PRIMARY KEY NOT NULL,\
                next_sequence INTEGER NOT NULL DEFAULT 1,\
                enqueue_blocked INTEGER NOT NULL DEFAULT 0\
            );\
            CREATE TABLE IF NOT EXISTS agent_learning_queue (\
                event_id TEXT PRIMARY KEY NOT NULL,\
                agent_id TEXT NOT NULL,\
                sequence INTEGER NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL CHECK(status IN ('pending','claimed','delivered')),\
                claimed_by TEXT,\
                claimed_at INTEGER,\
                attempts INTEGER NOT NULL DEFAULT 0,\
                last_error TEXT,\
                created_at INTEGER NOT NULL,\
                delivered_at INTEGER,\
                UNIQUE(agent_id, sequence)\
            );\
            CREATE INDEX IF NOT EXISTS idx_agent_learning_delivery \
                ON agent_learning_queue(agent_id, status, sequence);",
        ),
        // 35: durable per-session prompt queue.
        M::up(
            "CREATE TABLE IF NOT EXISTS session_prompt_queue (\
                id TEXT PRIMARY KEY NOT NULL,\
                session_pk TEXT NOT NULL,\
                position INTEGER NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL CHECK(status IN ('pending','claimed')) DEFAULT 'pending',\
                created_at INTEGER NOT NULL,\
                UNIQUE(session_pk, position)\
            );\
            CREATE INDEX IF NOT EXISTS idx_session_prompt_queue_pending \
                ON session_prompt_queue(session_pk, status, position);",
        ),
        // 36: Automations Hub hook configuration and immutable run history.
        // Runs intentionally do not reference automation_hooks: deleting a hook
        // must retain every historical run and attempt.
        M::up(
            "CREATE TABLE IF NOT EXISTS automation_hooks (\
                id TEXT PRIMARY KEY NOT NULL,\
                name TEXT NOT NULL COLLATE NOCASE UNIQUE,\
                trigger_kind TEXT NOT NULL,\
                action_kind TEXT NOT NULL,\
                enabled INTEGER NOT NULL DEFAULT 1,\
                inbound_path TEXT UNIQUE,\
                config_json TEXT NOT NULL,\
                created_at INTEGER NOT NULL,\
                updated_at INTEGER NOT NULL\
            );\
            CREATE TABLE IF NOT EXISTS automation_hook_runs (\
                id TEXT PRIMARY KEY NOT NULL,\
                hook_id TEXT NOT NULL,\
                status TEXT NOT NULL,\
                envelope_json TEXT NOT NULL,\
                snapshot_json TEXT NOT NULL,\
                session_pk TEXT,\
                error TEXT,\
                attempt_count INTEGER NOT NULL DEFAULT 0,\
                last_http_status INTEGER,\
                queued_at INTEGER NOT NULL,\
                started_at INTEGER,\
                finished_at INTEGER\
            );\
            CREATE INDEX IF NOT EXISTS idx_automation_hook_runs_hook \
                ON automation_hook_runs(hook_id, queued_at DESC);\
            CREATE TABLE IF NOT EXISTS automation_hook_attempts (\
                run_id TEXT NOT NULL,\
                ordinal INTEGER NOT NULL,\
                started_at INTEGER NOT NULL,\
                finished_at INTEGER,\
                http_status INTEGER,\
                error TEXT,\
                PRIMARY KEY(run_id, ordinal),\
                FOREIGN KEY(run_id) REFERENCES automation_hook_runs(id)\
            );",
        ),
        // 37: immutable origin for hook-created sessions. Repeating the queue
        // DDL repairs databases previously opened from the feature branch,
        // where automation migrations occupied v35/v36 before main's queue
        // migration was introduced.
        M::up(
            "CREATE TABLE IF NOT EXISTS session_automation_origins (\
                session_pk TEXT PRIMARY KEY NOT NULL,\
                kind TEXT NOT NULL,\
                hook_id TEXT NOT NULL,\
                run_id TEXT NOT NULL,\
                depth INTEGER NOT NULL\
            );\
            CREATE TABLE IF NOT EXISTS session_prompt_queue (\
                id TEXT PRIMARY KEY NOT NULL,\
                session_pk TEXT NOT NULL,\
                position INTEGER NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL CHECK(status IN ('pending','claimed')) DEFAULT 'pending',\
                created_at INTEGER NOT NULL,\
                UNIQUE(session_pk, position)\
            );\
            CREATE INDEX IF NOT EXISTS idx_session_prompt_queue_pending \
                ON session_prompt_queue(session_pk, status, position);",
        ),
        // 38: session ownership and agent run history. This is deliberately
        // appended after main's v35 queue, v36 automation history, and v37
        // compatibility repair so both released main v37 databases and legacy
        // Plan4 v35 databases converge without losing either feature set.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let has_primary_agent_id = tx
                .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='primary_agent_id'")?
                .exists([])?;
            if !has_primary_agent_id {
                tx.execute("ALTER TABLE sessions ADD COLUMN primary_agent_id TEXT", [])?;
            }
            let has_primary_agent_snapshot = tx
                .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='primary_agent_snapshot'")?
                .exists([])?;
            if !has_primary_agent_snapshot {
                tx.execute("ALTER TABLE sessions ADD COLUMN primary_agent_snapshot TEXT", [])?;
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS agent_runs (\
                   run_id TEXT PRIMARY KEY,\
                   session_pk TEXT NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,\
                   parent_run_id TEXT REFERENCES agent_runs(run_id),\
                   retry_of TEXT REFERENCES agent_runs(run_id),\
                   primary_agent_id TEXT NOT NULL,\
                   executing_agent_id TEXT,\
                   executing_agent_name_snapshot TEXT NOT NULL,\
                   agent_kind TEXT NOT NULL CHECK(agent_kind IN ('primary','main-delegate','subagent')),\
                   task TEXT NOT NULL,\
                   status TEXT NOT NULL CHECK(status IN ('queued','running','completed','failed','cancelled','interrupted')),\
                   started_at INTEGER,\
                   finished_at INTEGER,\
                   tool_count INTEGER NOT NULL DEFAULT 0 CHECK(tool_count >= 0),\
                   resolved_model TEXT,\
                   resolved_effort TEXT,\
                   result TEXT,\
                   error TEXT\
                 );\
                 CREATE INDEX IF NOT EXISTS agent_runs_parent_idx ON agent_runs(session_pk,parent_run_id,started_at);\
                 CREATE INDEX IF NOT EXISTS agent_runs_status_idx ON agent_runs(session_pk,status);\
                 CREATE TABLE IF NOT EXISTS agent_run_messages (\
                   session_pk TEXT NOT NULL,\
                   message_seq INTEGER NOT NULL,\
                   run_id TEXT NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,\
                   PRIMARY KEY(session_pk,message_seq),\
                   FOREIGN KEY(session_pk,message_seq) REFERENCES messages(session_pk,seq) ON DELETE CASCADE\
                 );\
                 CREATE INDEX IF NOT EXISTS agent_run_messages_run_idx ON agent_run_messages(run_id,message_seq);",
            )?;
            Ok(())
        }),
        // 39: one-time removal of superseded single-agent, Learning, and
        // orchestrator state. This cleanup must precede all newer additive
        // migrations so existing v38 databases converge safely.
        M::up_with_hook("", |tx: &rusqlite::Transaction<'_>| {
            let has_background_events = tx
                .prepare(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='background_events'",
                )?
                .exists([])?;
            tx.execute(
                "DELETE FROM settings WHERE key IN (
                    'agent_model',
                    'agent_perm_mode',
                    'agent.max_provider_turns',
                    'agent.auto_continue_budget',
                    'memory.nudge_interval'
                 )",
                [],
            )?;
            if has_background_events {
                tx.execute(
                    "DELETE FROM background_events WHERE kind IN ('learning', 'orch')",
                    [],
                )?;
            }
            tx.execute_batch(
                "DROP TABLE IF EXISTS orch_task_deps;
                 DROP TABLE IF EXISTS orch_tasks;
                 DROP TABLE IF EXISTS skill_usage;
                 DROP TABLE IF EXISTS curator_state;
                 DROP TABLE IF EXISTS curator_runs;",
            )?;
            Ok(())
        }),
        // 40: durable linkage back to the tool call that dispatched a child
        // run. The guards keep the migration convergent for the migration
        // replay test and databases opened from an already-migrated build.
        M::up_with_hook("", |tx: &rusqlite::Transaction| {
            let has_source_tool_call_id = tx
                .prepare(
                    "SELECT 1 FROM pragma_table_info('agent_runs') WHERE name='source_tool_call_id'",
                )?
                .exists([])?;
            if !has_source_tool_call_id {
                tx.execute(
                    "ALTER TABLE agent_runs ADD COLUMN source_tool_call_id TEXT",
                    [],
                )?;
            }
            let has_dispatch_index = tx
                .prepare("SELECT 1 FROM pragma_table_info('agent_runs') WHERE name='dispatch_index'")?
                .exists([])?;
            if !has_dispatch_index {
                tx.execute(
                    "ALTER TABLE agent_runs ADD COLUMN dispatch_index INTEGER CHECK(dispatch_index IS NULL OR dispatch_index >= 0)",
                    [],
                )?;
            }
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS agent_runs_dispatch_idx \
                   ON agent_runs(session_pk,parent_run_id,source_tool_call_id,dispatch_index);",
            )?;
            Ok(())
        }),
        // 41: child agent runs share the session message ledger, but each run
        // owns its tool-call lifecycle through agent_run_messages. The original
        // per-session unique index prevents different runs from recording the
        // same provider tool_call_id, so remove it after the ownership schema
        // is in place. The scoped update query keeps writes unambiguous.
        M::up("DROP INDEX IF EXISTS idx_messages_tool_call;"),
        // 42: task artifact persistence schema. Artifacts are content
        // produced during a session (by the user or an agent run) and can be
        // shared into other sessions via artifact_references. Deliberately no
        // FOREIGN KEY / ON DELETE CASCADE here: artifacts and their
        // references must survive independently of session or agent-run
        // deletion, so ownership is tracked by plain TEXT id columns instead.
        M::up(
            "CREATE TABLE IF NOT EXISTS artifacts (\
               id TEXT PRIMARY KEY,\
               source_session_pk TEXT NOT NULL,\
               source_message_seq INTEGER,\
               source_run_id TEXT,\
               creator TEXT NOT NULL CHECK(creator IN ('user','agent')),\
               creator_id TEXT,\
               name TEXT NOT NULL,\
               description TEXT,\
               content_type TEXT,\
               size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),\
               sha256 TEXT NOT NULL,\
               storage_key TEXT NOT NULL,\
               status TEXT NOT NULL CHECK(status IN ('available','source-archived','deleted')),\
               created_at INTEGER NOT NULL,\
               deleted_at INTEGER\
             );\
             CREATE INDEX IF NOT EXISTS artifacts_source_session_idx \
               ON artifacts(source_session_pk, created_at);\
             CREATE TABLE IF NOT EXISTS artifact_references (\
               id TEXT PRIMARY KEY,\
               artifact_id TEXT NOT NULL,\
               target_session_pk TEXT NOT NULL,\
               shared_from_session_pk TEXT NOT NULL,\
               shared_by TEXT,\
               parent_reference_id TEXT,\
               created_at INTEGER NOT NULL,\
               UNIQUE(artifact_id, target_session_pk)\
             );\
             CREATE INDEX IF NOT EXISTS artifact_references_target_idx \
               ON artifact_references(target_session_pk, created_at);\
             CREATE INDEX IF NOT EXISTS artifact_references_artifact_idx \
               ON artifact_references(artifact_id);\
             CREATE TABLE IF NOT EXISTS artifact_storage_jobs (\
               id TEXT PRIMARY KEY,\
               status TEXT NOT NULL,\
               source_root TEXT NOT NULL,\
               target_root TEXT NOT NULL,\
               total_count INTEGER NOT NULL DEFAULT 0 CHECK(total_count >= 0),\
               completed_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_count >= 0),\
               current_artifact_id TEXT,\
               error TEXT,\
               created_at INTEGER NOT NULL,\
               updated_at INTEGER NOT NULL\
             );",
        ),
    ])
}

pub struct Store {
    pool: Pool,
    #[cfg(test)]
    fail_next_legacy_agent_settings_delete: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_session_prompt_claim_recovery: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    session_prompt_claim_recovery_pause:
        std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

impl Clone for Store {
    fn clone(&self) -> Self {
        Store {
            pool: self.pool.clone(),
            #[cfg(test)]
            fail_next_legacy_agent_settings_delete: std::sync::atomic::AtomicBool::new(
                self.fail_next_legacy_agent_settings_delete
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            #[cfg(test)]
            session_prompt_claim_recovery_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            fail_next_session_prompt_claim_recovery: std::sync::atomic::AtomicBool::new(
                self.fail_next_session_prompt_claim_recovery
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRuntimeSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// One `messages_fts` match, joined against its owning session — the unit
/// the `session_search` native tool's DISCOVERY action returns, and (Task
/// 11) the `search_sessions` RPC method's response for the Cockpit Learning
/// panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct FtsHit {
    pub session_pk: String,
    pub seq: i64,
    pub snippet: String,
    pub title: Option<String>,
    pub kind: String,
    pub created_at: i64,
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

fn map_device_row(r: &Row) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        last_seen: r.get(3)?,
        revoked: r.get::<_, i64>(4)? != 0,
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

/// One row of `plugin_catalog_cache`: an entry from the last verified signed
/// remote catalog feed. `sequence` is the feed's monotonic anti-rollback
/// counter at the time this entry was accepted; `blocked` + `blocked_reason`
/// carry a publisher-issued denylist entry for this plugin id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCatalogRow {
    pub id: String,
    pub manifest_toml: String,
    pub version: String,
    pub sequence: u64,
    pub blocked: bool,
    pub blocked_reason: Option<String>,
    pub fetched_at: i64,
}

/// One row of `devices`: a paired remote client. `id` is caller-assigned;
/// `token_hash` is the SHA-256 of the device's bearer token (hashing happens
/// in the caller — this layer only stores/compares the hash). A revoked
/// device is kept for audit but no longer resolves via
/// `find_device_by_token_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
    pub revoked: bool,
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
        .interact(move |c| -> anyhow::Result<T> {
            // Pooled connections + WAL still return SQLITE_BUSY immediately on
            // write contention (e.g. a request's read racing a detached
            // usage-record / prune write). A busy_timeout makes them wait
            // instead of erroring — otherwise concurrent load surfaces as a 500.
            let _ = c.busy_timeout(std::time::Duration::from_secs(5));
            c.pragma_update(None, "foreign_keys", "ON")?;
            Ok(f(c)?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("db interact failed: {e}"))??;
    Ok(out)
}

/// Refuse a protected write from a non-user origin. This is the storage-layer
/// half of Phase 6's negative space (spec §9.3): the curated app-control
/// surface never exposes these writes, and this guard stops any tool that
/// reaches `Store` directly from performing them.
///
/// `rusqlite::Error::UserFunctionError`/`ModuleError` need the `functions`/
/// `vtab` features this workspace doesn't enable, so — like
/// `to_sql_json_error` above — this reuses `ToSqlConversionFailure` (always
/// available, and `String` already satisfies its boxed-error bound) purely
/// as a carrier for an app-level message.
fn ensure_user_origin(origin: crate::domain::WriteOrigin, what: &str) -> rusqlite::Result<()> {
    if !origin.is_user() {
        return Err(to_sql_json_error(format!(
            "write to {what} is not permitted for {} origin (app-control negative space)",
            origin.as_str()
        )));
    }
    Ok(())
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
        Ok(Store {
            pool,
            #[cfg(test)]
            fail_next_legacy_agent_settings_delete: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_session_prompt_claim_recovery: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            session_prompt_claim_recovery_pause: std::sync::Mutex::new(None),
        })
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

    pub async fn set_setting(
        &self,
        origin: crate::domain::WriteOrigin,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.with_conn(move |c| {
            ensure_user_origin(origin, "settings")?;
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

    /// Update only the permission column. Runtime selection and harness
    /// canonicalization have independent atomic persistence paths.
    pub async fn update_project_perm_mode(
        &self,
        project_id: &str,
        perm_mode: PermMode,
    ) -> anyhow::Result<bool> {
        let project_id = project_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE projects SET perm_mode=?2 WHERE project_id=?1",
                params![project_id, perm_mode.as_str()],
            )
            .map(|changed| changed > 0)
        })
        .await
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
        let primary_agent_snapshot = s
            .primary_agent_snapshot
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk,primary_agent_id,primary_agent_snapshot) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    s.session_pk, s.project_id, s.agent_session_id, s.worktree_path,
                    s.branch, s.title, s.status.as_str(), s.created_at, s.last_active,
                    s.started_by, s.resume_attempts, s.branch_owned, s.perm_mode.as_str(),
                    s.kind.as_str(), s.speaker, s.agent, s.parent_session_pk,
                    s.primary_agent_id, primary_agent_snapshot
                ],
            )
        })
        .await?;
        Ok(())
    }

    pub async fn insert_chat_session_with_runtime(
        &self,
        s: Session,
        model: Option<String>,
        effort: Option<String>,
    ) -> anyhow::Result<()> {
        let updated_at = now_ms();
        let session_pk = s.session_pk.clone();
        let primary_agent_snapshot = s
            .primary_agent_snapshot
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk,primary_agent_id,primary_agent_snapshot) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    s.session_pk, s.project_id, s.agent_session_id, s.worktree_path,
                    s.branch, s.title, s.status.as_str(), s.created_at, s.last_active,
                    s.started_by, s.resume_attempts, s.branch_owned, s.perm_mode.as_str(),
                    s.kind.as_str(), s.speaker, s.agent, s.parent_session_pk,
                    s.primary_agent_id, primary_agent_snapshot
                ],
            )?;
            tx.execute(
                "INSERT INTO session_runtime_settings(session_pk,model,effort,updated_at) VALUES(?1,?2,?3,?4)",
                params![session_pk, model, effort, updated_at],
            )?;
            tx.commit()
        })
        .await
    }

    pub async fn insert_hook_origin(
        &self,
        session_pk: &str,
        origin: &crate::automation::HookOrigin,
    ) -> anyhow::Result<()> {
        let session_pk = session_pk.to_string();
        let origin = origin.clone();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO session_automation_origins(session_pk,kind,hook_id,run_id,depth) VALUES(?1,?2,?3,?4,?5)",
                params![session_pk, origin.kind, origin.hook_id, origin.run_id, origin.depth],
            )?;
            Ok(())
        }).await
    }

    pub async fn hook_origin(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<crate::automation::HookOrigin>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT kind,hook_id,run_id,depth FROM session_automation_origins WHERE session_pk=?1",
                params![session_pk],
                |row| Ok(crate::automation::HookOrigin {
                    kind: row.get(0)?, hook_id: row.get(1)?, run_id: row.get(2)?, depth: row.get(3)?,
                }),
            ).optional()
        })
        .await
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

    /// Clear a session title so the next completed turn generates one.
    pub async fn clear_session_title(&self, pk: &str) -> anyhow::Result<()> {
        let pk = pk.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET title=NULL WHERE session_pk=?1",
                params![pk],
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

    pub async fn list_recent_sessions_for_agent(
        &self,
        agent_id: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Session>> {
        let agent_id = agent_id.to_string();
        let limit = limit.clamp(1, 50);
        self.with_conn(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLS} FROM sessions WHERE primary_agent_id=?1 \
                 ORDER BY last_active DESC, created_at DESC, session_pk DESC LIMIT ?2"
            ))?;
            let rows = stmt
                .query_map(params![agent_id, limit], row_to_session)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
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

    /// Replace the project-wide runtime selection. Unlike
    /// `update_project_prefs`, `None` is an explicit SQL NULL.
    pub async fn update_project_runtime(
        &self,
        project_id: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE projects SET model=?2, effort=?3 WHERE project_id=?1",
                params![project_id, model, effort],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get_model_effort_preference(
        &self,
        key: &crate::llm_router::model_effort::ModelPreferenceKey,
    ) -> anyhow::Result<Option<String>> {
        let key = key.clone();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT effort FROM model_effort_preferences WHERE family=?1 AND model=?2",
                params![key.family, key.model],
                |row| row.get(0),
            )
            .optional()
        })
        .await
    }

    pub async fn list_model_effort_preferences(
        &self,
    ) -> anyhow::Result<Vec<(crate::llm_router::model_effort::ModelPreferenceKey, String)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT family, model, effort FROM model_effort_preferences ORDER BY family, model",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        crate::llm_router::model_effort::ModelPreferenceKey {
                            family: row.get(0)?,
                            model: row.get(1)?,
                        },
                        row.get(2)?,
                    ))
                })?
                .collect();
            rows
        })
        .await
    }

    pub async fn set_model_effort_preference(
        &self,
        key: &crate::llm_router::model_effort::ModelPreferenceKey,
        effort: &str,
    ) -> anyhow::Result<()> {
        let key = key.clone();
        let effort = effort.to_string();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO model_effort_preferences(family,model,effort) VALUES (?1,?2,?3) \
                 ON CONFLICT(family,model) DO UPDATE SET effort=excluded.effort",
                params![key.family, key.model, effort],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn clear_model_effort_preference(
        &self,
        key: &crate::llm_router::model_effort::ModelPreferenceKey,
    ) -> anyhow::Result<()> {
        let key = key.clone();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM model_effort_preferences WHERE family=?1 AND model=?2",
                params![key.family, key.model],
            )
            .map(|_| ())
        })
        .await
    }

    /// Atomically demote `Running → Idle` only if the current status is still `Running`.
    /// A session already marked `Interrupted` or `Ended` is left untouched.
    /// Also resets `resume_attempts` to 0 — a turn that reaches a normal (or
    /// errored-but-demoted) end clears the auto-resume cap.
    pub async fn demote_if_running(&self, pk: &str, last_active: i64) -> anyhow::Result<bool> {
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
            .map(|changed| changed > 0)
        })
        .await
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

    /// Bind a session to a project — the persistence half of `app_projects`'s
    /// "attach" action (Phase 6 spec §9.1). Only the `project_id` column
    /// changes; workspace/branch/worktree are left as-is (a chat session has
    /// none, and a live session's already-built `RunnerDeps.project_id` does
    /// not hot-reload — this only affects a fresh start/resume).
    pub async fn set_session_project(&self, pk: &str, project_id: &str) -> anyhow::Result<()> {
        let pk = pk.to_string();
        let project_id = project_id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE sessions SET project_id=?2 WHERE session_pk=?1",
                params![pk, project_id],
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
                "INSERT INTO messages(session_pk,seq,role,block_type,payload,tool_call_id,status,tool_kind,created_at,speaker) \
                 SELECT ?1, COALESCE(MAX(seq),0)+1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
                 FROM messages WHERE session_pk=?1 \
                 RETURNING seq",
                params![m.session_pk, m.role, m.block_type, payload,
                        m.tool_call_id, m.status, m.tool_kind, created, m.speaker],
                |r| r.get::<_, i64>(0),
            )
        })
        .await
    }

    pub async fn get_session_runtime_settings(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<SessionRuntimeSettings>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT model,effort FROM session_runtime_settings WHERE session_pk=?1",
                params![session_pk],
                |r| {
                    Ok(SessionRuntimeSettings {
                        model: r.get(0)?,
                        effort: r.get(1)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    pub async fn update_session_runtime_settings(
        &self,
        session_pk: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> anyhow::Result<()> {
        let session_pk = session_pk.to_string();
        let updated_at = now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO session_runtime_settings(session_pk,model,effort,updated_at) VALUES(?1,?2,?3,?4) \
                 ON CONFLICT(session_pk) DO UPDATE SET model=excluded.model,effort=excluded.effort,updated_at=excluded.updated_at",
                params![session_pk, model, effort, updated_at],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn observe_session_route(
        &self,
        session_pk: &str,
        selection: &crate::llm_router::provenance::RouteSelection,
    ) -> anyhow::Result<Option<Message>> {
        let session_pk = session_pk.to_string();
        let selection = selection.clone();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let previous = tx
                .query_row(
                    "SELECT requested_model,resolved_provider,resolved_family,resolved_model,effective_effort,connection_id \
                     FROM session_route_state WHERE session_pk=?1",
                    params![session_pk],
                    |r| {
                        Ok(crate::llm_router::provenance::RouteSelection {
                            requested_model: r.get(0)?,
                            resolved_provider_id: r.get(1)?,
                            resolved_family: r.get(2)?,
                            resolved_model: r.get(3)?,
                            resolved_model_display_name: String::new(),
                            effective_effort: r.get(4)?,
                            effective_effort_label: None,
                            connection_id: r.get(5)?,
                            connection_label: String::new(),
                            reason: crate::llm_router::provenance::RouteSelectionReason::Initial,
                        })
                    },
                )
                .optional()?;
            let created_at = now_ms();
            let notice = crate::llm_router::provenance::notice_text(previous.as_ref(), &selection)
                .map(|copy| -> rusqlite::Result<Message> {
                    let payload = serde_json::json!({ "text": copy });
                    let payload_json = serde_json::to_string(&payload).map_err(to_sql_json_error)?;
                    let seq = tx.query_row(
                        "INSERT INTO messages(session_pk,seq,role,block_type,payload,tool_call_id,status,tool_kind,created_at) \
                         SELECT ?1, COALESCE(MAX(seq),0)+1, 'system', 'notice', ?2, NULL, NULL, NULL, ?3 \
                         FROM messages WHERE session_pk=?1 \
                         RETURNING seq",
                        params![session_pk, payload_json, created_at],
                        |r| r.get::<_, i64>(0),
                    )?;
                    tx.execute(
                        "UPDATE sessions SET last_active=?2 WHERE session_pk=?1",
                        params![session_pk, created_at],
                    )?;
                    Ok(Message {
                        session_pk: session_pk.clone(),
                        seq,
                        run_id: None,
                        role: "system".into(),
                        block_type: "notice".into(),
                        payload,
                        tool_call_id: None,
                        status: None,
                        tool_kind: None,
                        created_at,
                        speaker: None,
                    })
                })
                .transpose()?;
            tx.execute(
                "INSERT INTO session_route_state(\
                    session_pk,requested_model,resolved_provider,resolved_family,resolved_model,\
                    effective_effort,connection_id,updated_at\
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                 ON CONFLICT(session_pk) DO UPDATE SET \
                    requested_model=excluded.requested_model,\
                    resolved_provider=excluded.resolved_provider,\
                    resolved_family=excluded.resolved_family,\
                    resolved_model=excluded.resolved_model,\
                    effective_effort=excluded.effective_effort,\
                    connection_id=excluded.connection_id,\
                    updated_at=excluded.updated_at",
                params![
                    session_pk,
                    selection.requested_model,
                    selection.resolved_provider_id,
                    selection.resolved_family,
                    selection.resolved_model,
                    selection.effective_effort,
                    selection.connection_id,
                    created_at,
                ],
            )?;
            tx.commit()?;
            Ok(notice)
        })
        .await
    }

    pub async fn list_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<Message>> {
            let mut stmt = c.prepare(
                "SELECT m.session_pk,m.seq,m.role,m.block_type,m.payload,m.tool_call_id,m.status,m.tool_kind,m.created_at,m.speaker,rm.run_id \
                 FROM messages m \
                 LEFT JOIN agent_run_messages rm ON rm.session_pk=m.session_pk AND rm.message_seq=m.seq \
                 WHERE m.session_pk=?1 ORDER BY m.seq",
            )?;
            let items = stmt
                .query_map(params![session_pk], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    /// List transcript rows that belong to the primary session view. The full
    /// durable ledger remains available through [`Self::list_messages`] for
    /// exports, search, and other run-aware consumers.
    pub async fn list_primary_messages(&self, session_pk: &str) -> anyhow::Result<Vec<Message>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT m.session_pk,m.seq,m.role,m.block_type,m.payload,m.tool_call_id,\
                        m.status,m.tool_kind,m.created_at,m.speaker,rm.run_id \
                 FROM messages m \
                 LEFT JOIN agent_run_messages rm \
                   ON rm.session_pk=m.session_pk AND rm.message_seq=m.seq \
                 LEFT JOIN agent_runs ar ON ar.run_id=rm.run_id \
                 WHERE m.session_pk=?1 \
                   AND (rm.run_id IS NULL OR ar.parent_run_id IS NULL) \
                 ORDER BY m.seq",
            )?;
            let rows = stmt
                .query_map(params![session_pk], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Resolve `pk`'s lineage root: walk `parent_session_pk` up to the
    /// session with no parent. Used to exclude the CALLING session's own
    /// lineage from `session_search` recall — a session should never
    /// "discover" the very conversation it's part of. Empty when `pk` is
    /// unknown (nothing to exclude).
    pub async fn lineage_of(&self, pk: &str) -> anyhow::Result<Vec<String>> {
        let pk = pk.to_string();
        self.with_conn(move |c| -> rusqlite::Result<Vec<String>> {
            let root: Option<String> = c
                .query_row(
                    "WITH RECURSIVE up(pk, parent) AS ( \
                       SELECT session_pk, parent_session_pk FROM sessions WHERE session_pk=?1 \
                       UNION ALL \
                       SELECT s.session_pk, s.parent_session_pk FROM sessions s \
                       JOIN up ON s.session_pk = up.parent) \
                     SELECT pk FROM up WHERE parent IS NULL LIMIT 1",
                    [&pk],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(root.into_iter().collect())
        })
        .await
    }

    /// DISCOVERY over `messages_fts` for the `session_search` native tool
    /// (spec §7.4): match `query`, join `sessions` for title/kind, exclude
    /// worker/review sessions (internal delegation noise) and any hit whose
    /// lineage root is in `exclude_lineage` (the caller's own conversation),
    /// dedup remaining hits by lineage root (one hit per past conversation),
    /// and rank interactive-origin sessions above scheduled ones.
    ///
    /// `query` is always passed as a bound parameter — never interpolated
    /// into the SQL — so it cannot inject outer-SQL syntax. It IS still
    /// parsed as an FTS5 query expression by SQLite itself; a malformed
    /// expression (e.g. an unterminated quote) surfaces as an `Err` here,
    /// which the `session_search` tool turns into a clean tool-result error
    /// instead of panicking.
    pub async fn search_messages_fts(
        &self,
        query: &str,
        exclude_lineage: &[String],
        limit: i64,
    ) -> anyhow::Result<Vec<FtsHit>> {
        let query = query.to_string();
        let exclude: std::collections::HashSet<String> = exclude_lineage.iter().cloned().collect();
        self.with_conn(move |c| -> rusqlite::Result<Vec<FtsHit>> {
            let mut stmt = c.prepare(
                "SELECT f.session_pk, f.seq, \
                        snippet(messages_fts, 0, '[', ']', ' … ', 12) AS snip, \
                        s.title, s.kind, s.started_by, m.created_at \
                 FROM messages_fts f \
                 JOIN sessions s ON s.session_pk = f.session_pk \
                 JOIN messages m ON m.session_pk = f.session_pk AND m.seq = f.seq \
                 WHERE messages_fts MATCH ?1 \
                   AND s.kind NOT IN ('worker','review') \
                 ORDER BY (s.started_by = 'scheduler') ASC, \
                          m.created_at DESC \
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![query, limit], |r| {
                Ok(FtsHit {
                    session_pk: r.get(0)?,
                    seq: r.get(1)?,
                    snippet: r.get(2)?,
                    title: r.get(3)?,
                    kind: r.get(4)?,
                    created_at: r.get(6)?,
                })
            })?;
            // Lineage-dedup: collapse each lineage to its single best (most
            // recent) hit and drop anything in the caller's own lineage.
            let mut seen_roots = std::collections::HashSet::new();
            let mut out = Vec::new();
            for hit in rows {
                let hit = hit?;
                let root: String = c
                    .query_row(
                        "WITH RECURSIVE up(pk, parent) AS ( \
                           SELECT session_pk, parent_session_pk FROM sessions WHERE session_pk=?1 \
                           UNION ALL \
                           SELECT s.session_pk, s.parent_session_pk FROM sessions s \
                           JOIN up ON s.session_pk = up.parent) \
                         SELECT pk FROM up WHERE parent IS NULL LIMIT 1",
                        [&hit.session_pk],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| hit.session_pk.clone());
                if exclude.contains(&root) || !seen_roots.insert(root) {
                    continue;
                }
                out.push(hit);
            }
            Ok(out)
        })
        .await
    }

    /// A `±radius`-message window around `seq` in session `pk`'s ledger, for
    /// the `session_search` tool's `read` action. Reuses `list_messages`
    /// (already ordered by seq) and slices in memory — recall windows are
    /// small and infrequent, so a second indexed query isn't worth it.
    pub async fn messages_window(
        &self,
        pk: &str,
        seq: i64,
        radius: i64,
    ) -> anyhow::Result<Vec<Message>> {
        let all = self.list_messages(pk).await?;
        let lo = seq.saturating_sub(radius);
        let hi = seq.saturating_add(radius);
        Ok(all
            .into_iter()
            .filter(|m| m.seq >= lo && m.seq <= hi)
            .collect())
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

    /// List pending session prompts in FIFO order.
    pub async fn list_session_prompt_queue(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Vec<QueuedSessionPrompt>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT payload FROM session_prompt_queue \
                 WHERE session_pk=?1 AND status='pending' ORDER BY position",
            )?;
            let items = stmt
                .query_map(params![session_pk], |row| {
                    let payload: String = row.get(0)?;
                    serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    /// List session keys with a pending queue head, ordered by that head's FIFO
    /// position and then session key for a deterministic boot drain.
    pub(crate) async fn pending_session_prompt_session_pks(&self) -> anyhow::Result<Vec<String>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT session_pk FROM session_prompt_queue \
                 WHERE status='pending' \
                 GROUP BY session_pk \
                 ORDER BY MIN(position), session_pk",
            )?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>();
            rows
        })
        .await
    }

    /// Add a session prompt at the end of its FIFO queue.
    pub async fn enqueue_session_prompt(&self, prompt: QueuedSessionPrompt) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&prompt)?;
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO session_prompt_queue(id, session_pk, position, payload, created_at) \
                 VALUES (?1, ?2, \
                    (SELECT COALESCE(MAX(position), 0) + 1 FROM session_prompt_queue WHERE session_pk=?2), \
                    ?3, ?4)",
                params![prompt.id, prompt.session_pk, payload, prompt.created_at],
            )?;
            tx.commit()
        })
        .await
    }

    /// Remove every pending or claimed prompt for one session and return their
    /// payloads for queue-owned attachment cleanup.
    pub async fn take_all_session_prompts(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Vec<QueuedSessionPrompt>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let payloads = {
                let mut stmt = tx.prepare(
                    "DELETE FROM session_prompt_queue WHERE session_pk=?1 RETURNING payload",
                )?;
                let rows = stmt.query_map(params![session_pk], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let prompts = payloads
                .into_iter()
                .map(|payload| {
                    serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            tx.commit()?;
            Ok(prompts)
        })
        .await
    }

    /// Remove a pending session prompt and return its payload for cleanup.
    pub async fn take_session_prompt(
        &self,
        session_pk: &str,
        id: &str,
    ) -> anyhow::Result<Option<QueuedSessionPrompt>> {
        let session_pk = session_pk.to_string();
        let id = id.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let payload = tx
                .query_row(
                    "DELETE FROM session_prompt_queue \
                     WHERE session_pk=?1 AND id=?2 AND status='pending' RETURNING payload",
                    params![session_pk, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let prompt = payload
                .map(|payload| {
                    serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .transpose()?;
            tx.commit()?;
            Ok(prompt)
        })
        .await
    }

    /// Remove a pending session prompt owned by `session_pk`.
    pub async fn remove_session_prompt(&self, session_pk: &str, id: &str) -> anyhow::Result<bool> {
        let session_pk = session_pk.to_string();
        let id = id.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "DELETE FROM session_prompt_queue \
                 WHERE session_pk=?1 AND id=?2 AND status='pending'",
                params![session_pk, id],
            )?;
            tx.commit()?;
            Ok(changed > 0)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn pause_next_session_prompt_claim_recovery_for_test(
        &self,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let pause = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        *self.session_prompt_claim_recovery_pause.lock().unwrap() = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn fail_next_session_prompt_claim_recovery_for_test(&self) {
        self.fail_next_session_prompt_claim_recovery
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Recover prompts claimed by a process that died before it could complete
    /// or restore them.
    ///
    /// **BOOT-ONLY:** call this exactly once during daemon startup, before any
    /// gateway or control-plane work begins. Calling it in a live process
    /// would incorrectly return an active continuation's claimed prompt.
    /// Positions are intentionally untouched, so recovered prompts retain FIFO
    /// order. The update is idempotent: a subsequent call returns zero until a
    /// new claim is abandoned.
    pub(crate) async fn recover_abandoned_session_prompt_claims(&self) -> anyhow::Result<usize> {
        #[cfg(test)]
        if self
            .fail_next_session_prompt_claim_recovery
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("injected session prompt claim recovery failure");
        }
        #[cfg(test)]
        let pause = {
            self.session_prompt_claim_recovery_pause
                .lock()
                .unwrap()
                .take()
        };
        #[cfg(test)]
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
        self.with_conn(move |c| {
            c.execute(
                "UPDATE session_prompt_queue SET status='pending' WHERE status='claimed'",
                [],
            )
        })
        .await
    }

    /// Atomically claim the next pending prompt and reserve an idle session for it.
    ///
    /// The status compare-and-set and queue claim share one immediate
    /// transaction, so concurrent clients can never both start a turn for the
    /// same session or advance its FIFO queue out of order.
    pub async fn claim_next_session_prompt_if_idle(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<QueuedSessionPrompt>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let row = tx
                .query_row(
                    "SELECT id, payload FROM session_prompt_queue \
                     WHERE session_pk=?1 AND status='pending' ORDER BY position LIMIT 1",
                    params![session_pk],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let result = match row {
                Some((id, payload)) => {
                    let prompt = serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    let reserved = tx.execute(
                        "UPDATE sessions SET status=?2 WHERE session_pk=?1 AND status=?3",
                        params![
                            session_pk,
                            SessionStatus::Running.as_str(),
                            SessionStatus::Idle.as_str(),
                        ],
                    )?;
                    if reserved == 0 {
                        None
                    } else {
                        let claimed = tx.execute(
                            "UPDATE session_prompt_queue SET status='claimed' \
                             WHERE id=?1 AND status='pending'",
                            params![id],
                        )?;
                        if claimed != 1 {
                            return Err(rusqlite::Error::InvalidQuery);
                        }
                        Some(prompt)
                    }
                }
                None => None,
            };
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// Atomically claim the next pending prompt for one session.
    pub async fn claim_next_session_prompt(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Option<QueuedSessionPrompt>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let row = tx
                .query_row(
                    "SELECT id, payload FROM session_prompt_queue \
                     WHERE session_pk=?1 AND status='pending' ORDER BY position LIMIT 1",
                    params![session_pk],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let result = match row {
                Some((id, payload)) => {
                    tx.execute(
                        "UPDATE session_prompt_queue SET status='claimed' \
                         WHERE id=?1 AND status='pending'",
                        params![id],
                    )?;
                    Some(serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?)
                }
                None => None,
            };
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// Return a claimed prompt to its original FIFO position.
    pub async fn restore_claimed_session_prompt(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE session_prompt_queue SET status='pending' WHERE id=?1 AND status='claimed'",
                params![id],
            )
            .map(|changed| changed > 0)
        })
        .await
    }

    /// Delete a successfully delivered claimed prompt and return it for cleanup.
    pub async fn take_completed_claimed_session_prompt(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<QueuedSessionPrompt>> {
        let id = id.to_string();
        self.with_conn(move |c| {
            let payload = c
                .query_row(
                    "DELETE FROM session_prompt_queue \
                     WHERE id=?1 AND status='claimed' RETURNING payload",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            payload
                .map(|payload| {
                    serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .transpose()
        })
        .await
    }

    /// Delete a successfully delivered claimed prompt.
    pub async fn complete_claimed_session_prompt(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM session_prompt_queue WHERE id=?1 AND status='claimed'",
                params![id],
            )
            .map(|changed| changed > 0)
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
        origin: crate::domain::WriteOrigin,
        project_id: &str,
        tool: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        let decision = decision.to_string();
        self.with_conn(move |c| {
            ensure_user_origin(origin, "tool_policies")?;
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
    pub async fn delete_tool_policy(
        &self,
        origin: crate::domain::WriteOrigin,
        project_id: &str,
        tool: &str,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        self.with_conn(move |c| {
            ensure_user_origin(origin, "tool_policies")?;
            c.execute(
                "DELETE FROM tool_policies WHERE project_id=?1 AND tool=?2",
                params![project_id, tool],
            )
        })
        .await?;
        Ok(())
    }

    /// Record one app-control mutation. `actor` and `origin` both carry the
    /// `WriteOrigin` string (the legacy `actor` column stays populated for the
    /// gateway-audit tooling; `origin` is the Phase-6 column). This accepts
    /// any origin — it records who acted, it is not a guarded setter like
    /// `set_tool_policy`. Reads are never audited — only mutations call this.
    pub async fn record_audit(
        &self,
        origin: crate::domain::WriteOrigin,
        session_pk: Option<&str>,
        tool: &str,
        action: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        let origin_s = origin.as_str().to_string();
        let session_pk = session_pk.map(|s| s.to_string());
        let tool = tool.to_string();
        let action = action.to_string();
        let decision = decision.to_string();
        let at = now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO audit(actor, action, tool, decision, at, session_pk, origin) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![origin_s, action, tool, decision, at, session_pk, origin_s],
            )
        })
        .await?;
        Ok(())
    }

    /// The most recent `limit` app-control audit rows, newest first.
    pub async fn list_audit(&self, limit: u32) -> anyhow::Result<Vec<crate::domain::AuditRow>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, tool, action, decision, \
                        COALESCE(origin, actor, 'user') AS origin, session_pk, at \
                 FROM audit ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt
                .query_map(params![limit], |r| {
                    Ok(crate::domain::AuditRow {
                        id: r.get(0)?,
                        tool: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        action: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        decision: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        origin: r.get(4)?,
                        session_pk: r.get(5)?,
                        at: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Merge `patch` into the tool_call row's payload (SQLite `json_patch`,
    /// so the original `{name, input}` survives an `{output: …}` update),
    /// optionally flip status, and return the row's seq, the merged payload,
    /// and its persisted tool_kind — the caller re-emits all three.
    pub async fn update_run_tool_call(
        &self,
        run_id: &str,
        session_pk: &str,
        tool_call_id: &str,
        status: Option<&str>,
        patch: &serde_json::Value,
    ) -> anyhow::Result<(i64, serde_json::Value, Option<String>)> {
        let run_id = run_id.to_string();
        let session_pk = session_pk.to_string();
        let tool_call_id = tool_call_id.to_string();
        let status = status.map(|s| s.to_string());
        let patch = serde_json::to_string(patch)?;
        let (seq, payload, tool_kind) = self
            .with_conn(move |c| {
                c.query_row(
                    "UPDATE messages \
                     SET payload=json_patch(payload, ?4), status=COALESCE(?5, status) \
                     WHERE session_pk=?2 AND tool_call_id=?3 \
                       AND EXISTS ( \
                           SELECT 1 FROM agent_run_messages rm \
                           WHERE rm.session_pk=messages.session_pk \
                             AND rm.message_seq=messages.seq \
                             AND rm.run_id=?1 \
                       ) \
                     RETURNING seq,payload,tool_kind",
                    params![run_id, session_pk, tool_call_id, patch, status],
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

    #[cfg(test)]
    pub(crate) fn fail_next_legacy_agent_settings_delete(&self) {
        self.fail_next_legacy_agent_settings_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Delete exactly the legacy single-agent settings keys (`agent_model`
    /// and `agent_perm_mode`) in one transaction. Used by agent bootstrap's
    /// first-upgrade/reset cleanup after the registry filesystem commit; no
    /// other settings row is touched.
    pub async fn delete_legacy_agent_settings(&self) -> anyhow::Result<()> {
        #[cfg(test)]
        if self
            .fail_next_legacy_agent_settings_delete
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("injected legacy agent settings cleanup failure");
        }
        self.with_conn(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM settings WHERE key IN ('agent_model', 'agent_perm_mode')",
                [],
            )?;
            tx.commit()
        })
        .await
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

    /// Enqueue a durable background-rail row with no originating run. This
    /// preserves the generic new-user-turn delivery semantics for jobs and
    /// legacy producers.
    pub async fn enqueue_background_event(
        &self,
        target_session_pk: &str,
        kind: &str,
        payload: &str,
    ) -> anyhow::Result<String> {
        self.enqueue_background_event_for_run(target_session_pk, None, kind, payload)
            .await
    }

    /// Enqueue a durable background delegation outcome bound to the primary
    /// run that dispatched it. The rail drainer attaches this to that run
    /// instead of opening a new primary user turn.
    pub async fn enqueue_background_delegation_event(
        &self,
        target_session_pk: &str,
        origin_run_id: &str,
        payload: &str,
    ) -> anyhow::Result<String> {
        self.enqueue_background_event_for_run(
            target_session_pk,
            Some(origin_run_id),
            crate::domain::BackgroundKind::Delegation.as_str(),
            payload,
        )
        .await
    }

    async fn enqueue_background_event_for_run(
        &self,
        target_session_pk: &str,
        origin_run_id: Option<&str>,
        kind: &str,
        payload: &str,
    ) -> anyhow::Result<String> {
        let id = crate::paths::new_id();
        let (id2, target, origin, kind, payload, now) = (
            id.clone(),
            target_session_pk.to_string(),
            origin_run_id.map(str::to_string),
            kind.to_string(),
            payload.to_string(),
            crate::paths::now_ms(),
        );
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO background_events(id, target_session_pk, origin_run_id, kind, payload, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id2, target, origin, kind, payload, now],
            )
            .map(|_| ())
        })
        .await?;
        Ok(id)
    }

    /// Atomically claim the OLDEST undelivered, unclaimed rail row whose target
    /// session is IDLE (the idle-only invariant, spec §6.1). Returns `None`
    /// when nothing is deliverable. The claim + read run in one transaction so
    /// two drainers never claim the same row.
    ///
    /// Excludes `kind='learning'` rows (spec §3.1/§7.2 rail split): a
    /// learning fork is never delivered as a chat user turn — it is claimed
    /// separately by [`Store::claim_learning_event`] and driven by the
    /// dedicated learning worker (`learning.rs`), never by this generic
    /// drainer.
    pub async fn claim_deliverable_background_event(
        &self,
        claimer: &str,
    ) -> anyhow::Result<Option<crate::domain::BackgroundEvent>> {
        let claimer = claimer.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            let picked: Option<String> = tx
                .query_row(
                    "SELECT be.id FROM background_events be \
                     JOIN sessions s ON s.session_pk = be.target_session_pk \
                     WHERE be.delivered_at IS NULL AND be.claimed_by IS NULL \
                       AND be.kind != 'learning' \
                       AND s.status = 'idle' \
                     ORDER BY be.created_at ASC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let Some(id) = picked else {
                return Ok(None);
            };
            tx.execute(
                "UPDATE background_events SET claimed_by = ?2 WHERE id = ?1",
                params![id, claimer],
            )?;
            let row = tx.query_row(
                "SELECT id, target_session_pk, origin_run_id, kind, payload, created_at, claimed_by, delivered_at \
                 FROM background_events WHERE id = ?1",
                params![id],
                |r| {
                    Ok(crate::domain::BackgroundEvent {
                        id: r.get(0)?,
                        target_session_pk: r.get(1)?,
                        origin_run_id: r.get(2)?,
                        kind: r.get(3)?,
                        payload: r.get(4)?,
                        created_at: r.get(5)?,
                        claimed_by: r.get(6)?,
                        delivered_at: r.get(7)?,
                    })
                },
            )?;
            tx.commit()?;
            Ok(Some(row))
        })
        .await
    }

    /// Atomically claim the OLDEST undelivered, unclaimed `kind='learning'`
    /// row (spec §3.1/§7.2), for the dedicated learning worker
    /// (`learning.rs`) — the counterpart to
    /// [`Store::claim_deliverable_background_event`], which excludes these
    /// rows. Deliberately has NO idle-target filter: a learning fork drives
    /// an isolated review session, not the target chat's turn, so it must
    /// run regardless of whether the parent chat is idle or mid-turn. The
    /// claim + read run in one transaction so two learning workers never
    /// claim the same row.
    pub async fn claim_learning_event(
        &self,
        claimer: &str,
    ) -> anyhow::Result<Option<crate::domain::BackgroundEvent>> {
        let claimer = claimer.to_string();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            let picked: Option<String> = tx
                .query_row(
                    "SELECT id FROM background_events \
                     WHERE kind = 'learning' AND delivered_at IS NULL AND claimed_by IS NULL \
                     ORDER BY created_at ASC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let Some(id) = picked else {
                return Ok(None);
            };
            tx.execute(
                "UPDATE background_events SET claimed_by = ?2 WHERE id = ?1",
                params![id, claimer],
            )?;
            let row = tx.query_row(
                "SELECT id, target_session_pk, origin_run_id, kind, payload, created_at, claimed_by, delivered_at \
                 FROM background_events WHERE id = ?1",
                params![id],
                |r| {
                    Ok(crate::domain::BackgroundEvent {
                        id: r.get(0)?,
                        target_session_pk: r.get(1)?,
                        origin_run_id: r.get(2)?,
                        kind: r.get(3)?,
                        payload: r.get(4)?,
                        created_at: r.get(5)?,
                        claimed_by: r.get(6)?,
                        delivered_at: r.get(7)?,
                    })
                },
            )?;
            tx.commit()?;
            Ok(Some(row))
        })
        .await
    }

    /// Mark a claimed rail row delivered (its user turn has been injected).
    pub async fn mark_background_delivered(&self, id: &str) -> anyhow::Result<()> {
        let (id, now) = (id.to_string(), crate::paths::now_ms());
        self.with_conn(move |c| {
            c.execute(
                "UPDATE background_events SET delivered_at = ?2 WHERE id = ?1",
                params![id, now],
            )
            .map(|_| ())
        })
        .await
    }

    /// Release a claim so the row is retried next tick (target went busy, or
    /// delivery errored). Never touches `delivered_at`.
    pub async fn release_background_claim(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE background_events SET claimed_by = NULL WHERE id = ?1 AND delivered_at IS NULL",
                params![id],
            )
            .map(|_| ())
        })
        .await
    }

    /// Remove every pending rail row targeting a session (session-end cascade,
    /// spec §6.1: orphaned background work must not leak into a new chat).
    /// Delivered rows are kept as an audit trail. Returns the count removed.
    pub async fn delete_background_events_for_session(
        &self,
        target_session_pk: &str,
    ) -> anyhow::Result<u64> {
        let target = target_session_pk.to_string();
        self.with_conn(move |c| {
            Ok(c.execute(
                "DELETE FROM background_events WHERE target_session_pk = ?1 AND delivered_at IS NULL",
                params![target],
            )? as u64)
        })
        .await
    }

    #[cfg(test)]
    pub async fn pending_background_count(&self) -> anyhow::Result<i64> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM background_events WHERE delivered_at IS NULL",
                [],
                |r| r.get(0),
            )
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

    /// Replace the entire cached remote catalog with `rows` in one
    /// transaction. Called after a signed feed fetch verifies successfully;
    /// an empty slice clears the cache.
    pub async fn upsert_remote_catalog(&self, rows: &[RemoteCatalogRow]) -> anyhow::Result<()> {
        let rows = rows.to_vec();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM plugin_catalog_cache", [])?;
            for r in &rows {
                tx.execute(
                    "INSERT INTO plugin_catalog_cache(id, manifest_toml, version, sequence, \
                         blocked, blocked_reason, fetched_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        r.id,
                        r.manifest_toml,
                        r.version,
                        r.sequence as i64,
                        r.blocked as i64,
                        r.blocked_reason,
                        r.fetched_at
                    ],
                )?;
            }
            tx.commit()
        })
        .await
    }

    pub async fn list_remote_catalog(&self) -> anyhow::Result<Vec<RemoteCatalogRow>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, manifest_toml, version, sequence, blocked, blocked_reason, fetched_at \
                 FROM plugin_catalog_cache ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(RemoteCatalogRow {
                        id: r.get(0)?,
                        manifest_toml: r.get(1)?,
                        version: r.get(2)?,
                        sequence: r.get::<_, i64>(3)? as u64,
                        blocked: r.get::<_, i64>(4)? != 0,
                        blocked_reason: r.get(5)?,
                        fetched_at: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Last-accepted feed sequence, or 0 if no feed has ever been accepted.
    /// Used by the anti-rollback check before applying a newly fetched feed.
    pub async fn get_catalog_feed_sequence(&self) -> anyhow::Result<u64> {
        Ok(self
            .get_catalog_feed_state()
            .await?
            .map(|(seq, _, _)| seq)
            .unwrap_or(0))
    }

    /// Returns `(sequence, updated_at, outcome)` for the single
    /// `catalog_feed_state` row, or `None` if a feed fetch has never
    /// completed.
    pub async fn get_catalog_feed_state(&self) -> anyhow::Result<Option<(u64, i64, String)>> {
        self.with_conn(move |c| {
            c.query_row(
                "SELECT sequence, updated_at, outcome FROM catalog_feed_state WHERE id=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
        })
        .await
    }

    /// Record the outcome of a feed fetch attempt (`"ok"`, or an error
    /// classification) alongside the sequence that was accepted or last
    /// known-good.
    pub async fn set_catalog_feed_state(&self, sequence: u64, outcome: &str) -> anyhow::Result<()> {
        let outcome = outcome.to_string();
        let updated_at = now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO catalog_feed_state(id, sequence, updated_at, outcome) \
                 VALUES (1, ?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET \
                   sequence=excluded.sequence, updated_at=excluded.updated_at, outcome=excluded.outcome",
                params![sequence as i64, updated_at, outcome],
            )
            .map(|_| ())
        })
        .await
    }

    /// Register a newly paired remote device. `token_hash` is already the
    /// SHA-256 of the device's bearer token — this layer never sees the
    /// plaintext token.
    pub async fn insert_device(
        &self,
        id: &str,
        name: &str,
        token_hash: &str,
    ) -> anyhow::Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let token_hash = token_hash.to_string();
        let created_at = now_ms();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO devices(id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, token_hash, created_at],
            )
            .map(|_| ())
        })
        .await
    }

    /// Resolve a device by its hashed bearer token. Revoked devices never
    /// resolve here (they still appear in `list_devices` for audit).
    pub async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> anyhow::Result<Option<Device>> {
        let token_hash = token_hash.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT id, name, created_at, last_seen, revoked FROM devices \
                 WHERE token_hash = ?1 AND revoked = 0",
                params![token_hash],
                map_device_row,
            )
            .optional()
        })
        .await
    }

    pub async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, name, created_at, last_seen, revoked FROM devices ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], map_device_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Revoke a device by id. Returns `true` iff a row was found and revoked.
    pub async fn revoke_device(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.to_string();
        let rows = self
            .with_conn(move |c| {
                c.execute(
                    "UPDATE devices SET revoked = 1 WHERE id = ?1 AND revoked = 0",
                    params![id],
                )
            })
            .await?;
        Ok(rows == 1)
    }

    pub async fn touch_device_last_seen(&self, id: &str, now_ms: i64) -> anyhow::Result<()> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.execute(
                "UPDATE devices SET last_seen = ?2 WHERE id = ?1",
                params![id, now_ms],
            )
            .map(|_| ())
        })
        .await
    }

    /// Insert a single-use, TTL-bounded pairing code. `code_hash` is already
    /// hashed — this layer never sees the plaintext enrollment code.
    pub async fn insert_pairing_code(
        &self,
        code_hash: &str,
        expires_at: i64,
    ) -> anyhow::Result<()> {
        let code_hash = code_hash.to_string();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO pairing_codes(code_hash, expires_at) VALUES (?1, ?2)",
                params![code_hash, expires_at],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn insert_primary_agent_run(&self, run: NewAgentRun) -> anyhow::Result<AgentRun> {
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            validate_agent_run(&tx, &run, true)?;
            let stored = insert_agent_run_row(&tx, run)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    pub async fn insert_agent_run(&self, run: NewAgentRun) -> anyhow::Result<AgentRun> {
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            validate_agent_run(&tx, &run, false)?;
            let stored = insert_agent_run_row(&tx, run)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    pub async fn insert_owned_session_with_primary_run(
        &self,
        mut session: Session,
        identity: AgentIdentitySnapshot,
        run: NewAgentRun,
    ) -> anyhow::Result<AgentRun> {
        if session.session_pk != run.session_pk
            || run.agent_kind != AgentRunKind::Primary
            || run.parent_run_id.is_some()
            || run.retry_of.is_some()
            || run.primary_agent_id != identity.id
            || run.executing_agent_id.as_deref() != Some(identity.id.as_str())
        {
            anyhow::bail!("owned session and primary run do not form a valid root");
        }
        session.primary_agent_id = Some(identity.id.clone());
        session.primary_agent_snapshot = Some(identity);
        let primary_agent_snapshot = session
            .primary_agent_snapshot
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO sessions(session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk,primary_agent_id,primary_agent_snapshot) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    session.session_pk, session.project_id, session.agent_session_id,
                    session.worktree_path, session.branch, session.title, session.status.as_str(),
                    session.created_at, session.last_active, session.started_by,
                    session.resume_attempts, session.branch_owned, session.perm_mode.as_str(),
                    session.kind.as_str(), session.speaker, session.agent, session.parent_session_pk,
                    session.primary_agent_id, primary_agent_snapshot
                ],
            )?;
            validate_agent_run(&tx, &run, true)?;
            let stored = insert_agent_run_row(&tx, run)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    pub async fn get_agent_run(&self, run_id: &str) -> anyhow::Result<Option<AgentRun>> {
        let run_id = run_id.to_string();
        self.with_conn(move |c| {
            let query = format!("SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE run_id=?1");
            c.query_row(&query, params![run_id], row_to_agent_run)
                .optional()
        })
        .await
    }

    pub async fn list_session_agent_runs(&self, session_pk: &str) -> anyhow::Result<Vec<AgentRun>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let query = format!(
                "SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE session_pk=?1 ORDER BY rowid"
            );
            let mut stmt = c.prepare(&query)?;
            let rows = stmt
                .query_map(params![session_pk], row_to_agent_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn root_agent_run_id(&self, run_id: &str) -> anyhow::Result<Option<String>> {
        let run_id = run_id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                "WITH RECURSIVE ancestors(run_id, parent_run_id) AS (SELECT run_id, parent_run_id FROM agent_runs WHERE run_id=?1 UNION ALL SELECT parent.run_id, parent.parent_run_id FROM agent_runs parent JOIN ancestors child ON child.parent_run_id=parent.run_id) SELECT run_id FROM ancestors WHERE parent_run_id IS NULL LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
        })
        .await
    }

    pub async fn list_descendant_agent_runs(
        &self,
        root_run_id: &str,
    ) -> anyhow::Result<Vec<AgentRun>> {
        let root_run_id = root_run_id.to_string();
        self.with_conn(move |c| {
            let query = format!(
                "WITH RECURSIVE descendants AS (SELECT * FROM agent_runs WHERE parent_run_id=?1 UNION ALL SELECT child.* FROM agent_runs child JOIN descendants parent ON child.parent_run_id=parent.run_id) SELECT {AGENT_RUN_COLS} FROM descendants ORDER BY started_at, run_id"
            );
            let mut stmt = c.prepare(&query)?;
            let rows = stmt
                .query_map(params![root_run_id], row_to_agent_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await
    }

    pub async fn transition_agent_run(
        &self,
        run_id: &str,
        allowed_from: &[AgentRunStatus],
        to: AgentRunStatus,
        result: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        let run_id = run_id.to_string();
        let allowed_from = allowed_from
            .iter()
            .map(|status| status.as_db().to_string())
            .collect::<Vec<_>>();
        let result = result.map(str::to_string);
        let error = error.map(str::to_string);
        let at = now_ms();
        self.with_conn(move |c| {
            let mut conditions = String::from("status IN (");
            conditions.push_str(&std::iter::repeat_n("?", allowed_from.len()).collect::<Vec<_>>().join(","));
            conditions.push_str(") AND status NOT IN ('completed','failed','cancelled','interrupted')");
            let mut params = vec![
                rusqlite::types::Value::Text(to.as_db().to_string()),
                rusqlite::types::Value::Integer(at),
                rusqlite::types::Value::Integer(to.is_terminal().into()),
                result.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
                error.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
                rusqlite::types::Value::Text(run_id),
            ];
            params.extend(allowed_from.into_iter().map(rusqlite::types::Value::Text));
            c.execute(
                &format!(
                    "UPDATE agent_runs SET status=?1, started_at=CASE WHEN ?1='running' AND started_at IS NULL THEN ?2 ELSE started_at END, finished_at=CASE WHEN ?3=1 AND finished_at IS NULL THEN ?2 ELSE finished_at END, result=?4, error=?5 WHERE run_id=?6 AND {conditions}"
                ),
                rusqlite::params_from_iter(params),
            )
            .map(|updated| updated == 1)
        })
        .await
    }

    pub async fn increment_agent_run_tool_count(&self, run_id: &str) -> anyhow::Result<()> {
        let run_id = run_id.to_string();
        self.with_conn(move |c| {
            if c.execute(
                "UPDATE agent_runs SET tool_count=tool_count+1 WHERE run_id=?1",
                params![run_id],
            )? == 0
            {
                return Err(to_sql_json_error("unknown agent run"));
            }
            Ok(())
        })
        .await
    }

    pub async fn interrupt_incomplete_agent_runs(
        &self,
        reason: &str,
    ) -> anyhow::Result<Vec<AgentRun>> {
        let reason = reason.to_string();
        let at = now_ms();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            let query = format!(
                "SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE status IN ('queued','running') ORDER BY rowid"
            );
            let mut stmt = tx.prepare(&query)?;
            let mut runs = stmt
                .query_map([], row_to_agent_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            tx.execute(
                "UPDATE agent_runs SET status='interrupted', finished_at=?1, error=?2 WHERE status IN ('queued','running')",
                params![at, reason.clone()],
            )?;
            for run in &mut runs {
                run.status = AgentRunStatus::Interrupted;
                run.finished_at = Some(at);
                run.error = Some(reason.clone());
            }
            tx.commit()?;
            Ok(runs)
        })
        .await
    }

    pub async fn insert_run_message(
        &self,
        run_id: &str,
        message: NewMessage,
    ) -> anyhow::Result<i64> {
        let run_id = run_id.to_string();
        let payload = serde_json::to_string(&message.payload)?;
        let created = now_ms();
        self.with_conn(move |c| {
            let tx = c.transaction()?;
            let session_pk: String = tx.query_row("SELECT session_pk FROM agent_runs WHERE run_id=?1", params![run_id], |r| r.get(0))?;
            if session_pk != message.session_pk {
                return Err(to_sql_json_error("run and message sessions differ"));
            }
            let seq = tx.query_row(
                "INSERT INTO messages(session_pk,seq,role,block_type,payload,tool_call_id,status,tool_kind,created_at,speaker) SELECT ?1,COALESCE(MAX(seq),0)+1,?2,?3,?4,?5,?6,?7,?8,?9 FROM messages WHERE session_pk=?1 RETURNING seq",
                params![message.session_pk, message.role, message.block_type, payload, message.tool_call_id, message.status, message.tool_kind, created, message.speaker],
                |r| r.get::<_, i64>(0),
            )?;
            tx.execute("INSERT INTO agent_run_messages(session_pk,message_seq,run_id) VALUES (?1,?2,?3)", params![session_pk, seq, run_id])?;
            tx.commit()?;
            Ok(seq)
        }).await
    }

    pub async fn list_run_messages(
        &self,
        session_pk: &str,
        run_id: &str,
    ) -> anyhow::Result<Vec<Message>> {
        let session_pk = session_pk.to_string();
        let run_id = run_id.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare("SELECT m.session_pk,m.seq,m.role,m.block_type,m.payload,m.tool_call_id,m.status,m.tool_kind,m.created_at,m.speaker,rm.run_id FROM messages m JOIN agent_run_messages rm ON rm.session_pk=m.session_pk AND rm.message_seq=m.seq WHERE rm.session_pk=?1 AND rm.run_id=?2 ORDER BY m.seq")?;
            let rows = stmt
                .query_map(params![session_pk, run_id], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await
    }

    /// Atomically consume a pairing code: deletes it iff it exists and has
    /// not expired, returning whether it was consumed. A code can only ever
    /// be consumed once (the row is gone after the first success).
    pub async fn consume_pairing_code(&self, code_hash: &str, now_ms: i64) -> anyhow::Result<bool> {
        let code_hash = code_hash.to_string();
        let rows_affected = self
            .with_conn(move |c| {
                c.execute(
                    "DELETE FROM pairing_codes WHERE code_hash = ?1 AND expires_at > ?2",
                    params![code_hash, now_ms],
                )
            })
            .await?;
        Ok(rows_affected == 1)
    }

    /// Persist a newly produced task artifact. `size_bytes` is stored as a
    /// SQLite INTEGER (signed 64-bit); a `u64` value that does not fit in
    /// `i64` is rejected before the write is attempted.
    pub async fn insert_artifact(&self, artifact: &ArtifactRecord) -> anyhow::Result<()> {
        let size_bytes = i64::try_from(artifact.size_bytes).map_err(|_| {
            anyhow::anyhow!(
                "artifact size_bytes {} exceeds representable range",
                artifact.size_bytes
            )
        })?;
        let artifact = artifact.clone();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO artifacts(id,source_session_pk,source_message_seq,source_run_id,\
                 creator,creator_id,name,description,content_type,size_bytes,sha256,\
                 storage_key,status,created_at,deleted_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    artifact.id,
                    artifact.source_session_pk,
                    artifact.source_message_seq,
                    artifact.source_run_id,
                    artifact.creator.as_db(),
                    artifact.creator_id,
                    artifact.name,
                    artifact.description,
                    artifact.content_type,
                    size_bytes,
                    artifact.sha256,
                    artifact.storage_key,
                    artifact.status.as_db(),
                    artifact.created_at,
                    artifact.deleted_at,
                ],
            )
            .map(|_| ())
        })
        .await
    }

    /// Share an artifact into another session's scope. The
    /// `UNIQUE(artifact_id, target_session_pk)` index rejects a second
    /// reference of the same artifact into the same session.
    pub async fn insert_artifact_reference(
        &self,
        reference: &ArtifactReference,
    ) -> anyhow::Result<()> {
        let reference = reference.clone();
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO artifact_references(id,artifact_id,target_session_pk,\
                 shared_from_session_pk,shared_by,parent_reference_id,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    reference.id,
                    reference.artifact_id,
                    reference.target_session_pk,
                    reference.shared_from_session_pk,
                    reference.shared_by,
                    reference.parent_reference_id,
                    reference.created_at,
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn artifact_by_id(&self, id: &str) -> anyhow::Result<Option<ArtifactRecord>> {
        let id = id.to_string();
        self.with_conn(move |c| {
            c.query_row(
                &format!("SELECT {ARTIFACT_COLS} FROM artifacts WHERE id=?1"),
                params![id],
                row_to_artifact,
            )
            .optional()
        })
        .await
    }

    /// List a session's artifacts: rows the session originated (`reference`
    /// is `None`) plus artifacts shared into the session via a reference
    /// (`reference` is `Some`). An artifact never appears twice even if it
    /// was both originated by and (re-)shared into the same session.
    pub async fn artifacts_for_session(
        &self,
        session_pk: &str,
    ) -> anyhow::Result<Vec<ArtifactListRow>> {
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let mut rows = Vec::new();
            {
                let mut stmt = c.prepare(&format!(
                    "SELECT {ARTIFACT_COLS} FROM artifacts \
                     WHERE source_session_pk=?1 ORDER BY created_at"
                ))?;
                for artifact in stmt.query_map(params![session_pk], row_to_artifact)? {
                    rows.push(ArtifactListRow {
                        artifact: artifact?,
                        reference: None,
                    });
                }
            }
            {
                let mut stmt = c.prepare(
                    "SELECT a.id,a.source_session_pk,a.source_message_seq,a.source_run_id,\
                     a.creator,a.creator_id,a.name,a.description,a.content_type,a.size_bytes,\
                     a.sha256,a.storage_key,a.status,a.created_at,a.deleted_at,\
                     r.id,r.artifact_id,r.target_session_pk,r.shared_from_session_pk,\
                     r.shared_by,r.parent_reference_id,r.created_at \
                     FROM artifact_references r JOIN artifacts a ON a.id = r.artifact_id \
                     WHERE r.target_session_pk=?1 AND a.source_session_pk <> ?1 \
                     ORDER BY r.created_at",
                )?;
                for row in stmt.query_map(params![session_pk], row_to_artifact_and_reference)? {
                    let (artifact, reference) = row?;
                    rows.push(ArtifactListRow {
                        artifact,
                        reference: Some(reference),
                    });
                }
            }
            Ok(rows)
        })
        .await
    }

    /// Resolve an artifact id or reference id against a caller's session
    /// scope. `artifact_or_reference_id` is first tried as an original
    /// artifact id owned by `session_pk`; if that fails to match, it is
    /// tried as a reference id whose `target_session_pk` is `session_pk`.
    /// Returns `None` if neither scope grants access.
    pub async fn reference_for_session(
        &self,
        artifact_or_reference_id: &str,
        session_pk: &str,
    ) -> anyhow::Result<Option<ArtifactAccessRow>> {
        let id = artifact_or_reference_id.to_string();
        let session_pk = session_pk.to_string();
        self.with_conn(move |c| {
            let original = c
                .query_row(
                    &format!("SELECT {ARTIFACT_COLS} FROM artifacts WHERE id=?1 AND source_session_pk=?2"),
                    params![id, session_pk],
                    row_to_artifact,
                )
                .optional()?;
            if let Some(artifact) = original {
                return Ok(Some(ArtifactAccessRow {
                    artifact,
                    reference: None,
                }));
            }
            let joined = c
                .query_row(
                    "SELECT a.id,a.source_session_pk,a.source_message_seq,a.source_run_id,\
                     a.creator,a.creator_id,a.name,a.description,a.content_type,a.size_bytes,\
                     a.sha256,a.storage_key,a.status,a.created_at,a.deleted_at,\
                     r.id,r.artifact_id,r.target_session_pk,r.shared_from_session_pk,\
                     r.shared_by,r.parent_reference_id,r.created_at \
                     FROM artifact_references r JOIN artifacts a ON a.id = r.artifact_id \
                     WHERE r.id=?1 AND r.target_session_pk=?2",
                    params![id, session_pk],
                    row_to_artifact_and_reference,
                )
                .optional()?;
            Ok(joined.map(|(artifact, reference)| ArtifactAccessRow {
                artifact,
                reference: Some(reference),
            }))
        })
        .await
    }

    /// Mark every non-deleted artifact originated by `source_session_pk` as
    /// deleted, stamping `deleted_at`, and return the updated rows. Intended
    /// for use when a session (and thus its content root) is destroyed.
    pub async fn mark_source_artifacts_deleted(
        &self,
        source_session_pk: &str,
        deleted_at: i64,
    ) -> anyhow::Result<Vec<ArtifactRecord>> {
        let source_session_pk = source_session_pk.to_string();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(&format!(
                "UPDATE artifacts SET status='deleted', deleted_at=?2 \
                 WHERE source_session_pk=?1 AND status <> 'deleted' \
                 RETURNING {ARTIFACT_COLS}"
            ))?;
            let rows = stmt
                .query_map(params![source_session_pk, deleted_at], row_to_artifact)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}

const ARTIFACT_COLS: &str = "id,source_session_pk,source_message_seq,source_run_id,creator,\
    creator_id,name,description,content_type,size_bytes,sha256,storage_key,status,created_at,\
    deleted_at";

fn row_to_artifact(r: &Row) -> rusqlite::Result<ArtifactRecord> {
    let size_bytes: i64 = r.get(9)?;
    Ok(ArtifactRecord {
        id: r.get(0)?,
        source_session_pk: r.get(1)?,
        source_message_seq: r.get(2)?,
        source_run_id: r.get(3)?,
        creator: ArtifactCreator::from_db(&r.get::<_, String>(4)?)?,
        creator_id: r.get(5)?,
        name: r.get(6)?,
        description: r.get(7)?,
        content_type: r.get(8)?,
        size_bytes: u64::try_from(size_bytes).map_err(to_sql_json_error)?,
        sha256: r.get(10)?,
        storage_key: r.get(11)?,
        status: ArtifactStatus::from_db(&r.get::<_, String>(12)?)?,
        created_at: r.get(13)?,
        deleted_at: r.get(14)?,
    })
}

fn row_to_artifact_and_reference(r: &Row) -> rusqlite::Result<(ArtifactRecord, ArtifactReference)> {
    let artifact = row_to_artifact(r)?;
    let reference = ArtifactReference {
        id: r.get(15)?,
        artifact_id: r.get(16)?,
        target_session_pk: r.get(17)?,
        shared_from_session_pk: r.get(18)?,
        shared_by: r.get(19)?,
        parent_reference_id: r.get(20)?,
        created_at: r.get(21)?,
    };
    Ok((artifact, reference))
}

const AGENT_RUN_COLS: &str =
    "run_id,session_pk,parent_run_id,retry_of,source_tool_call_id,dispatch_index,primary_agent_id,executing_agent_id,executing_agent_name_snapshot,agent_kind,task,status,started_at,finished_at,tool_count,resolved_model,resolved_effort,result,error";

fn row_to_agent_run(r: &Row) -> rusqlite::Result<AgentRun> {
    let tool_count: i64 = r.get(14)?;
    Ok(AgentRun {
        run_id: r.get(0)?,
        session_pk: r.get(1)?,
        parent_run_id: r.get(2)?,
        retry_of: r.get(3)?,
        source_tool_call_id: r.get(4)?,
        dispatch_index: r.get(5)?,
        primary_agent_id: r.get(6)?,
        executing_agent_id: r.get(7)?,
        executing_agent_name_snapshot: r.get(8)?,
        agent_kind: AgentRunKind::from_db(&r.get::<_, String>(9)?)?,
        task: r.get(10)?,
        status: AgentRunStatus::from_db(&r.get::<_, String>(11)?)?,
        started_at: r.get(12)?,
        finished_at: r.get(13)?,
        tool_count: u32::try_from(tool_count).map_err(to_sql_json_error)?,
        resolved_model: r.get(15)?,
        resolved_effort: r.get(16)?,
        result: r.get(17)?,
        error: r.get(18)?,
    })
}

fn insert_agent_run_row(
    tx: &rusqlite::Transaction<'_>,
    run: NewAgentRun,
) -> rusqlite::Result<AgentRun> {
    tx.execute(
        "INSERT INTO agent_runs(run_id,session_pk,parent_run_id,retry_of,source_tool_call_id,dispatch_index,primary_agent_id,executing_agent_id,executing_agent_name_snapshot,agent_kind,task,status,resolved_model,resolved_effort) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![run.run_id, run.session_pk, run.parent_run_id, run.retry_of, run.source_tool_call_id, run.dispatch_index, run.primary_agent_id, run.executing_agent_id, run.executing_agent_name_snapshot, run.agent_kind.as_db(), run.task, run.status.as_db(), run.resolved_model, run.resolved_effort],
    )?;
    let query = format!("SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE run_id=?1");
    tx.query_row(&query, params![run.run_id], row_to_agent_run)
}

fn validate_agent_run(
    tx: &rusqlite::Transaction<'_>,
    run: &NewAgentRun,
    root: bool,
) -> rusqlite::Result<()> {
    match (&run.source_tool_call_id, run.dispatch_index) {
        (None, None) => {}
        (Some(source), Some(index)) if !source.trim().is_empty() && index >= 0 => {}
        _ => {
            return Err(to_sql_json_error(
                "agent run dispatch linkage must be a non-empty source id paired with a non-negative index",
            ));
        }
    }
    if root && (run.source_tool_call_id.is_some() || run.dispatch_index.is_some()) {
        return Err(to_sql_json_error(
            "a primary agent run cannot be linked to a dispatch tool call",
        ));
    }
    if root {
        if run.agent_kind != AgentRunKind::Primary
            || run.parent_run_id.is_some()
            || run.retry_of.is_some()
        {
            return Err(to_sql_json_error(
                "a primary agent run must be a parentless, non-retry root",
            ));
        }
        return Ok(());
    }
    if run.agent_kind == AgentRunKind::Primary || run.parent_run_id.is_none() {
        return Err(to_sql_json_error(
            "only a root primary run may be parentless",
        ));
    }
    let parent = tx.query_row(
        "SELECT session_pk,primary_agent_id FROM agent_runs WHERE run_id=?1",
        params![run.parent_run_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    if parent.0 != run.session_pk || parent.1 != run.primary_agent_id {
        return Err(to_sql_json_error(
            "agent run parent belongs to another root",
        ));
    }
    if let Some(retry_of) = &run.retry_of {
        let retry = tx.query_row(
            "SELECT session_pk,parent_run_id,primary_agent_id,source_tool_call_id,dispatch_index,status FROM agent_runs WHERE run_id=?1",
            params![retry_of],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    AgentRunStatus::from_db(&row.get::<_, String>(5)?)?,
                ))
            },
        )?;
        if retry.0 != run.session_pk
            || retry.1 != run.parent_run_id
            || retry.2 != run.primary_agent_id
            || retry.3 != run.source_tool_call_id
            || retry.4 != run.dispatch_index
        {
            return Err(to_sql_json_error(
                "agent run retry must inherit its predecessor's tree and dispatch linkage",
            ));
        }
        if !matches!(
            retry.5,
            AgentRunStatus::Failed | AgentRunStatus::Cancelled | AgentRunStatus::Interrupted
        ) {
            return Err(to_sql_json_error(
                "agent run retry must reference a failed, cancelled, or interrupted predecessor",
            ));
        }
        let has_retry_branch: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_runs WHERE retry_of=?1)",
            params![retry_of],
            |row| row.get(0),
        )?;
        if has_retry_branch {
            return Err(to_sql_json_error(
                "agent run retry predecessor already has a retry branch",
            ));
        }
    }
    Ok(())
}

fn row_to_message(r: &Row) -> rusqlite::Result<Message> {
    let payload: String = r.get(4)?;
    Ok(Message {
        session_pk: r.get(0)?,
        seq: r.get(1)?,
        run_id: r.get(10)?,
        role: r.get(2)?,
        block_type: r.get(3)?,
        payload: serde_json::from_str(&payload).map_err(|error| from_sql_json_error(4, error))?,
        tool_call_id: r.get(5)?,
        status: r.get(6)?,
        tool_kind: r.get(7)?,
        created_at: r.get(8)?,
        speaker: r.get(9)?,
    })
}

const SESSION_COLS: &str =
    "session_pk,project_id,agent_session_id,worktree_path,branch,title,status,created_at,last_active,started_by,resume_attempts,branch_owned,perm_mode,kind,speaker,agent,parent_session_pk,primary_agent_id,primary_agent_snapshot";

fn row_to_session(r: &Row) -> rusqlite::Result<Session> {
    let snapshot: Option<String> = r.get(18)?;
    let primary_agent_snapshot = snapshot
        .map(|raw| serde_json::from_str(&raw).map_err(to_sql_json_error))
        .transpose()?;
    let status: String = r.get(6)?;
    let kind: String = r.get(13)?;
    Ok(Session {
        session_pk: r.get(0)?,
        primary_agent_id: r.get(17)?,
        primary_agent_snapshot,
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
    use crate::domain::{AttachmentRef, NewMessage, PermMode, Project, WriteOrigin};
    use crate::domain::{Session, SessionStatus};
    use crate::llm_router::provenance::{
        RouteFailureCategory, RouteSelection, RouteSelectionReason,
    };
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

    fn agent_run_input(
        run_id: &str,
        parent_run_id: Option<&str>,
        retry_of: Option<&str>,
        agent_kind: AgentRunKind,
        status: AgentRunStatus,
    ) -> NewAgentRun {
        NewAgentRun {
            run_id: run_id.into(),
            session_pk: "s1".into(),
            parent_run_id: parent_run_id.map(str::to_string),
            retry_of: retry_of.map(str::to_string),
            source_tool_call_id: None,
            dispatch_index: None,
            primary_agent_id: "ada".into(),
            executing_agent_id: Some("ada".into()),
            executing_agent_name_snapshot: "Ada".into(),
            agent_kind,
            task: "test dispatch linkage".into(),
            status,
            resolved_model: None,
            resolved_effort: None,
        }
    }

    #[tokio::test]
    async fn automation_tables_exist_after_open() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let tables = store
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT name FROM sqlite_master \
                     WHERE type='table' AND name LIKE 'automation_hook%' ORDER BY name",
                )?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap();

        assert_eq!(
            tables,
            [
                "automation_hook_attempts",
                "automation_hook_runs",
                "automation_hooks"
            ]
        );
    }

    #[tokio::test]
    async fn migration_38_adds_ownership_schema_to_main_v37_database() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE sessions (session_pk TEXT PRIMARY KEY);
                 CREATE TABLE messages (
                    session_pk TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    PRIMARY KEY(session_pk, seq)
                 );
                 CREATE TABLE session_prompt_queue (
                    id TEXT PRIMARY KEY NOT NULL,
                    session_pk TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('pending','claimed')) DEFAULT 'pending',
                    created_at INTEGER NOT NULL,
                    UNIQUE(session_pk, position)
                 );
                 CREATE INDEX idx_session_prompt_queue_pending
                    ON session_prompt_queue(session_pk, status, position);
                 INSERT INTO session_prompt_queue(id, session_pk, position, payload, created_at)
                    VALUES ('queued', 'main-v37', 1, '{\"text\":\"preserve queue\"}', 1);
                 CREATE TABLE automation_hooks (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    trigger_kind TEXT NOT NULL,
                    action_kind TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    inbound_path TEXT UNIQUE,
                    config_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                 );
                 INSERT INTO automation_hooks(id, name, trigger_kind, action_kind, config_json, created_at, updated_at)
                    VALUES ('hook', 'Preserve hook', 'schedule', 'session', '{}', 1, 1);
                 CREATE TABLE automation_hook_runs (
                    id TEXT PRIMARY KEY NOT NULL,
                    hook_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    envelope_json TEXT NOT NULL,
                    snapshot_json TEXT NOT NULL,
                    session_pk TEXT,
                    error TEXT,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_http_status INTEGER,
                    queued_at INTEGER NOT NULL,
                    started_at INTEGER,
                    finished_at INTEGER
                 );
                 INSERT INTO automation_hook_runs(id, hook_id, status, envelope_json, snapshot_json, queued_at)
                    VALUES ('run', 'hook', 'completed', '{}', '{}', 1);
                 CREATE INDEX idx_automation_hook_runs_hook
                    ON automation_hook_runs(hook_id, queued_at DESC);
                 CREATE TABLE automation_hook_attempts (
                    run_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER,
                    http_status INTEGER,
                    error TEXT,
                    PRIMARY KEY(run_id, ordinal),
                    FOREIGN KEY(run_id) REFERENCES automation_hook_runs(id)
                 );
                 INSERT INTO automation_hook_attempts(run_id, ordinal, started_at)
                    VALUES ('run', 1, 1);
                 CREATE TABLE session_automation_origins (
                    session_pk TEXT PRIMARY KEY NOT NULL,
                    kind TEXT NOT NULL,
                    hook_id TEXT NOT NULL,
                    run_id TEXT NOT NULL,
                    depth INTEGER NOT NULL
                 );
                 INSERT INTO session_automation_origins(session_pk, kind, hook_id, run_id, depth)
                    VALUES ('main-v37', 'hook', 'hook', 'run', 0);
                 PRAGMA user_version=37;",
            )
            .unwrap();
        }

        type PreservedRowCounts = (i64, i64, i64, i64, i64);
        type Migration38Snapshot = (i64, Vec<String>, Vec<String>, PreservedRowCounts);

        let upgraded = Store::open(tmp.path()).await.unwrap();
        let (user_version, ownership_columns, agent_run_tables, preserved_rows): Migration38Snapshot = upgraded
            .with_conn(|c| {
                let ownership_columns = c
                    .prepare("SELECT name FROM pragma_table_info('sessions') WHERE name LIKE 'primary_agent_%' ORDER BY name")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let agent_run_tables = c
                    .prepare(
                        "SELECT name FROM sqlite_master \n                         WHERE type='table' AND name IN ('agent_runs', 'agent_run_messages') \n                         ORDER BY name",
                    )?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((
                    c.query_row("PRAGMA user_version", [], |row| row.get(0))?,
                    ownership_columns,
                    agent_run_tables,
                    (
                        c.query_row("SELECT COUNT(*) FROM session_prompt_queue", [], |row| row.get(0))?,
                        c.query_row("SELECT COUNT(*) FROM automation_hooks", [], |row| row.get(0))?,
                        c.query_row("SELECT COUNT(*) FROM automation_hook_runs", [], |row| row.get(0))?,
                        c.query_row("SELECT COUNT(*) FROM automation_hook_attempts", [], |row| row.get(0))?,
                        c.query_row("SELECT COUNT(*) FROM session_automation_origins", [], |row| row.get(0))?,
                    ),
                ))
            })
            .await
            .unwrap();

        assert_eq!(user_version, 42);
        assert_eq!(
            ownership_columns,
            ["primary_agent_id", "primary_agent_snapshot"]
        );
        assert_eq!(agent_run_tables, ["agent_run_messages", "agent_runs"]);
        assert_eq!(preserved_rows, (1, 1, 1, 1, 1));
    }

    #[tokio::test]
    async fn migration_40_adds_nullable_agent_dispatch_linkage() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE agent_runs (
                    run_id TEXT PRIMARY KEY,
                    session_pk TEXT NOT NULL,
                    parent_run_id TEXT
                 );
                 INSERT INTO agent_runs(run_id,session_pk) VALUES ('legacy-run','legacy-session');
                 PRAGMA user_version=38;",
            )
            .unwrap();
        }

        let upgraded = Store::open(tmp.path()).await.unwrap();
        let columns = upgraded
            .with_conn(|c| {
                c.prepare("SELECT name FROM pragma_table_info('agent_runs') ORDER BY cid")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .unwrap();
        assert!(
            columns.iter().any(|column| column == "source_tool_call_id"),
            "migration 40 must add source_tool_call_id: {columns:?}"
        );
        assert!(
            columns.iter().any(|column| column == "dispatch_index"),
            "migration 40 must add dispatch_index: {columns:?}"
        );

        let (linkage, has_index, user_version): ((Option<String>, Option<i64>), bool, i64) =
            upgraded
                .with_conn(|c| {
                    let linkage = c.query_row(
                        "SELECT source_tool_call_id,dispatch_index FROM agent_runs WHERE run_id='legacy-run'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    let has_index = c
                        .prepare("SELECT 1 FROM sqlite_master WHERE type='index' AND name='agent_runs_dispatch_idx'")?
                        .exists([])?;
                    let user_version = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                    Ok((linkage, has_index, user_version))
                })
                .await
                .unwrap();
        assert_eq!(linkage, (None, None));
        assert!(has_index, "migration 40 must add the dispatch lookup index");
        assert_eq!(user_version, 42);

        drop(upgraded);
        let reopened = Store::open(tmp.path()).await.unwrap();
        let reopened_version: i64 = reopened
            .with_conn(|c| c.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(reopened_version, 42);
    }

    #[tokio::test]
    async fn migration_41_removes_session_wide_tool_call_uniqueness() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let (user_version, has_unique_tool_call_index) = store
            .with_conn(|c| {
                let user_version: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                let has_unique_tool_call_index = c
                    .prepare(
                        "SELECT 1 FROM sqlite_master \
                         WHERE type='index' AND name='idx_messages_tool_call'",
                    )?
                    .exists([])?;
                Ok((user_version, has_unique_tool_call_index))
            })
            .await
            .unwrap();

        assert_eq!(user_version, 42);
        assert!(
            !has_unique_tool_call_index,
            "tool call IDs must be reusable by separate agent runs"
        );
    }

    #[tokio::test]
    async fn migration_37_repairs_feature_branch_v36_missing_session_prompt_queue() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "DROP INDEX idx_session_prompt_queue_pending;\
                     DROP TABLE session_prompt_queue;\
                     PRAGMA user_version=36;",
                )
            })
            .await
            .unwrap();
        drop(store);

        let upgraded = Store::open(tmp.path()).await.unwrap();
        let (queue_table, queue_index, user_version): (bool, bool, i64) = upgraded
            .with_conn(|c| {
                let exists = |name: &str, object_type: &str| {
                    c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
                        rusqlite::params![object_type, name],
                        |row| row.get(0),
                    )
                };
                Ok((
                    exists("session_prompt_queue", "table")?,
                    exists("idx_session_prompt_queue_pending", "index")?,
                    c.query_row("PRAGMA user_version", [], |row| row.get(0))?,
                ))
            })
            .await
            .unwrap();

        assert!(queue_table, "v37 must restore the prompt queue table");
        assert!(queue_index, "v37 must restore the prompt queue index");
        assert_eq!(user_version, 42);
    }

    #[tokio::test]
    async fn concurrent_permission_update_preserves_atomic_model_effort() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(Store::open(tmp.path()).await.unwrap());
        store
            .insert_project(Project {
                project_id: "p-permission-race".into(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                model: Some("old-model".into()),
                effort: Some("low".into()),
                perm_mode: PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let runtime_store = store.clone();
        let runtime_barrier = barrier.clone();
        let runtime = tokio::spawn(async move {
            runtime_barrier.wait().await;
            runtime_store
                .update_project_runtime(
                    "p-permission-race",
                    Some("new-model".into()),
                    Some("high".into()),
                )
                .await
                .unwrap();
        });
        let permission_store = store.clone();
        let permission = tokio::spawn(async move {
            barrier.wait().await;
            permission_store
                .update_project_perm_mode("p-permission-race", PermMode::BypassPermissions)
                .await
                .unwrap();
        });
        runtime.await.unwrap();
        permission.await.unwrap();
        let project = store
            .get_project("p-permission-race")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(project.model.as_deref(), Some("new-model"));
        assert_eq!(project.effort.as_deref(), Some("high"));
        assert_eq!(project.perm_mode, PermMode::BypassPermissions);
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
            primary_agent_id: None,
            primary_agent_snapshot: None,
            project_id: Some("p1".into()),
            agent_session_id: None,
            worktree_path: Some("/tmp/wt".into()),
            branch: Some("ryuzi/abcdef01".into()),
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

    fn route_selection() -> RouteSelection {
        RouteSelection {
            requested_model: "sol".into(),
            resolved_provider_id: "openai-oauth".into(),
            resolved_family: "openai".into(),
            resolved_model: "gpt-5.6-sol".into(),
            resolved_model_display_name: "5.6 Sol".into(),
            effective_effort: Some("high".into()),
            effective_effort_label: Some("High".into()),
            connection_id: "connection-a".into(),
            connection_label: "Personal Codex".into(),
            reason: RouteSelectionReason::Initial,
        }
    }

    #[tokio::test]
    async fn migration_25_creates_session_route_state_idempotently() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            let columns = store
                .with_conn(|c| {
                    let mut stmt = c.prepare(
                        "SELECT name FROM pragma_table_info('session_route_state') ORDER BY cid",
                    )?;
                    let columns = stmt
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(columns)
                })
                .await
                .unwrap();
            assert_eq!(
                columns,
                [
                    "session_pk",
                    "requested_model",
                    "resolved_provider",
                    "resolved_family",
                    "resolved_model",
                    "effective_effort",
                    "connection_id",
                    "updated_at",
                ]
            );
            store
                .with_conn(|c| c.pragma_update(None, "user_version", 24))
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let count: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='session_route_state'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn session_runtime_settings_round_trip_independently_from_route_observation() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let mut session = sample_session();
        session.kind = SessionKind::Chat;
        session.project_id = None;
        store.insert_session(session).await.unwrap();
        store
            .update_session_runtime_settings(
                "s1",
                Some("openai/gpt-5.5".into()),
                Some("high".into()),
            )
            .await
            .unwrap();
        assert_eq!(
            store.get_session_runtime_settings("s1").await.unwrap(),
            Some(SessionRuntimeSettings {
                model: Some("openai/gpt-5.5".into()),
                effort: Some("high".into()),
            })
        );
    }

    #[tokio::test]
    async fn session_runtime_foreign_key_rejects_orphans_and_cascades() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(store
            .update_session_runtime_settings("missing", Some("m".into()), None)
            .await
            .is_err());

        let mut session = sample_session();
        session.kind = SessionKind::Chat;
        session.project_id = None;
        store
            .insert_chat_session_with_runtime(session, Some("m".into()), Some("high".into()))
            .await
            .unwrap();
        store
            .with_conn(|c| c.execute("DELETE FROM sessions WHERE session_pk='s1'", []))
            .await
            .unwrap();
        assert_eq!(
            store.get_session_runtime_settings("s1").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn chat_session_and_runtime_insert_roll_back_together() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "CREATE TRIGGER reject_runtime BEFORE INSERT ON session_runtime_settings \
                     BEGIN SELECT RAISE(ABORT, 'reject runtime'); END;",
                )
            })
            .await
            .unwrap();
        let mut session = sample_session();
        session.kind = SessionKind::Chat;
        session.project_id = None;
        assert!(store
            .insert_chat_session_with_runtime(session, Some("m".into()), None)
            .await
            .is_err());
        assert!(store.get_session("s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn session_route_state_first_observation_is_silent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();

        assert_eq!(
            store
                .observe_session_route("s1", &route_selection())
                .await
                .unwrap(),
            None
        );
        assert!(store.list_messages("s1").await.unwrap().is_empty());
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().last_active,
            Some(1)
        );
        let stored: (String, String, String, String, Option<String>, String) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT requested_model,resolved_provider,resolved_family,resolved_model,effective_effort,connection_id FROM session_route_state WHERE session_pk='s1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
            })
            .await
            .unwrap();
        assert_eq!(
            stored,
            (
                "sol".into(),
                "openai-oauth".into(),
                "openai".into(),
                "gpt-5.6-sol".into(),
                Some("high".into()),
                "connection-a".into(),
            )
        );
    }

    #[tokio::test]
    async fn session_route_state_equal_identity_deduplicates_mutable_labels_and_reason() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .observe_session_route("s1", &route_selection())
            .await
            .unwrap();

        let mut renamed = route_selection();
        renamed.requested_model = "friendly-alias".into();
        renamed.resolved_model_display_name = "Renamed Sol".into();
        renamed.effective_effort_label = Some("Maximum".into());
        renamed.connection_label = "Renamed account".into();
        renamed.reason = RouteSelectionReason::Failover(RouteFailureCategory::Quota);
        assert_eq!(
            store.observe_session_route("s1", &renamed).await.unwrap(),
            None
        );
        assert!(store.list_messages("s1").await.unwrap().is_empty());
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().last_active,
            Some(1)
        );
        let requested: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT requested_model FROM session_route_state WHERE session_pk='s1'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(requested, "friendly-alias");
    }

    #[tokio::test]
    async fn session_route_state_change_inserts_notice_and_updates_atomically() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .observe_session_route("s1", &route_selection())
            .await
            .unwrap();

        let mut changed = route_selection();
        changed.connection_id = "connection-b".into();
        changed.connection_label = "Work Codex".into();
        changed.reason = RouteSelectionReason::RoundRobin;
        let notice = store
            .observe_session_route("s1", &changed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notice.session_pk, "s1");
        assert_eq!(notice.seq, 1);
        assert_eq!(notice.role, "system");
        assert_eq!(notice.block_type, "notice");
        assert_eq!(
            notice.payload,
            serde_json::json!({"text": "Account switched to Work Codex · round robin"})
        );
        assert_eq!(notice.tool_call_id, None);
        assert_eq!(notice.status, None);
        assert_eq!(notice.tool_kind, None);
        assert_eq!(
            store.list_messages("s1").await.unwrap(),
            vec![notice.clone()]
        );
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().last_active,
            Some(notice.created_at)
        );
        let state: (String, i64) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT connection_id,updated_at FROM session_route_state WHERE session_pk='s1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .await
            .unwrap();
        assert_eq!(state, ("connection-b".into(), notice.created_at));
    }

    #[tokio::test]
    async fn session_route_state_survives_cold_reopen() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store.insert_session(sample_session()).await.unwrap();
            store
                .observe_session_route("s1", &route_selection())
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let mut changed = route_selection();
        changed.resolved_model = "gpt-5.6-sol-plus".into();
        changed.resolved_model_display_name = "5.6 Sol Plus".into();
        let notice = store
            .observe_session_route("s1", &changed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            notice.payload,
            serde_json::json!({"text": "Switched to 5.6 Sol Plus · High"})
        );
    }

    #[tokio::test]
    async fn session_route_state_message_trigger_failure_rolls_back_state() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .observe_session_route("s1", &route_selection())
            .await
            .unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "CREATE TEMP TRIGGER abort_route_notice BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT, 'route notice rejected'); END;",
                )
            })
            .await
            .unwrap();

        let mut changed = route_selection();
        changed.connection_id = "connection-b".into();
        changed.connection_label = "Work Codex".into();
        assert!(store.observe_session_route("s1", &changed).await.is_err());
        let state: String = store
            .with_conn(|c| {
                c.execute_batch("DROP TRIGGER abort_route_notice")?;
                c.query_row(
                    "SELECT connection_id FROM session_route_state WHERE session_pk='s1'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(state, "connection-a");
        assert!(store.list_messages("s1").await.unwrap().is_empty());
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().last_active,
            Some(1)
        );
        assert!(store
            .observe_session_route("s1", &changed)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn session_route_state_concurrent_store_instances_emit_once() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store_a = Store::open(tmp.path()).await.unwrap();
        let store_b = Store::open(tmp.path()).await.unwrap();
        store_a.insert_session(sample_session()).await.unwrap();
        store_a
            .observe_session_route("s1", &route_selection())
            .await
            .unwrap();
        let mut changed = route_selection();
        changed.connection_id = "connection-b".into();
        changed.connection_label = "Work Codex".into();
        changed.reason = RouteSelectionReason::RoundRobin;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let changed_b = changed.clone();
        let barrier_a = barrier.clone();
        let a = tokio::spawn(async move {
            barrier_a.wait().await;
            store_a.observe_session_route("s1", &changed).await.unwrap()
        });
        let b = tokio::spawn(async move {
            barrier.wait().await;
            store_b
                .observe_session_route("s1", &changed_b)
                .await
                .unwrap()
        });

        let outcomes = [a.await.unwrap(), b.await.unwrap()];
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_some()).count(),
            1
        );
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(store.list_messages("s1").await.unwrap().len(), 1);
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
                // Migration 11 created this graph, then migration 39 removed
                // it as superseded orchestration state. A current store must
                // not resurrect either legacy table.
                for table in ["orch_tasks", "orch_task_deps"] {
                    let exists: bool = c
                        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
                        .exists([table])?;
                    assert!(!exists, "{table} must not survive migration 39");
                }
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
                primary_agent_id: None,
                primary_agent_snapshot: None,
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
    async fn primary_messages_exclude_child_owned_rows() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .insert_primary_agent_run(agent_run_input(
                "root",
                None,
                None,
                AgentRunKind::Primary,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();
        store
            .insert_agent_run(agent_run_input(
                "child",
                Some("root"),
                None,
                AgentRunKind::Subagent,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();

        store
            .insert_message(NewMessage::block(
                "s1",
                "user",
                "text",
                serde_json::json!({"text": "unowned"}),
            ))
            .await
            .unwrap();
        store
            .insert_run_message(
                "root",
                NewMessage::block(
                    "s1",
                    "assistant",
                    "text",
                    serde_json::json!({"text": "root owned"}),
                ),
            )
            .await
            .unwrap();
        store
            .insert_run_message(
                "child",
                NewMessage::block(
                    "s1",
                    "assistant",
                    "text",
                    serde_json::json!({"text": "child owned"}),
                ),
            )
            .await
            .unwrap();

        let primary = store.list_primary_messages("s1").await.unwrap();
        assert_eq!(
            primary
                .iter()
                .map(|message| message.payload["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["unowned", "root owned"]
        );
        assert_eq!(
            primary
                .iter()
                .map(|message| message.run_id.as_deref())
                .collect::<Vec<_>>(),
            [None, Some("root")],
            "the transcript must retain the owning primary run for every persisted row"
        );
        assert_eq!(store.list_messages("s1").await.unwrap().len(), 3);
        let child = store.list_run_messages("s1", "child").await.unwrap();
        assert_eq!(child.len(), 1);
        assert_eq!(child[0].payload["text"], "child owned");
    }

    #[tokio::test]
    async fn update_run_tool_call_does_not_cross_run_tool_call_id_collisions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .insert_primary_agent_run(agent_run_input(
                "root",
                None,
                None,
                AgentRunKind::Primary,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();
        store
            .insert_agent_run(agent_run_input(
                "child",
                Some("root"),
                None,
                AgentRunKind::Subagent,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();

        let tool_row = |output: &str| NewMessage {
            session_pk: "s1".into(),
            role: "assistant".into(),
            block_type: "tool_call".into(),
            payload: serde_json::json!({"name": "Bash", "output": output}),
            tool_call_id: Some("shared-id".into()),
            status: Some("in_progress".into()),
            tool_kind: Some("execute".into()),
            speaker: None,
        };
        let root_seq = store
            .insert_run_message("root", tool_row("root"))
            .await
            .unwrap();
        let child_seq = store
            .insert_run_message("child", tool_row("child"))
            .await
            .unwrap();

        let (seq, payload, kind) = store
            .update_run_tool_call(
                "child",
                "s1",
                "shared-id",
                Some("completed"),
                &serde_json::json!({"output": "child updated"}),
            )
            .await
            .unwrap();

        assert_eq!(seq, child_seq);
        assert_eq!(payload["output"], "child updated");
        assert_eq!(kind.as_deref(), Some("execute"));
        let root = store.list_run_messages("s1", "root").await.unwrap();
        assert_eq!(root[0].seq, root_seq);
        assert_eq!(root[0].payload["output"], "root");
        assert_eq!(root[0].status.as_deref(), Some("in_progress"));
        let child = store.list_run_messages("s1", "child").await.unwrap();
        assert_eq!(child[0].seq, child_seq);
        assert_eq!(child[0].payload["output"], "child updated");
        assert_eq!(child[0].status.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn tool_call_update_merges_output_and_returns_kind() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .insert_primary_agent_run(agent_run_input(
                "root",
                None,
                None,
                AgentRunKind::Primary,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();

        store
            .insert_run_message(
                "root",
                NewMessage {
                    session_pk: "s1".into(),
                    role: "assistant".into(),
                    block_type: "tool_call".into(),
                    payload: serde_json::json!({"name": "Bash", "input": {"command": "ls"}}),
                    tool_call_id: Some("tc-1".into()),
                    status: Some("pending".into()),
                    tool_kind: Some("execute".into()),
                    speaker: None,
                },
            )
            .await
            .unwrap();

        // The caller now sends ONLY the update patch; the store merges it.
        let (seq, merged, kind) = store
            .update_run_tool_call(
                "root",
                "s1",
                "tc-1",
                Some("completed"),
                &serde_json::json!({"output": "file.txt"}),
            )
            .await
            .unwrap();
        assert_eq!(
            seq, 1,
            "update_run_tool_call must return the row's real seq"
        );
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
            .update_run_tool_call("root", "s1", "tc-1", None, &serde_json::json!({}))
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
            .update_run_tool_call(
                "root",
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
    async fn agent_origin_settings_and_policy_writes_are_rejected() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // A USER-origin write succeeds (the normal path).
        store
            .set_setting(WriteOrigin::User, "theme", "dark")
            .await
            .expect("user setting write");
        store
            .set_tool_policy(WriteOrigin::User, "p1", "Bash", "allowAlways")
            .await
            .expect("user policy write");

        // An AGENT-origin write to the SAME protected APIs is refused AT THE
        // STORE — even though the caller invoked the method directly.
        let e = store
            .set_setting(WriteOrigin::Agent, "theme", "light")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not permitted"), "got: {e}");
        let e = store
            .set_tool_policy(WriteOrigin::Agent, "p1", "Bash", "rejectAlways")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not permitted"), "got: {e}");
        let e = store
            .delete_tool_policy(WriteOrigin::BackgroundReview, "p1", "Bash")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not permitted"), "got: {e}");

        // The rejected writes had no effect: the user's values still stand.
        assert_eq!(
            store.get_setting("theme").await.unwrap().as_deref(),
            Some("dark")
        );
        assert_eq!(
            store
                .get_tool_policy("p1", "Bash")
                .await
                .unwrap()
                .as_deref(),
            Some("allowAlways")
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
            .set_tool_policy(WriteOrigin::User, "p1", "Bash", "allowAlways")
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
            .set_tool_policy(WriteOrigin::User, "p1", "Bash", "rejectAlways")
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
            .set_tool_policy(WriteOrigin::User, "p1", "Bash", "allowAlways")
            .await
            .unwrap();
        store
            .set_tool_policy(WriteOrigin::User, "p2", "Edit", "rejectAlways")
            .await
            .unwrap();
        let rows = store.list_tool_policies().await.unwrap();
        assert_eq!(rows.len(), 2);
        store
            .delete_tool_policy(WriteOrigin::User, "p1", "Bash")
            .await
            .unwrap();
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
        store
            .set_setting(WriteOrigin::User, "default_agent", "claude")
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("default_agent").await.unwrap().as_deref(),
            Some("claude")
        );
        store
            .set_setting(WriteOrigin::User, "default_agent", "codex")
            .await
            .unwrap();
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
    async fn model_effort_preferences_use_structured_keys_and_clear_explicitly() {
        use crate::llm_router::model_effort::ModelPreferenceKey;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let key = ModelPreferenceKey {
            family: "openai".into(),
            model: "org/team/gpt-custom".into(),
        };

        assert_eq!(store.get_model_effort_preference(&key).await.unwrap(), None);
        store
            .set_model_effort_preference(&key, "ultra")
            .await
            .unwrap();
        assert_eq!(
            store
                .get_model_effort_preference(&key)
                .await
                .unwrap()
                .as_deref(),
            Some("ultra")
        );
        assert_eq!(
            store.list_model_effort_preferences().await.unwrap(),
            vec![(key.clone(), "ultra".into())]
        );
        store.clear_model_effort_preference(&key).await.unwrap();
        assert_eq!(store.get_model_effort_preference(&key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn model_effort_update_project_runtime_assigns_nulls() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .insert_project(Project {
                project_id: "runtime-prefs".into(),
                name: "runtime".into(),
                workdir: "/tmp/runtime".into(),
                source: None,
                model: Some("old-model".into()),
                effort: Some("high".into()),
                perm_mode: PermMode::Default,
                created_at: Some(1),
                is_git: false,
            })
            .await
            .unwrap();

        store
            .update_project_runtime("runtime-prefs", Some("new/model".into()), None)
            .await
            .unwrap();
        let project = store.get_project("runtime-prefs").await.unwrap().unwrap();
        assert_eq!(project.model.as_deref(), Some("new/model"));
        assert_eq!(project.effort, None);

        store
            .update_project_runtime("runtime-prefs", None, Some("none".into()))
            .await
            .unwrap();
        let project = store.get_project("runtime-prefs").await.unwrap().unwrap();
        assert_eq!(project.model, None);
        assert_eq!(project.effort.as_deref(), Some("none"));
    }

    #[tokio::test]
    async fn model_effort_migration_normalizes_eligible_legacy_storage_and_replays() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(|c| {
                    c.execute_batch(
                        r#"DROP TABLE model_effort_preferences;
                           INSERT INTO projects(project_id,name,workdir,model,effort)
                             VALUES ('p1','p1','/p1','openai/gpt-5.2-codex-high',NULL),
                                    ('p2','p2','/p2','openai/gpt-5.2-codex-review-high','ultra'),
                                    ('oauthprefix','oauthprefix','/oauth','openai-oauth/gpt-5.2-codex-high',NULL),
                                    ('alias','alias','/alias','fast-high',NULL),
                                    ('unknown','unknown','/unknown','openai/not-cataloged-high',NULL);
                           INSERT OR REPLACE INTO settings(key,value) VALUES
                             ('default_model','openai/gpt-5.2-codex-high-review'),
                             ('default_effort',''),
                             ('llm_model_routes','[{"id":"r1","name":"route","enabled":true,"strategy":"fallback","targets":[{"provider":"openai","model":"gpt-5.2-codex-review-high"},{"provider":"anthropic","model":"claude-high"}],"createdAt":1,"updatedAt":1}]');
                           PRAGMA user_version=23;"#,
                    )
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let p1 = store.get_project("p1").await.unwrap().unwrap();
        assert_eq!(p1.model.as_deref(), Some("openai/gpt-5.2-codex"));
        assert_eq!(p1.effort.as_deref(), Some("high"));
        let p2 = store.get_project("p2").await.unwrap().unwrap();
        assert_eq!(p2.model.as_deref(), Some("openai/gpt-5.2-codex-review"));
        assert_eq!(p2.effort.as_deref(), Some("ultra"), "existing effort wins");
        assert_eq!(
            store
                .get_project("alias")
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("fast-high")
        );
        assert_eq!(
            store
                .get_project("unknown")
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("openai/not-cataloged-high")
        );
        assert_eq!(
            store
                .get_project("oauthprefix")
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("openai/gpt-5.2-codex")
        );
        assert_eq!(
            store.get_setting("default_model").await.unwrap().as_deref(),
            Some("openai/gpt-5.2-codex-review")
        );
        let key = crate::llm_router::model_effort::ModelPreferenceKey {
            family: "openai".into(),
            model: "gpt-5.2-codex-review".into(),
        };
        assert_eq!(
            store
                .get_model_effort_preference(&key)
                .await
                .unwrap()
                .as_deref(),
            Some("high")
        );
        let routes = crate::llm_router::routes::list_model_routes(&store)
            .await
            .unwrap();
        assert_eq!(routes[0].targets[0].model, "gpt-5.2-codex-review");
        assert_eq!(routes[0].targets[0].effort.as_deref(), Some("high"));
        assert_eq!(routes[0].targets[1].model, "claude-high");

        let before = store.list_model_effort_preferences().await.unwrap();
        store
            .with_conn(|c| c.pragma_update(None, "user_version", 23))
            .await
            .unwrap();
        drop(store);
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(store.list_model_effort_preferences().await.unwrap(), before);
    }

    #[tokio::test]
    async fn model_effort_migration_preserves_exact_known_suffix_bearing_models() {
        let routes = r#"[{"id":"exact-route","name":"exact","enabled":true,"strategy":"fallback","targets":[{"provider":"openai","model":"foo-high"}],"createdAt":1,"updatedAt":1}]"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            let routes = routes.to_string();
            store
                .with_conn(move |c| {
                    c.execute_batch(
                        r#"DROP TABLE model_effort_preferences;
                           INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at)
                             VALUES ('exact-codex','openai-oauth','oauth','exact',0,1,'{"modelsOverride":["foo","foo-high"]}',1,1);
                           INSERT INTO projects(project_id,name,workdir,model,effort)
                             VALUES ('exact-project','exact','/exact','openai/foo-high',NULL);
                           INSERT OR REPLACE INTO settings(key,value) VALUES
                             ('default_model','openai/foo-high'),
                             ('default_effort','');
                           PRAGMA user_version=23;"#,
                    )?;
                    c.execute(
                        "INSERT OR REPLACE INTO settings(key,value) VALUES ('llm_model_routes',?1)",
                        params![routes],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let project = store.get_project("exact-project").await.unwrap().unwrap();
        assert_eq!(project.model.as_deref(), Some("openai/foo-high"));
        assert_eq!(project.effort, None);
        assert_eq!(
            store.get_setting("default_model").await.unwrap().as_deref(),
            Some("openai/foo-high")
        );
        assert_eq!(
            store
                .get_setting("llm_model_routes")
                .await
                .unwrap()
                .as_deref(),
            Some(routes)
        );
        assert!(store
            .list_model_effort_preferences()
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn model_effort_migration_preserves_known_nested_model_ids() {
        let routes = r#"[{"id":"nested-route","name":"nested","enabled":true,"strategy":"fallback","targets":[{"provider":"openai","model":"org/model-high"}],"createdAt":1,"updatedAt":1}]"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            let routes = routes.to_string();
            store
                .with_conn(move |c| {
                    c.execute_batch(
                        r#"DROP TABLE model_effort_preferences;
                           INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at)
                             VALUES ('nested-codex','openai-oauth','oauth','nested',0,1,'{"modelsOverride":["org/model","org/model-high"]}',1,1);
                           INSERT INTO projects(project_id,name,workdir,model,effort)
                             VALUES ('nested-project','nested','/nested','openai/org/model-high',NULL);
                           INSERT OR REPLACE INTO settings(key,value) VALUES
                             ('default_model','openai/org/model-high'),
                             ('default_effort','');
                           PRAGMA user_version=23;"#,
                    )?;
                    c.execute(
                        "INSERT OR REPLACE INTO settings(key,value) VALUES ('llm_model_routes',?1)",
                        params![routes],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let project = store.get_project("nested-project").await.unwrap().unwrap();
        assert_eq!(project.model.as_deref(), Some("openai/org/model-high"));
        assert_eq!(project.effort, None);
        assert_eq!(
            store.get_setting("default_model").await.unwrap().as_deref(),
            Some("openai/org/model-high")
        );
        assert_eq!(
            store
                .get_setting("llm_model_routes")
                .await
                .unwrap()
                .as_deref(),
            Some(routes)
        );
        assert!(store
            .list_model_effort_preferences()
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn model_effort_migration_malformed_routes_rolls_back_atomically() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store.with_conn(|c| c.execute_batch(
                "DROP TABLE model_effort_preferences;
                 INSERT INTO projects(project_id,name,workdir,model) VALUES ('p','p','/p','openai/gpt-5.2-codex-high');
                 INSERT OR REPLACE INTO settings(key,value) VALUES ('llm_model_routes','{malformed');
                 PRAGMA user_version=23;"
            )).await.unwrap();
        }

        assert!(Store::open(tmp.path()).await.is_err());
        let c = rusqlite::Connection::open(tmp.path()).unwrap();
        let version: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 23);
        let model: String = c
            .query_row("SELECT model FROM projects WHERE project_id='p'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(model, "openai/gpt-5.2-codex-high");
        assert!(c.prepare("SELECT 1 FROM model_effort_preferences").is_err());
    }

    #[tokio::test]
    async fn model_effort_migration_non_empty_default_effort_wins_without_seeding() {
        use crate::llm_router::model_effort::ModelPreferenceKey;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(|c| {
                    c.execute_batch(
                        "DROP TABLE model_effort_preferences;
                 INSERT OR REPLACE INTO settings(key,value) VALUES
                   ('default_model','openai/gpt-5.5-review-high'),
                   ('default_effort','ultra');
                 PRAGMA user_version=23;",
                    )
                })
                .await
                .unwrap();
        }
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(
            store.get_setting("default_model").await.unwrap().as_deref(),
            Some("openai/gpt-5.5-review")
        );
        assert_eq!(
            store
                .get_setting("default_effort")
                .await
                .unwrap()
                .as_deref(),
            Some("ultra")
        );
        assert_eq!(
            store
                .get_model_effort_preference(&ModelPreferenceKey {
                    family: "openai".into(),
                    model: "gpt-5.5-review".into(),
                })
                .await
                .unwrap(),
            None
        );
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
        assert_eq!(user_version, 42, "forward migration must land at v42");
    }

    #[tokio::test]
    async fn migration_30_adds_audit_session_and_origin_columns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let cols: Vec<String> = store
            .with_conn(|c| {
                let mut stmt = c.prepare("PRAGMA table_info(audit)")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        assert!(cols.iter().any(|c| c == "session_pk"), "cols: {cols:?}");
        assert!(cols.iter().any(|c| c == "origin"), "cols: {cols:?}");
    }

    #[tokio::test]
    async fn insert_primary_agent_run_rejects_non_primary_and_non_root_shapes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        let valid_root = NewAgentRun {
            run_id: "root".into(),
            session_pk: "s1".into(),
            parent_run_id: None,
            retry_of: None,
            source_tool_call_id: None,
            dispatch_index: None,
            primary_agent_id: "ada".into(),
            executing_agent_id: Some("ada".into()),
            executing_agent_name_snapshot: "Ada".into(),
            agent_kind: AgentRunKind::Primary,
            task: "root".into(),
            status: AgentRunStatus::Queued,
            resolved_model: None,
            resolved_effort: None,
        };
        for (run_id, agent_kind, parent_run_id, retry_of) in [
            ("delegate", AgentRunKind::MainDelegate, None, None),
            ("parented", AgentRunKind::Primary, Some("root".into()), None),
            ("retry", AgentRunKind::Primary, None, Some("root".into())),
        ] {
            let mut invalid = valid_root.clone();
            invalid.run_id = run_id.into();
            invalid.agent_kind = agent_kind;
            invalid.parent_run_id = parent_run_id;
            invalid.retry_of = retry_of;
            assert!(
                store.insert_primary_agent_run(invalid).await.is_err(),
                "{run_id} must not be accepted as a primary root"
            );
        }
        assert!(store.insert_primary_agent_run(valid_root).await.is_ok());
    }

    #[tokio::test]
    async fn agentic_session_migration_preserves_legacy_history_and_run_tree() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store.insert_project(sample_project()).await.unwrap();
            store.insert_session(sample_session()).await.unwrap();
            store
                .with_conn(|c| c.pragma_update(None, "user_version", 34))
                .await
                .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let legacy = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(legacy.primary_agent_id, None);
        assert_eq!(legacy.primary_agent_snapshot, None);

        let identity = AgentIdentitySnapshot {
            id: "ada".into(),
            name: "Ada at creation".into(),
            avatar_color: "violet".into(),
        };
        let mut owned = sample_session();
        owned.session_pk = "owned".into();
        let root = NewAgentRun {
            run_id: "root".into(),
            session_pk: "owned".into(),
            parent_run_id: None,
            retry_of: None,
            source_tool_call_id: None,
            dispatch_index: None,
            primary_agent_id: "ada".into(),
            executing_agent_id: Some("ada".into()),
            executing_agent_name_snapshot: "Ada".into(),
            agent_kind: AgentRunKind::Primary,
            task: "ship it".into(),
            status: AgentRunStatus::Queued,
            resolved_model: Some("anthropic/claude-opus-4-8".into()),
            resolved_effort: Some("high".into()),
        };
        store
            .insert_owned_session_with_primary_run(owned, identity.clone(), root)
            .await
            .unwrap();
        drop(store);

        let store = Store::open(tmp.path()).await.unwrap();
        let owned = store.get_session("owned").await.unwrap().unwrap();
        assert_eq!(owned.primary_agent_id.as_deref(), Some("ada"));
        assert_eq!(owned.primary_agent_snapshot, Some(identity));
        assert_eq!(
            store
                .get_agent_run("root")
                .await
                .unwrap()
                .unwrap()
                .executing_agent_name_snapshot,
            "Ada"
        );
    }

    #[tokio::test]
    async fn malformed_primary_agent_snapshot_errors_on_read() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE sessions SET primary_agent_id='ada', primary_agent_snapshot='{not json' WHERE session_pk='s1'",
                    [],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        assert!(store.get_session("s1").await.is_err());
    }

    #[tokio::test]
    async fn agentic_session_migration_upgrades_a_fresh_v34_database() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE sessions (
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
                 CREATE TABLE messages (
                    session_pk TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    block_type TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    tool_call_id TEXT,
                    status TEXT,
                    tool_kind TEXT,
                    created_at INTEGER NOT NULL,
                    speaker TEXT,
                    PRIMARY KEY (session_pk, seq)
                 );
                 INSERT INTO sessions (session_pk) VALUES ('legacy');
                 PRAGMA user_version = 34;",
            )
            .unwrap();
        }

        let store = Store::open(tmp.path()).await.unwrap();
        let legacy = store.get_session("legacy").await.unwrap().unwrap();
        assert_eq!(legacy.primary_agent_id, None);
        assert_eq!(legacy.primary_agent_snapshot, None);
        let has_agent_runs: bool = store
            .with_conn(|c| {
                c.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_runs'")?
                    .exists([])
            })
            .await
            .unwrap();
        assert!(has_agent_runs);
    }

    #[tokio::test]
    async fn agent_runs_validate_tree_transition_and_message_ownership() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_project(sample_project()).await.unwrap();
        let identity = AgentIdentitySnapshot {
            id: "ada".into(),
            name: "Ada".into(),
            avatar_color: "violet".into(),
        };
        let root = NewAgentRun {
            run_id: "root".into(),
            session_pk: "owned".into(),
            parent_run_id: None,
            retry_of: None,
            source_tool_call_id: None,
            dispatch_index: None,
            primary_agent_id: "ada".into(),
            executing_agent_id: Some("ada".into()),
            executing_agent_name_snapshot: "Ada".into(),
            agent_kind: AgentRunKind::Primary,
            task: "ship it".into(),
            status: AgentRunStatus::Queued,
            resolved_model: None,
            resolved_effort: None,
        };
        let mut session = sample_session();
        session.session_pk = "owned".into();
        store
            .insert_owned_session_with_primary_run(session, identity, root)
            .await
            .unwrap();

        let child = NewAgentRun {
            run_id: "child".into(),
            session_pk: "owned".into(),
            parent_run_id: Some("root".into()),
            retry_of: None,
            source_tool_call_id: None,
            dispatch_index: None,
            primary_agent_id: "ada".into(),
            executing_agent_id: Some("delegate".into()),
            executing_agent_name_snapshot: "Delegate".into(),
            agent_kind: AgentRunKind::MainDelegate,
            task: "delegate it".into(),
            status: AgentRunStatus::Queued,
            resolved_model: None,
            resolved_effort: None,
        };
        store.insert_agent_run(child).await.unwrap();
        assert_eq!(
            store
                .list_descendant_agent_runs("root")
                .await
                .unwrap()
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["child"]
        );
        assert!(store
            .transition_agent_run(
                "child",
                &[AgentRunStatus::Queued],
                AgentRunStatus::Running,
                None,
                None,
            )
            .await
            .unwrap());
        assert!(store
            .transition_agent_run(
                "child",
                &[AgentRunStatus::Running],
                AgentRunStatus::Completed,
                Some("done"),
                None,
            )
            .await
            .unwrap());
        assert!(!store
            .transition_agent_run(
                "child",
                &[AgentRunStatus::Completed],
                AgentRunStatus::Running,
                None,
                None,
            )
            .await
            .unwrap());
        let child = store.get_agent_run("child").await.unwrap().unwrap();
        assert!(child.started_at.is_some());
        assert!(child.finished_at.is_some());
        assert_eq!(child.result.as_deref(), Some("done"));

        let seq = store
            .insert_run_message(
                "child",
                NewMessage::block(
                    "owned",
                    "assistant",
                    "text",
                    serde_json::json!({"text": "done"}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(seq, 1);
        assert_eq!(
            store.list_run_messages("owned", "child").await.unwrap()[0].payload["text"],
            "done"
        );
        assert!(store
            .insert_run_message(
                "child",
                NewMessage::block(
                    "wrong",
                    "assistant",
                    "text",
                    serde_json::json!({"text": "wrong"})
                ),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn agent_run_dispatch_linkage_roundtrips_and_requires_paired_fields() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .insert_primary_agent_run(agent_run_input(
                "root",
                None,
                None,
                AgentRunKind::Primary,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();

        let mut linked_input = agent_run_input(
            "linked",
            Some("root"),
            None,
            AgentRunKind::MainDelegate,
            AgentRunStatus::Queued,
        );
        linked_input.source_tool_call_id = Some("tool-dispatch-1".into());
        linked_input.dispatch_index = Some(2);
        let linked = store.insert_agent_run(linked_input.clone()).await.unwrap();
        assert_eq!(
            linked.source_tool_call_id.as_deref(),
            Some("tool-dispatch-1")
        );
        assert_eq!(linked.dispatch_index, Some(2));

        let mut source_only = linked_input.clone();
        source_only.run_id = "source-only".into();
        source_only.dispatch_index = None;
        assert!(store.insert_agent_run(source_only).await.is_err());

        let mut index_only = linked_input.clone();
        index_only.run_id = "index-only".into();
        index_only.source_tool_call_id = None;
        assert!(store.insert_agent_run(index_only).await.is_err());

        let mut negative_index = linked_input.clone();
        negative_index.run_id = "negative-index".into();
        negative_index.dispatch_index = Some(-1);
        assert!(store.insert_agent_run(negative_index).await.is_err());

        let mut blank_source = linked_input.clone();
        blank_source.run_id = "blank-source".into();
        blank_source.source_tool_call_id = Some("   ".into());
        assert!(store.insert_agent_run(blank_source).await.is_err());

        let mut linked_root = agent_run_input(
            "linked-root",
            None,
            None,
            AgentRunKind::Primary,
            AgentRunStatus::Queued,
        );
        linked_root.source_tool_call_id = Some("tool-dispatch-1".into());
        linked_root.dispatch_index = Some(2);
        assert!(store.insert_primary_agent_run(linked_root).await.is_err());
    }

    #[tokio::test]
    async fn retry_requires_inherited_dispatch_linkage_and_rejects_branches() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.insert_session(sample_session()).await.unwrap();
        store
            .insert_primary_agent_run(agent_run_input(
                "root",
                None,
                None,
                AgentRunKind::Primary,
                AgentRunStatus::Queued,
            ))
            .await
            .unwrap();

        let mut failed_child = agent_run_input(
            "failed-child",
            Some("root"),
            None,
            AgentRunKind::MainDelegate,
            AgentRunStatus::Failed,
        );
        failed_child.source_tool_call_id = Some("tool-dispatch-1".into());
        failed_child.dispatch_index = Some(2);
        store.insert_agent_run(failed_child.clone()).await.unwrap();

        let mut retry = failed_child.clone();
        retry.run_id = "retry".into();
        retry.retry_of = Some("failed-child".into());
        store.insert_agent_run(retry.clone()).await.unwrap();

        let mut second_branch = retry.clone();
        second_branch.run_id = "second-branch".into();
        second_branch.retry_of = Some("failed-child".into());
        assert!(store.insert_agent_run(second_branch).await.is_err());

        let mut changed_linkage = retry;
        changed_linkage.run_id = "changed-linkage".into();
        changed_linkage.retry_of = Some("retry".into());
        changed_linkage.source_tool_call_id = Some("tool-dispatch-2".into());
        assert!(store.insert_agent_run(changed_linkage).await.is_err());
        assert_eq!(
            store.list_session_agent_runs("s1").await.unwrap().len(),
            3,
            "rejected retry branches must not insert rows"
        );
    }

    #[tokio::test]
    async fn migration_34_creates_agent_learning_queue_and_state() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let tables: Vec<String> = store
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' \
                     AND name LIKE 'agent_learning_%' ORDER BY name",
                )?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        assert_eq!(tables, vec!["agent_learning_queue", "agent_learning_state"]);
        let has_index: bool = store
            .with_conn(|c| {
                c.prepare(
                    "SELECT 1 FROM sqlite_master WHERE type='index' \
                     AND name='idx_agent_learning_delivery'",
                )?
                .exists([])
            })
            .await
            .unwrap();
        assert!(has_index, "ordered delivery index must exist");
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
    async fn migrations_13_to_41_replay_is_idempotent_and_converges_native_only() {
        // An existing DB carries pre-Ryuzi-only rows. Build a current-schema
        // DB, seed the old values, then rewind far enough that migration 13
        // and every later migration run again.
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
        // perm_mode; 23 plugin_installs + plugin_attach_status; 24 typed model
        // effort preferences + legacy normalization; 25 session_route_state;
        // 26 session_runtime_settings; 27 background_events + jobs.model_override;
        // 28 messages_fts + sync triggers, skill_usage, curator_state, curator_runs;
        // 29 messages.speaker + orch_tasks home/breaker/steer columns;
        // 30 audit.session_pk + audit.origin;
        // 31 plugin_catalog_cache + catalog_feed_state;
        // 34 agent_learning_state + agent_learning_queue — CREATE TABLE
        // IF NOT EXISTS; 35 session_prompt_queue; 36 automation hooks/runs/
        // attempts; 37 session_automation_origins plus compatibility queue
        // repair; 38 session ownership and agent runs; 39 agentic cleanup;
        // 40 durable dispatch linkage; 41 removes session-wide tool-call
        // uniqueness; 42 artifacts/artifact_references/artifact_storage_jobs —
        // all convergent, existence-guarded, or CREATE TABLE IF NOT EXISTS)
        // re-run on next open.
        // `Migrations` always fast-forwards to the latest defined version, so
        // there is no way to replay 13 alone once something is appended after
        // it. Bump this offset by one for every migration appended after 13 —
        // a stale offset silently skips migration 13 (the DB opens fine, but
        // this test starts failing its assertions). With migrations through 42
        // defined, wind back thirty.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let rewind = |c: &mut rusqlite::Connection| -> rusqlite::Result<()> {
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            c.pragma_update(None, "user_version", v - 30)
        };
        {
            let store = Store::open(tmp.path()).await.unwrap();
            store
                .with_conn(move |c| {
                    // The DB is fully migrated to v30 here, so `harness` was
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
        // wind user_version back, and reopen so 21 (and the tail
        // migrations 22–42) replay against it. Back TWENTY-TWO: the fully
        // migrated tail is now v42, so rewinding to v20 is what makes
        // migration 21 (native-only) replay.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Replaying from v20 also crosses migration 29's `ALTER TABLE
        // orch_tasks ADD COLUMN ...` hook, but migration 39 already dropped
        // orch_tasks in this fully-migrated store. The hook must therefore
        // no-op instead of altering a legacy table that no longer exists.
        let rewind = |c: &mut rusqlite::Connection| -> rusqlite::Result<()> {
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            c.pragma_update(None, "user_version", v - 22)
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
        // Migration 21 copies native prefs, then migration 39 removes those
        // superseded single-agent settings in the same replay pass.
        assert!(store.get_setting("agent_model").await.unwrap().is_none());
        assert!(store
            .get_setting("agent_perm_mode")
            .await
            .unwrap()
            .is_none());
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

        // The cleanup is unconditional, so replaying the tail removes even a
        // user-set legacy value.
        store
            .set_setting(WriteOrigin::User, "agent_model", "user-chose-this")
            .await
            .unwrap();
        store.with_conn(rewind).await.unwrap();
        drop(store);
        let store = Store::open(tmp.path()).await.unwrap();
        assert!(
            store.get_setting("agent_model").await.unwrap().is_none(),
            "migration 39 must remove agent_model on replay"
        );
        let route_state_exists: bool = store
            .with_conn(|c| {
                c.prepare(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_route_state'",
                )?
                .exists([])
            })
            .await
            .unwrap();
        assert!(route_state_exists);
    }

    #[tokio::test]
    async fn migration_27_adds_background_events_and_job_model_override() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let (uv, has_bg, has_override) = store
            .with_conn(|c| {
                let uv: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let has_bg = c
                    .prepare(
                        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='background_events'",
                    )?
                    .exists([])?;
                let has_override = c
                    .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name='model_override'")?
                    .exists([])?;
                Ok((uv, has_bg, has_override))
            })
            .await
            .unwrap();
        assert_eq!(uv, 42, "forward migration must land at v42");
        assert!(has_bg, "background_events table must exist");
        assert!(has_override, "jobs.model_override column must exist");
    }

    #[tokio::test]
    async fn migration_28_adds_fts_but_migration_39_drops_legacy_agentic_tables() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let (uv, has_fts, has_usage, has_cstate, has_cruns) = store
            .with_conn(|c| {
                let uv: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let t = |name: &str| -> rusqlite::Result<bool> {
                    c.prepare("SELECT 1 FROM sqlite_master WHERE name=?1")?
                        .exists([name])
                };
                Ok((
                    uv,
                    t("messages_fts")?,
                    t("skill_usage")?,
                    t("curator_state")?,
                    t("curator_runs")?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(uv, 42, "forward migration must land at v42");
        assert!(has_fts, "messages_fts must survive agentic cleanup");
        assert!(
            !has_usage && !has_cstate && !has_cruns,
            "legacy agentic tables must be removed"
        );
    }

    #[tokio::test]
    async fn messages_fts_trigger_indexes_text_rows_and_matches() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // A session row is not required for the trigger; insert a user text message.
        store
            .insert_message(crate::domain::NewMessage::block(
                "s1",
                "user",
                "text",
                serde_json::json!({ "text": "deploy the widget service to staging" }),
            ))
            .await
            .unwrap();
        // A non-text tool row must NOT be indexed.
        store
            .insert_message(crate::domain::NewMessage {
                session_pk: "s1".into(),
                role: "assistant".into(),
                block_type: "tool_call".into(),
                payload: serde_json::json!({ "name": "bash" }),
                tool_call_id: Some("t1".into()),
                status: None,
                tool_kind: Some("execute".into()),
                speaker: None,
            })
            .await
            .unwrap();
        let hits: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'widget'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(hits, 1, "only the user text row is indexed and matches");
    }

    /// Builds a minimal `Session` row for `search_messages_fts`/lineage tests.
    fn mk_session(pk: &str, kind: crate::domain::SessionKind, now: i64) -> crate::domain::Session {
        mk_session_with_parent(pk, kind, now, None)
    }

    fn mk_session_with_parent(
        pk: &str,
        kind: crate::domain::SessionKind,
        now: i64,
        parent: Option<&str>,
    ) -> crate::domain::Session {
        crate::domain::Session {
            session_pk: pk.into(),
            primary_agent_id: None,
            primary_agent_snapshot: None,
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: Some(format!("t-{pk}")),
            status: crate::domain::SessionStatus::Idle,
            perm_mode: crate::domain::PermMode::Default,
            started_by: None,
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: false,
            kind,
            speaker: None,
            agent: None,
            parent_session_pk: parent.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn search_messages_fts_finds_and_excludes_worker_sessions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(mk_session("chat-1", crate::domain::SessionKind::Chat, now))
            .await
            .unwrap();
        store
            .insert_session(mk_session("wrk-1", crate::domain::SessionKind::Worker, now))
            .await
            .unwrap();
        for pk in ["chat-1", "wrk-1"] {
            store
                .insert_message(crate::domain::NewMessage::block(
                    pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": "kubernetes ingress routing" }),
                ))
                .await
                .unwrap();
        }
        let hits = store.search_messages_fts("ingress", &[], 20).await.unwrap();
        assert!(hits.iter().any(|h| h.session_pk == "chat-1"));
        assert!(
            !hits.iter().any(|h| h.session_pk == "wrk-1"),
            "worker sessions excluded"
        );
    }

    #[tokio::test]
    async fn search_messages_fts_non_matching_query_returns_empty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(mk_session("chat-1", crate::domain::SessionKind::Chat, now))
            .await
            .unwrap();
        store
            .insert_message(crate::domain::NewMessage::block(
                "chat-1",
                "user",
                "text",
                serde_json::json!({ "text": "kubernetes ingress routing" }),
            ))
            .await
            .unwrap();
        let hits = store
            .search_messages_fts("nonexistentzzz", &[], 20)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_messages_fts_malformed_query_errors_cleanly_not_panics() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // An unterminated quote is invalid FTS5 query syntax; SQLite surfaces
        // it as a runtime error on the bound MATCH parameter, not a panic.
        let result = store.search_messages_fts("\"unterminated", &[], 20).await;
        assert!(
            result.is_err(),
            "malformed FTS5 query must error, not panic"
        );
    }

    #[tokio::test]
    async fn search_messages_fts_dedups_by_lineage_root_and_excludes_caller_lineage() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        // Two chat sessions in the SAME lineage (root "chat-root"): only the
        // best (most recent) hit should survive the dedup.
        store
            .insert_session(mk_session(
                "chat-root",
                crate::domain::SessionKind::Chat,
                now,
            ))
            .await
            .unwrap();
        store
            .insert_session(mk_session_with_parent(
                "chat-child",
                crate::domain::SessionKind::Chat,
                now + 1,
                Some("chat-root"),
            ))
            .await
            .unwrap();
        // A separate, unrelated lineage that the caller wants excluded
        // (its own current conversation).
        store
            .insert_session(mk_session(
                "own-lineage",
                crate::domain::SessionKind::Chat,
                now,
            ))
            .await
            .unwrap();
        for pk in ["chat-root", "chat-child", "own-lineage"] {
            store
                .insert_message(crate::domain::NewMessage::block(
                    pk,
                    "user",
                    "text",
                    serde_json::json!({ "text": "kubernetes ingress routing" }),
                ))
                .await
                .unwrap();
        }
        let hits = store
            .search_messages_fts("ingress", &["own-lineage".to_string()], 20)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|h| h.session_pk == "own-lineage"),
            "caller's own lineage excluded"
        );
        let lineage_hits: Vec<_> = hits
            .iter()
            .filter(|h| h.session_pk == "chat-root" || h.session_pk == "chat-child")
            .collect();
        assert_eq!(
            lineage_hits.len(),
            1,
            "same-lineage hits dedup to a single entry, got {lineage_hits:?}"
        );
    }

    #[tokio::test]
    async fn lineage_of_walks_to_the_root_and_is_empty_for_unknown_session() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(mk_session(
                "chat-root",
                crate::domain::SessionKind::Chat,
                now,
            ))
            .await
            .unwrap();
        store
            .insert_session(mk_session_with_parent(
                "chat-child",
                crate::domain::SessionKind::Chat,
                now + 1,
                Some("chat-root"),
            ))
            .await
            .unwrap();
        assert_eq!(
            store.lineage_of("chat-child").await.unwrap(),
            vec!["chat-root".to_string()]
        );
        assert_eq!(
            store.lineage_of("chat-root").await.unwrap(),
            vec!["chat-root".to_string()]
        );
        assert!(store
            .lineage_of("no-such-session")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn messages_window_returns_the_radius_slice() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        for i in 0..12 {
            store
                .insert_message(crate::domain::NewMessage::block(
                    "s1",
                    "user",
                    "text",
                    serde_json::json!({ "text": format!("msg {i}") }),
                ))
                .await
                .unwrap();
        }
        // seqs run 1..=12; a ±5 window around seq 6 is [1,11].
        let window = store.messages_window("s1", 6, 5).await.unwrap();
        let seqs: Vec<i64> = window.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, (1..=11).collect::<Vec<_>>());
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
    async fn background_rail_enqueue_claim_deliver_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        // An IDLE target and a RUNNING target.
        let mk = |pk: &str, status: crate::domain::SessionStatus| crate::domain::Session {
            session_pk: pk.into(),
            primary_agent_id: None,
            primary_agent_snapshot: None,
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status,
            perm_mode: crate::domain::PermMode::Default,
            started_by: None,
            created_at: Some(now),
            last_active: Some(now),
            resume_attempts: 0,
            branch_owned: false,
            kind: crate::domain::SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        };
        store
            .insert_session(mk("idle-1", crate::domain::SessionStatus::Idle))
            .await
            .unwrap();
        store
            .insert_session(mk("busy-1", crate::domain::SessionStatus::Running))
            .await
            .unwrap();

        store
            .enqueue_background_event("busy-1", "delegation", "{\"x\":1}")
            .await
            .unwrap();
        let id_idle = store
            .enqueue_background_event("idle-1", "delegation", "{\"x\":2}")
            .await
            .unwrap();

        // Only the idle-target row is claimable.
        let claimed = store
            .claim_deliverable_background_event("drainer")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, id_idle);
        assert_eq!(claimed.target_session_pk, "idle-1");
        // A second claim finds nothing (the busy target is skipped, the idle row is now claimed).
        assert!(store
            .claim_deliverable_background_event("drainer")
            .await
            .unwrap()
            .is_none());

        store.mark_background_delivered(&claimed.id).await.unwrap();
        assert_eq!(store.pending_background_count().await.unwrap(), 1); // busy row still pending

        // Session-end cascade removes the busy target's row.
        assert_eq!(
            store
                .delete_background_events_for_session("busy-1")
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.pending_background_count().await.unwrap(), 0);
    }

    /// Pins the Task 8 rail split (spec §3.1/§7.2): the generic drainer's
    /// claim must NEVER return a `kind='learning'` row (it would otherwise
    /// inject a learning payload as a chat user turn), and
    /// `claim_learning_event` must be the only way to claim one. A
    /// `delegation` row (Phase 3 behavior) must still be claimable by the
    /// generic drainer — proving the exclusion is scoped to `learning` only,
    /// not a regression of ordinary background delivery.
    #[tokio::test]
    async fn claim_deliverable_background_event_skips_learning_rows_claim_learning_event_takes_them(
    ) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = crate::paths::now_ms();
        store
            .insert_session(crate::domain::Session {
                session_pk: "idle-1".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: crate::domain::SessionStatus::Idle,
                perm_mode: crate::domain::PermMode::Default,
                started_by: None,
                created_at: Some(now),
                last_active: Some(now),
                resume_attempts: 0,
                branch_owned: false,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();

        let learning_id = store
            .enqueue_background_event("idle-1", "learning", "{}")
            .await
            .unwrap();
        let delegation_id = store
            .enqueue_background_event("idle-1", "delegation", "{\"x\":1}")
            .await
            .unwrap();

        // The generic drainer must skip the learning row and reach the
        // delegation row instead (no Phase-3 regression).
        let claimed = store
            .claim_deliverable_background_event("drainer")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, delegation_id);
        assert_eq!(claimed.kind, "delegation");
        // With the only non-learning row now claimed, the generic drainer
        // finds nothing else — specifically NOT the still-pending learning
        // row.
        assert!(store
            .claim_deliverable_background_event("drainer")
            .await
            .unwrap()
            .is_none());

        // The dedicated learning claim DOES pick up the learning row.
        let learning_claimed = store
            .claim_learning_event("learner")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(learning_claimed.id, learning_id);
        assert_eq!(learning_claimed.kind, "learning");
    }

    #[tokio::test]
    async fn migration_23_creates_plugin_install_tables() {
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

    // ---------- curator_state / curator_runs (Task 10) ----------

    #[tokio::test]
    async fn migration_24_creates_catalog_tables_and_roundtrips() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 0);
        let rows = vec![RemoteCatalogRow {
            id: "acme".into(),
            manifest_toml: "id=\"acme\"".into(),
            version: "1.2.0".into(),
            sequence: 5,
            blocked: false,
            blocked_reason: None,
            fetched_at: 100,
        }];
        store.upsert_remote_catalog(&rows).await.unwrap();
        store.set_catalog_feed_state(5, "ok").await.unwrap();
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 5);
        let got = store.list_remote_catalog().await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].version, "1.2.0");
        // replace-all semantics: a second upsert with fewer rows clears the old set
        store.upsert_remote_catalog(&[]).await.unwrap();
        assert!(store.list_remote_catalog().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn migration_29_creates_devices_and_pairing_codes_tables() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let counts = store
            .with_conn(|c| {
                let devices: i64 = c.query_row("SELECT count(*) FROM devices", [], |r| r.get(0))?;
                let codes: i64 =
                    c.query_row("SELECT count(*) FROM pairing_codes", [], |r| r.get(0))?;
                Ok((devices, codes))
            })
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[tokio::test]
    async fn device_insert_find_and_revoke_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        store
            .insert_device("dev-1", "alfin-laptop", "hash-abc")
            .await
            .unwrap();

        let found = store
            .find_device_by_token_hash("hash-abc")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "dev-1");
        assert_eq!(found.name, "alfin-laptop");
        assert!(!found.revoked);
        assert!(found.last_seen.is_none());

        assert!(store
            .find_device_by_token_hash("no-such-hash")
            .await
            .unwrap()
            .is_none());

        store.touch_device_last_seen("dev-1", 12_345).await.unwrap();
        let touched = store
            .find_device_by_token_hash("hash-abc")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(touched.last_seen, Some(12_345));

        assert_eq!(store.list_devices().await.unwrap().len(), 1);

        let revoked = store.revoke_device("dev-1").await.unwrap();
        assert!(revoked);
        // Revoked devices no longer resolve by token hash...
        assert!(store
            .find_device_by_token_hash("hash-abc")
            .await
            .unwrap()
            .is_none());
        // ...but still show up in the full listing, marked revoked.
        let all = store.list_devices().await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].revoked);

        // Revoking an unknown device id is a no-op, not an error.
        assert!(!store.revoke_device("no-such-device").await.unwrap());
    }

    #[tokio::test]
    async fn queued_session_prompt_preserves_all_persisted_fields_and_turn_text() {
        let prompt = QueuedSessionPrompt {
            id: "queued-1".into(),
            session_pk: "session-1".into(),
            agent: "agent-visible prompt".into(),
            display: "displayed prompt".into(),
            attachments: vec![AttachmentRef {
                name: "note.txt".into(),
                url: "file:///queue/note.txt".into(),
                content_type: Some("text/plain".into()),
                size: 7,
            }],
            created_at: 42,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store.enqueue_session_prompt(prompt.clone()).await.unwrap();

        let persisted = store
            .list_session_prompt_queue("session-1")
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted, prompt);
        let turn = persisted.into_turn_prompt();
        assert_eq!(turn.agent, "agent-visible prompt");
        assert_eq!(turn.display, "displayed prompt");
        assert!(turn.blocks.is_empty());
        assert!(turn.attachments.is_empty());
    }

    #[tokio::test]
    async fn session_prompt_queue_recovers_abandoned_claims_in_fifo_order() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        for (id, created_at) in [("first", 1), ("second", 2)] {
            store
                .enqueue_session_prompt(QueuedSessionPrompt {
                    id: id.into(),
                    session_pk: "s1".into(),
                    agent: format!("agent {id}"),
                    display: id.into(),
                    attachments: vec![],
                    created_at,
                })
                .await
                .unwrap();
        }

        assert_eq!(
            store
                .claim_next_session_prompt("s1")
                .await
                .unwrap()
                .unwrap()
                .id,
            "first"
        );
        assert_eq!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["second"]
        );

        assert_eq!(
            store
                .recover_abandoned_session_prompt_claims()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            store
                .recover_abandoned_session_prompt_claims()
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn session_prompt_queue_persists_fifo_and_claim_lifecycle() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let first = QueuedSessionPrompt {
            id: "first".into(),
            session_pk: "s1".into(),
            agent: "agent first".into(),
            display: "first".into(),
            attachments: vec![],
            created_at: 1,
        };
        let second = QueuedSessionPrompt {
            id: "second".into(),
            session_pk: "s1".into(),
            agent: "agent second".into(),
            display: "second".into(),
            attachments: vec![],
            created_at: 2,
        };

        {
            let store = Store::open(tmp.path()).await.unwrap();
            store.enqueue_session_prompt(first.clone()).await.unwrap();
            store.enqueue_session_prompt(second.clone()).await.unwrap();
            assert_eq!(
                store
                    .list_session_prompt_queue("s1")
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|prompt| prompt.id)
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
        }

        let store = Store::open(tmp.path()).await.unwrap();
        assert!(!store
            .remove_session_prompt("other", "second")
            .await
            .unwrap());
        assert!(store
            .claim_next_session_prompt("s1")
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["second"]
        );
        assert!(store.restore_claimed_session_prompt("first").await.unwrap());
        assert_eq!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            store
                .claim_next_session_prompt("s1")
                .await
                .unwrap()
                .unwrap()
                .id,
            "first"
        );
        assert!(store
            .complete_claimed_session_prompt("first")
            .await
            .unwrap());
        assert!(store.remove_session_prompt("s1", "second").await.unwrap());
        assert!(store
            .list_session_prompt_queue("s1")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn session_prompt_queue_lists_pending_session_keys_in_deterministic_head_order() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        for (id, session_pk, created_at) in [
            ("claimed", "claimed-session", 0),
            ("b-first", "b", 2),
            ("a-first", "a", 1),
            ("a-second", "a", 3),
        ] {
            store
                .enqueue_session_prompt(QueuedSessionPrompt {
                    id: id.into(),
                    session_pk: session_pk.into(),
                    agent: id.into(),
                    display: id.into(),
                    attachments: vec![],
                    created_at,
                })
                .await
                .unwrap();
        }
        store
            .claim_next_session_prompt("claimed-session")
            .await
            .unwrap();

        assert_eq!(
            store.pending_session_prompt_session_pks().await.unwrap(),
            ["a", "b"]
        );
    }

    #[tokio::test]
    async fn session_prompt_queue_claims_fifo_head_and_reserves_an_idle_session_together() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Idle,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: PermMode::Default,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        for id in ["first", "second"] {
            store
                .enqueue_session_prompt(QueuedSessionPrompt {
                    id: id.into(),
                    session_pk: "s1".into(),
                    agent: id.into(),
                    display: id.into(),
                    attachments: vec![],
                    created_at: 1,
                })
                .await
                .unwrap();
        }

        assert_eq!(
            store
                .claim_next_session_prompt_if_idle("s1")
                .await
                .unwrap()
                .unwrap()
                .id,
            "first"
        );
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().status,
            SessionStatus::Running
        );
        assert!(store
            .claim_next_session_prompt_if_idle("s1")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .list_session_prompt_queue("s1")
                .await
                .unwrap()
                .into_iter()
                .map(|prompt| prompt.id)
                .collect::<Vec<_>>(),
            ["second"]
        );
    }

    #[tokio::test]
    async fn session_prompt_queue_idle_claim_deserialization_error_rolls_back_reservation() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Idle,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                perm_mode: PermMode::Default,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        store
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO session_prompt_queue(id, session_pk, position, payload, created_at) \
                     VALUES ('bad', 's1', 1, 'not json', 1)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        assert!(store.claim_next_session_prompt_if_idle("s1").await.is_err());
        assert_eq!(
            store.get_session("s1").await.unwrap().unwrap().status,
            SessionStatus::Idle
        );
        assert_eq!(
            store
                .with_conn(|connection| {
                    connection.query_row(
                        "SELECT status FROM session_prompt_queue WHERE id='bad'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                })
                .await
                .unwrap(),
            "pending"
        );
    }

    #[tokio::test]
    async fn session_prompt_queue_concurrent_claims_return_one_item() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .enqueue_session_prompt(QueuedSessionPrompt {
                id: "one".into(),
                session_pk: "s1".into(),
                agent: "one".into(),
                display: "one".into(),
                attachments: vec![],
                created_at: 1,
            })
            .await
            .unwrap();

        let one = store.clone();
        let two = store.clone();
        let (first, second) = tokio::join!(
            one.claim_next_session_prompt("s1"),
            two.claim_next_session_prompt("s1")
        );
        assert_eq!(
            [first.unwrap().is_some(), second.unwrap().is_some()]
                .into_iter()
                .filter(|claimed| *claimed)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn pairing_code_is_single_use_and_expiry_bounded() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let now = 1_700_000_000_000_i64;

        store
            .insert_pairing_code("code-hash-1", now + 60_000)
            .await
            .unwrap();

        // First consume succeeds...
        assert!(store
            .consume_pairing_code("code-hash-1", now)
            .await
            .unwrap());
        // ...second consume of the same (now-deleted) code fails.
        assert!(!store
            .consume_pairing_code("code-hash-1", now)
            .await
            .unwrap());

        // An expired code never consumes, even on the first attempt.
        store
            .insert_pairing_code("code-hash-2", now - 1)
            .await
            .unwrap();
        assert!(!store
            .consume_pairing_code("code-hash-2", now)
            .await
            .unwrap());
    }

    fn sample_artifact(
        id: &str,
        source_session_pk: &str,
        status: ArtifactStatus,
    ) -> ArtifactRecord {
        ArtifactRecord {
            id: id.into(),
            source_session_pk: source_session_pk.into(),
            source_message_seq: Some(1),
            source_run_id: Some("run-1".into()),
            creator: ArtifactCreator::Agent,
            creator_id: Some("ada".into()),
            name: "report.md".into(),
            description: Some("summary".into()),
            content_type: Some("text/markdown".into()),
            size_bytes: 42,
            sha256: "deadbeef".into(),
            storage_key: format!("{id}/report.md"),
            status,
            created_at: 1_700_000_000_000,
            deleted_at: None,
        }
    }

    fn sample_reference(
        id: &str,
        artifact_id: &str,
        target_session_pk: &str,
        shared_from_session_pk: &str,
    ) -> ArtifactReference {
        ArtifactReference {
            id: id.into(),
            artifact_id: artifact_id.into(),
            target_session_pk: target_session_pk.into(),
            shared_from_session_pk: shared_from_session_pk.into(),
            shared_by: Some("ada".into()),
            parent_reference_id: None,
            created_at: 1_700_000_000_100,
        }
    }

    #[tokio::test]
    async fn artifact_store_inserts_and_reads_artifact() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let artifact = sample_artifact("art-1", "s1", ArtifactStatus::Available);

        store.insert_artifact(&artifact).await.unwrap();

        let fetched = store.artifact_by_id("art-1").await.unwrap();
        assert_eq!(fetched, Some(artifact));
    }

    #[tokio::test]
    async fn artifact_store_rejects_duplicate_reference_target() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let artifact = sample_artifact("art-1", "s1", ArtifactStatus::Available);
        store.insert_artifact(&artifact).await.unwrap();

        let first = sample_reference("ref-1", "art-1", "s2", "s1");
        store.insert_artifact_reference(&first).await.unwrap();

        // Same artifact + same target session, different reference id: the
        // UNIQUE(artifact_id, target_session_pk) index must reject this.
        let duplicate = sample_reference("ref-2", "art-1", "s2", "s1");
        let err = store.insert_artifact_reference(&duplicate).await;
        assert!(err.is_err(), "duplicate reference target must be rejected");
    }

    #[tokio::test]
    async fn artifact_store_lists_source_and_received_reference() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let artifact = sample_artifact("art-1", "s1", ArtifactStatus::Available);
        store.insert_artifact(&artifact).await.unwrap();
        let reference = sample_reference("ref-1", "art-1", "s2", "s1");
        store.insert_artifact_reference(&reference).await.unwrap();

        let s1_rows = store.artifacts_for_session("s1").await.unwrap();
        assert_eq!(s1_rows.len(), 1);
        assert_eq!(s1_rows[0].artifact, artifact);
        assert_eq!(s1_rows[0].reference, None);

        let s2_rows = store.artifacts_for_session("s2").await.unwrap();
        assert_eq!(s2_rows.len(), 1);
        assert_eq!(s2_rows[0].artifact, artifact);
        assert_eq!(s2_rows[0].reference, Some(reference));
    }

    #[tokio::test]
    async fn artifact_store_scopes_reference_access_to_session() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let artifact = sample_artifact("art-1", "s1", ArtifactStatus::Available);
        store.insert_artifact(&artifact).await.unwrap();
        let reference = sample_reference("ref-1", "art-1", "s2", "s1");
        store.insert_artifact_reference(&reference).await.unwrap();

        // s1 (the originating session) resolves the original artifact id.
        let via_s1 = store.reference_for_session("art-1", "s1").await.unwrap();
        assert_eq!(
            via_s1,
            Some(ArtifactAccessRow {
                artifact: artifact.clone(),
                reference: None,
            })
        );

        // s2 cannot resolve the original artifact id directly...
        let s2_by_original = store.reference_for_session("art-1", "s2").await.unwrap();
        assert_eq!(s2_by_original, None);

        // ...but can resolve it via its own reference id.
        let s2_by_reference = store.reference_for_session("ref-1", "s2").await.unwrap();
        assert_eq!(
            s2_by_reference,
            Some(ArtifactAccessRow {
                artifact: artifact.clone(),
                reference: Some(reference),
            })
        );

        // s3 has no reference and no origination, so neither id resolves.
        let s3_by_original = store.reference_for_session("art-1", "s3").await.unwrap();
        assert_eq!(s3_by_original, None);
        let s3_by_reference = store.reference_for_session("ref-1", "s3").await.unwrap();
        assert_eq!(s3_by_reference, None);
    }

    #[tokio::test]
    async fn artifact_store_marks_source_artifacts_deleted() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        let active = sample_artifact("art-active", "s1", ArtifactStatus::Available);
        let archived = sample_artifact("art-archived", "s1", ArtifactStatus::SourceArchived);
        store.insert_artifact(&active).await.unwrap();
        store.insert_artifact(&archived).await.unwrap();

        let deleted_at = 1_700_000_500_000_i64;
        let mut deleted = store
            .mark_source_artifacts_deleted("s1", deleted_at)
            .await
            .unwrap();
        deleted.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(deleted.len(), 2);
        for record in &deleted {
            assert_eq!(record.status, ArtifactStatus::Deleted);
            assert_eq!(record.deleted_at, Some(deleted_at));
        }

        let refetched = store.artifact_by_id("art-active").await.unwrap().unwrap();
        assert_eq!(refetched.status, ArtifactStatus::Deleted);
        assert_eq!(refetched.deleted_at, Some(deleted_at));

        // Every artifact originated by s1 is already deleted, so a second
        // call finds nothing left to mark.
        let repeated = store
            .mark_source_artifacts_deleted("s1", deleted_at + 1)
            .await
            .unwrap();
        assert!(repeated.is_empty());
    }
}
