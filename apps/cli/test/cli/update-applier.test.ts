import { test, expect } from "bun:test";
import { applyUpdate, type ApplierDeps } from "../../src/cli/update-applier";
import type { Handoff } from "../../src/cli/update-handoff";

function base(over: Partial<ApplierDeps>, script: (Handoff | null)[]): { deps: ApplierDeps; calls: string[] } {
  const calls: string[] = [];
  let i = 0;

  // Default stage implementation — can be overridden via over.stage.
  // We wrap the override so the "stage" call is always tracked in calls[].
  const defaultStage: ApplierDeps["stage"] = async () => {
    return { ok: true, canaryPath: "/home/me/.local/bin/.hr.canary" };
  };
  const stageImpl = over.stage ?? defaultStage;
  const trackedStage: ApplierDeps["stage"] = async () => {
    calls.push("stage");
    return stageImpl();
  };

  const { stage: _ignored, ...restOver } = over;

  const deps: ApplierDeps = {
    dir: "/d",
    installPath: "/home/me/.local/bin/hr",
    repo: "o/r",
    tag: "v0.3.0",
    version: "0.3.0",
    stage: trackedStage,
    spawnCanary: () => {
      calls.push("spawnCanary");
      return { pid: 999 };
    },
    readHandoff: () => script[Math.min(i++, script.length - 1)] ?? null,
    writeHandoff: (h) => calls.push("handoff:" + h.phase),
    clearHandoff: () => calls.push("clearHandoff"),
    drain: async () => {
      calls.push("drain");
    },
    drainTimeoutMs: 1000,
    backup: () => calls.push("backup"),
    swap: () => calls.push("swap"),
    restore: () => calls.push("restore"),
    killCanary: () => calls.push("killCanary"),
    stopGateways: async () => {
      calls.push("stopGateways");
    },
    now: () => 0,
    sleep: async () => {},
    canaryTimeoutMs: 1000,
    log: () => {},
    ...restOver,
  };
  return { deps, calls };
}

test("happy path: stage → spawn → healthy → drain → backup → swap → promote → promoted", async () => {
  const { deps, calls } = base({}, [
    { phase: "probing", pid: 999, version: "0.3.0" },
    { phase: "healthy", pid: 999, version: "0.3.0" },
    { phase: "promoted", pid: 999, version: "0.3.0" },
  ]);
  const outcome = await applyUpdate(deps);
  expect(outcome).toBe("promoted");
  // backup + atomic swap happen, and only AFTER drain; promote is signalled before stopGateways;
  // clearHandoff is called after confirmed promoted
  expect(calls).toEqual(["stage", "spawnCanary", "drain", "backup", "swap", "handoff:promote", "stopGateways", "clearHandoff"]);
});

test("canary unhealthy: aborts, kills canary, no swap, old daemon keeps serving", async () => {
  const { deps, calls } = base({}, [
    { phase: "probing", pid: 999, version: "0.3.0" },
    { phase: "failed", pid: 999, version: "0.3.0", detail: "db" },
  ]);
  const outcome = await applyUpdate(deps);
  expect(outcome).toBe("aborted");
  expect(calls).toContain("killCanary");
  expect(calls).not.toContain("swap");
  expect(calls).not.toContain("stopGateways");
  expect(calls).toContain("clearHandoff");
});

test("stage failure: aborts before spawning a canary", async () => {
  const { deps, calls } = base({ stage: async () => ({ ok: false, error: "checksum" }) }, []);
  const outcome = await applyUpdate(deps);
  expect(outcome).toBe("aborted");
  expect(calls).toEqual(["stage"]);
});

test("canary dies after promote signal: rollback restores the old binary", async () => {
  // healthy → we promote → but canary never reaches 'promoted' and its pid dies → rollback
  const { deps, calls } = base(
    {
      // after promote signal, handoff stays 'healthy' (never 'promoted'); pid considered dead via killCanary check
      now: (() => {
        let t = 0;
        return () => (t += 600); // advance past canaryTimeoutMs quickly
      })(),
    },
    [
      { phase: "probing", pid: 999, version: "0.3.0" },
      { phase: "healthy", pid: 999, version: "0.3.0" },
      { phase: "healthy", pid: 999, version: "0.3.0" }, // never promoted
    ],
  );
  const outcome = await applyUpdate(deps);
  expect(outcome).toBe("rolledback");
  expect(calls).toContain("restore");
  expect(calls).toContain("clearHandoff");
});
