import type { Handoff } from "./update-handoff";

export interface ApplierDeps {
  dir: string;
  installPath: string;
  repo: string;
  tag: string;
  version: string;
  stage: () => Promise<{ ok: boolean; canaryPath?: string; error?: string }>;
  spawnCanary: (canaryPath: string) => { pid: number };
  readHandoff: () => Handoff | null;
  writeHandoff: (h: Handoff) => void;
  clearHandoff: () => void;
  drain: (ms: number) => Promise<void>;
  drainTimeoutMs: number;
  backup: () => void;
  swap: () => void;
  restore: () => void;
  killCanary: (pid: number) => void;
  stopGateways: () => Promise<void>;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
  canaryTimeoutMs: number;
  log: (m: string) => void;
}

export type ApplyOutcome = "promoted" | "aborted" | "rolledback";

async function waitFor(deps: ApplierDeps, pred: (h: Handoff | null) => boolean): Promise<Handoff | null> {
  const deadline = deps.now() + deps.canaryTimeoutMs;
  while (deps.now() < deadline) {
    const h = deps.readHandoff();
    if (pred(h)) return h;
    if (h?.phase === "failed") return h;
    await deps.sleep(100);
  }
  return deps.readHandoff();
}

export async function applyUpdate(deps: ApplierDeps): Promise<ApplyOutcome> {
  deps.log(`update: applying ${deps.version}`);
  const staged = await deps.stage();
  if (!staged.ok || !staged.canaryPath) {
    deps.log(`update: stage failed: ${staged.error ?? "unknown"}`);
    return "aborted";
  }

  const { pid } = deps.spawnCanary(staged.canaryPath);

  // Wait for the canary's health verdict.
  const verdict = await waitFor(deps, (h) => h?.phase === "healthy");
  if (verdict?.phase !== "healthy") {
    deps.log(`update: canary unhealthy (${verdict?.detail ?? "timeout"}), staying on ${deps.version}`);
    deps.killCanary(pid);
    deps.clearHandoff();
    return "aborted"; // old daemon never stopped → zero downtime
  }

  // Green: finish in-flight turns, swap the binary, hand over.
  await deps.drain(deps.drainTimeoutMs);
  deps.backup();
  deps.swap(); // atomic rename .hr.canary → hr
  deps.writeHandoff({ phase: "promote", pid, version: deps.version });
  await deps.stopGateways();

  // Watchdog: confirm the canary becomes the live daemon.
  const promoted = await waitFor(deps, (h) => h?.phase === "promoted");
  if (promoted?.phase === "promoted") {
    deps.log(`update: promoted to ${deps.version}`);
    deps.clearHandoff();
    return "promoted";
  }

  // Canary failed to promote → roll back to the backed-up binary.
  deps.log(`update: canary failed to promote, rolling back to previous binary`);
  deps.killCanary(pid);
  deps.restore(); // rename hr.bak → hr
  deps.clearHandoff();
  return "rolledback";
}
