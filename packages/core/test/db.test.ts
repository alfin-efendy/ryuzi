import { test, expect } from "bun:test";
import { statSync, unlinkSync } from "node:fs";
import { openDb } from "../src/store/db";

test("migrate creates all tables", () => {
  const db = openDb(":memory:");
  const rows = db
    .query<{ name: string }, []>("SELECT name FROM sqlite_master WHERE type='table'")
    .all()
    .map((r) => r.name);
  for (const t of ["settings", "projects", "project_bindings", "sessions", "session_surfaces", "audit"]) {
    expect(rows).toContain(t);
  }
});

test("settings round-trips a value", () => {
  const db = openDb(":memory:");
  db.run("INSERT INTO settings(key, value) VALUES (?, ?)", ["k", "v"]);
  const got = db.query<{ value: string }, [string]>("SELECT value FROM settings WHERE key=?").get("k");
  expect(got?.value).toBe("v");
});

test("openDb sets file permissions to owner-only (no group/other read)", () => {
  const path = `/tmp/harness-perm-${Bun.hash(Math.random().toString())}.sqlite`;
  const db = openDb(path);
  db.close();
  const mode = statSync(path).mode & 0o777;
  // Assert no group or other bits are set (chmod 600 or stricter).
  // We test (mode & 0o077) === 0 rather than exact 0o600 equality because
  // some Linux/WSL filesystems may leave the sticky bit or other bits set by
  // the kernel after the chmod call, but they must never leave group/other
  // readable bits.
  expect(mode & 0o077).toBe(0);
  unlinkSync(path);
});
