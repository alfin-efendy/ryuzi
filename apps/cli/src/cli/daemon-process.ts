import { dirname, join } from "node:path";
import { renameSync, writeFileSync, chmodSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { buildDaemon, detectPlatform } from "@harness/core";
import { writeStatus, clearStatus } from "./daemon-status";
import { UpdateManager } from "./update-manager";
import { applyUpdate, type ApplierDeps } from "./update-applier";
import { stageCanary, defaultExtractHr } from "./update-stage";
import { readHandoff, writeHandoff, clearHandoff } from "./update-handoff";
import { daemonRelaunchCmd } from "./daemon-spawn";
import { version } from "./meta";
import { isCompiledExecutable } from "./daemon-spawn";

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

  // Build the real applyUpdate if this platform supports self-apply.
  const platform = detectPlatform();
  const productionApplyUpdate =
    platform !== null
      ? async (info: { repo: string; tag: string; version: string }) => {
          const installPath = process.execPath;
          const applierDeps: ApplierDeps = {
            dir,
            installPath,
            repo: info.repo,
            tag: info.tag,
            version: info.version,
            stage: () =>
              stageCanary(
                { repo: info.repo, tag: info.tag, version: info.version, installPath },
                {
                  fetchImpl: fetch,
                  extractHr: defaultExtractHr,
                  writeFile: (p, b, m) => {
                    writeFileSync(p, b);
                    chmodSync(p, m);
                  },
                  platform,
                  tmpDir: mkdtempSync(join(tmpdir(), "hr-up-")),
                },
              ),
            spawnCanary: (canaryPath: string) => {
              const cmd = [...daemonRelaunchCmd({ execPath: canaryPath, main: Bun.main, compiled: isCompiledExecutable() }), "--canary"];
              const proc = Bun.spawn(cmd, {
                detached: true,
                stdio: ["ignore", "ignore", "ignore"],
                env: { ...process.env, HARNESS_CANARY_TARGET: info.version },
              });
              proc.unref();
              return { pid: proc.pid };
            },
            readHandoff: () => readHandoff(dir),
            writeHandoff: (h) => writeHandoff(dir, h),
            clearHandoff: () => clearHandoff(dir),
            drain: (ms) => daemon.cp.drain(ms),
            drainTimeoutMs: Number(daemon.settings.get("auto_update_drain_timeout_ms") ?? "300000"),
            backup: () => renameSync(installPath, `${installPath}.bak`),
            swap: () => renameSync(join(dirname(installPath), ".hr.canary"), installPath),
            restore: () => renameSync(`${installPath}.bak`, installPath),
            killCanary: (pid: number) => {
              try {
                process.kill(pid, "SIGTERM");
              } catch {
                /* already gone */
              }
            },
            stopGateways: () => daemon.stop(),
            now: () => Date.now(),
            sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
            canaryTimeoutMs: Number(daemon.settings.get("auto_update_canary_timeout_ms") ?? "60000"),
            log: (m) => console.log(m),
          };
          await applyUpdate(applierDeps);
        }
      : undefined;

  const updater = new UpdateManager({
    cp: daemon.cp,
    settings: daemon.settings,
    version: version(),
    execPath: process.execPath,
    compiled: isCompiledExecutable(),
    log: (m) => console.log(m),
    applyUpdate: productionApplyUpdate,
  });
  const shutdown = makeShutdown(dir, {
    stop: async () => {
      updater.stop();
      await daemon.stop();
    },
  });
  process.on("SIGTERM", () => void shutdown());
  process.on("SIGINT", () => void shutdown());
  try {
    await startWithTimeout(daemon, CONNECT_TIMEOUT_MS);
    writeStatus(dir, { pid: process.pid, state: "running", startedAt });
    console.log("daemon: running");
    updater.start();
  } catch (e) {
    writeStatus(dir, { pid: process.pid, state: "error", startedAt, lastError: (e as Error).message });
    console.error("daemon: failed to start:", (e as Error).message);
    process.exit(1);
  }
  await new Promise<never>(() => {}); // block until a signal triggers shutdown()
}
