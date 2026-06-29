import type { Database } from "bun:sqlite";

const KEY_MAP: Record<string, string> = {
  discord_token: "discord.token",
  discord_app_id: "discord.app_id",
  discord_guild_id: "discord.guild_id",
};

export function migrateSettings(db: Database): void {
  const get = (k: string) => db.query<{ value: string }, [string]>("SELECT value FROM settings WHERE key=?").get(k);
  const set = (k: string, v: string) =>
    db.run("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [k, v]);
  const del = (k: string) => db.run("DELETE FROM settings WHERE key=?", [k]);
  for (const [oldK, newK] of Object.entries(KEY_MAP)) {
    const old = get(oldK);
    if (old && !get(newK)) set(newK, old.value);
    if (old) del(oldK);
  }
  if (!get("enabled_gateways")) set("enabled_gateways", "discord");
  if (!get("enabled_runtimes")) set("enabled_runtimes", "claude-code");
  if (!get("default_runtime")) set("default_runtime", "claude-code");
}
