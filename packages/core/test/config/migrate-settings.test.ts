import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { migrate } from "../../src/store/db";
import { migrateSettings } from "../../src/config/migrate-settings";

function db() {
  const d = new Database(":memory:");
  migrate(d);
  return d;
}
const get = (d: Database, k: string) => d.query<{ value: string }, [string]>("SELECT value FROM settings WHERE key=?").get(k)?.value;
const set = (d: Database, k: string, v: string) =>
  d.run("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [k, v]);

test("maps flat discord keys to namespaced and removes old", () => {
  const d = db();
  set(d, "discord_token", "tok");
  set(d, "discord_app_id", "app");
  set(d, "discord_guild_id", "gid");
  migrateSettings(d);
  expect(get(d, "discord.token")).toBe("tok");
  expect(get(d, "discord.app_id")).toBe("app");
  expect(get(d, "discord_token")).toBeUndefined();
});

test("seeds enabled sets when unset and is idempotent", () => {
  const d = db();
  migrateSettings(d);
  expect(get(d, "enabled_gateways")).toBe("discord");
  expect(get(d, "enabled_runtimes")).toBe("claude-code");
  expect(get(d, "default_runtime")).toBe("claude-code");
  set(d, "enabled_gateways", "discord,telegram");
  migrateSettings(d); // idempotent: does not clobber existing
  expect(get(d, "enabled_gateways")).toBe("discord,telegram");
});
