import { dirname } from "node:path";
import { buildDaemon } from "@harness/core";
import { writeStatus, clearStatus } from "./daemon-status";

const CONNECT_TIMEOUT_MS = 30000;

export function startWithTimeout(daemon: { start(): Promise<void> }, ms: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timed out connecting after ${ms}ms`)), ms);
    daemon.start().then(
      () => {
        clearTimeout(t);
        resolve();
      },
      (e) => {
        clearTimeout(t);
        reject(e as Error);
      },
    );
  });
}

export function makeShutdown(
  dir: string,
  daemon: { stop(): Promise<void> | void },
  exit: (code: number) => void = (c) => process.exit(c),
): () => Promise<void> {
  let stopping = false;
  return async () => {
    if (stopping) return;
    stopping = true;
    try {
      await daemon.stop();
    } catch {
      /* best effort */
    }
    clearStatus(dir);
    exit(0);
  };
}

export async function runDaemon(deps: { dbPath: string }): Promise<void> {
  const dir = dirname(deps.dbPath);
  const startedAt = Date.now();
  writeStatus(dir, { pid: process.pid, state: "connecting", startedAt });
  const daemon = buildDaemon({ dbPath: deps.dbPath });
  const shutdown = makeShutdown(dir, daemon);
  process.on("SIGTERM", () => void shutdown());
  process.on("SIGINT", () => void shutdown());
  try {
    await startWithTimeout(daemon, CONNECT_TIMEOUT_MS);
    writeStatus(dir, { pid: process.pid, state: "running", startedAt });
    console.log("daemon: running");
  } catch (e) {
    writeStatus(dir, { pid: process.pid, state: "error", startedAt, lastError: (e as Error).message });
    console.error("daemon: failed to start:", (e as Error).message);
    process.exit(1);
  }
  await new Promise<never>(() => {}); // block until a signal triggers shutdown()
}
