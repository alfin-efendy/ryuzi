import { Database } from "bun:sqlite";
import { chmodSync } from "node:fs";
import { dirname } from "node:path";
import { migrateSettings } from "../config/migrate-settings";

function addColumnIfMissing(db: Database, table: string, column: string, decl: string): void {
  // table/column are internal literals, never user input — safe to interpolate.
  const cols = db.query<{ name: string }, []>(`PRAGMA table_info(${table})`).all();
  if (!cols.some((c) => c.name === column)) db.run(`ALTER TABLE ${table} ADD COLUMN ${column} ${decl}`);
}

export function migrate(db: Database): void {
  db.run(`CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT
  )`);
  db.run(`CREATE TABLE IF NOT EXISTS projects (
    project_id TEXT PRIMARY KEY,
    name TEXT,
    workdir TEXT NOT NULL,
    source TEXT,
    harness TEXT NOT NULL DEFAULT 'claude-code',
    model TEXT,
    effort TEXT,
    perm_mode TEXT NOT NULL DEFAULT 'default',
    created_by TEXT,
    created_at INTEGER
  )`);
  db.run(`CREATE TABLE IF NOT EXISTS project_bindings (
    gateway TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    PRIMARY KEY (gateway, workspace_id)
  )`);
  db.run(`CREATE TABLE IF NOT EXISTS sessions (
    session_pk TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    agent_session_id TEXT,
    worktree_path TEXT,
    branch TEXT,
    title TEXT,
    status TEXT NOT NULL DEFAULT 'idle',
    started_by TEXT,
    created_at INTEGER,
    last_active INTEGER,
    resume_attempts INTEGER NOT NULL DEFAULT 0
  )`);
  addColumnIfMissing(db, "sessions", "resume_attempts", "INTEGER NOT NULL DEFAULT 0");
  db.run(`CREATE TABLE IF NOT EXISTS session_surfaces (
    gateway TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    session_pk TEXT NOT NULL,
    PRIMARY KEY (gateway, conversation_id)
  )`);
  db.run(`CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    gateway TEXT,
    conversation_id TEXT,
    actor TEXT,
    action TEXT,
    tool TEXT,
    decision TEXT,
    at INTEGER
  )`);
}

export function openDb(path: string): Database {
  const db = new Database(path);
  if (path !== ":memory:") {
    chmodSync(path, 0o600);
    try {
      chmodSync(dirname(path), 0o700);
    } catch (e) {
      // skip when we don't own the parent dir (e.g. /tmp in tests / root-owned paths);
      // surface anything unexpected (read-only FS, etc.) instead of silently swallowing
      const code = (e as { code?: string }).code;
      if (code !== "EPERM" && code !== "EACCES") throw e;
    }
  }
  db.run("PRAGMA journal_mode = WAL");
  db.run("PRAGMA foreign_keys = ON");
  migrate(db);
  migrateSettings(db);
  return db;
}
