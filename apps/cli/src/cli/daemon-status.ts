import { existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";

export interface DaemonStatusFile {
  pid: number;
  state: "connecting" | "running" | "error";
  startedAt: number;
  lastError?: string;
}

export interface DaemonState {
  running: boolean;
  startedAt?: number;
  lastError?: string;
  starting?: boolean;
}

function statusPath(dir: string): string {
  return join(dir, "daemon.json");
}

export function readStatus(dir: string): DaemonStatusFile | null {
  const p = statusPath(dir);
  if (!existsSync(p)) return null;
  try {
    return JSON.parse(readFileSync(p, "utf8")) as DaemonStatusFile;
  } catch {
    return null;
  }
}

export function writeStatus(dir: string, s: DaemonStatusFile): void {
  writeFileSync(statusPath(dir), JSON.stringify(s));
}

export function clearStatus(dir: string): void {
  try {
    rmSync(statusPath(dir));
  } catch {
    /* already gone */
  }
}

// NOTE: bare liveness check via signal 0 — cannot distinguish our daemon from an
// unrelated process that reused the pid after a hard-kill (SIGKILL/OOM). Acceptable
// for now; a future hardening could re-stamp daemon.json as a heartbeat or verify identity.
export function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export function deriveState(s: DaemonStatusFile | null, alive: (pid: number) => boolean): DaemonState {
  if (!s) return { running: false };
  if (s.state === "error") return { running: false, lastError: s.lastError };
  if (s.pid > 0 && alive(s.pid)) {
    if (s.state === "connecting") return { running: false, starting: true, startedAt: s.startedAt };
    return { running: true, startedAt: s.startedAt };
  }
  return { running: false };
}
