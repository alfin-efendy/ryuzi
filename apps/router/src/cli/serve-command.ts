// apps/router/src/cli/serve-command.ts
import { dirname, join } from "node:path";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { buildDaemon } from "./start-command";
import { startServeServer } from "../serve/index";
import { SettingsStore } from "../config/store";
import { openDb } from "../store/db";
import type { CliDeps } from "./run";

export function serveTokenPath(dbPath: string): string {
  return join(dirname(dbPath), "serve-token");
}

// A stable per-install loopback token, generated on first use.
export function ensureLocalToken(dbPath: string): string {
  const p = serveTokenPath(dbPath);
  if (existsSync(p)) return readFileSync(p, "utf8").trim();
  mkdirSync(dirname(p), { recursive: true });
  const token = crypto.randomUUID().replaceAll("-", "");
  writeFileSync(p, token, { mode: 0o600 });
  return token;
}

export async function cmdServe(_args: string[], deps: CliDeps): Promise<number> {
  const db = openDb(deps.dbPath);
  const settings = new SettingsStore(db);
  const daemon = buildDaemon({ dbPath: deps.dbPath, db });
  await daemon.start();

  const host = settings.get("serve.host") ?? "127.0.0.1";
  const port = Number(settings.get("serve.port") ?? "8787");
  const localToken = ensureLocalToken(deps.dbPath);
  const server = startServeServer(daemon.cp, { settings, host, port, localToken });

  deps.io.out(`serve: listening on ${server.url} (auth: ${settings.get("serve.auth_mode") ?? "loopback"})`);

  const shutdown = () => {
    server.stop();
    void daemon.stop();
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
  await new Promise<never>(() => {}); // block until signal
  return 0;
}
