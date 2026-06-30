import { existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";

export type HandoffPhase = "probing" | "healthy" | "failed" | "promote" | "promoted";

export interface Handoff {
  phase: HandoffPhase;
  pid: number;
  version: string;
  detail?: string;
}

function path(dir: string): string {
  return join(dir, "update.json");
}

export function readHandoff(dir: string): Handoff | null {
  const p = path(dir);
  if (!existsSync(p)) return null;
  try {
    return JSON.parse(readFileSync(p, "utf8")) as Handoff;
  } catch {
    return null;
  }
}

export function writeHandoff(dir: string, h: Handoff): void {
  writeFileSync(path(dir), JSON.stringify(h));
}

export function clearHandoff(dir: string): void {
  try {
    rmSync(path(dir));
  } catch {
    /* already gone */
  }
}
