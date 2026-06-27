import type { Database } from "bun:sqlite";
import { SETTING_DEFS, validateSetting } from "./schema";

export class SettingsStore {
  constructor(private db: Database) {}

  get(key: string): string | undefined {
    const row = this.db
      .query<{ value: string }, [string]>("SELECT value FROM settings WHERE key = ?")
      .get(key);
    if (row) return row.value;
    return SETTING_DEFS[key]?.default;
  }

  set(key: string, value: string): void {
    const err = validateSetting(key, value);
    if (err) throw new Error(err);
    this.db.run(
      "INSERT INTO settings(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
      [key, value],
    );
  }

  list(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const r of this.db.query<{ key: string; value: string }, []>("SELECT key, value FROM settings").all()) {
      out[r.key] = r.value;
    }
    return out;
  }

  missingRequired(): string[] {
    return Object.entries(SETTING_DEFS)
      .filter(([k, def]) => def.required && this.get(k) === undefined)
      .map(([k]) => k);
  }
}
