import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readStatus, writeStatus, clearStatus, isAlive, deriveState } from "../../src/cli/daemon-status";

function dir() {
  return mkdtempSync(join(tmpdir(), "hr-status-"));
}

test("write/read/clear round-trip", () => {
  const d = dir();
  expect(readStatus(d)).toBeNull();
  writeStatus(d, { pid: 123, state: "running", startedAt: 1000 });
  expect(readStatus(d)).toEqual({ pid: 123, state: "running", startedAt: 1000 });
  clearStatus(d);
  expect(readStatus(d)).toBeNull();
});

test("isAlive: self is alive, absurd pid is not", () => {
  expect(isAlive(process.pid)).toBe(true);
  expect(isAlive(2147483646)).toBe(false);
});

test("deriveState maps status to UI state", () => {
  const alive = (pid: number) => pid === 1;
  expect(deriveState(null, alive)).toEqual({ running: false });
  expect(deriveState({ pid: 1, state: "running", startedAt: 5 }, alive)).toEqual({ running: true, startedAt: 5 });
  expect(deriveState({ pid: 1, state: "connecting", startedAt: 5 }, alive)).toEqual({ running: false, starting: true, startedAt: 5 });
  expect(deriveState({ pid: 9, state: "running", startedAt: 5 }, alive)).toEqual({ running: false }); // dead pid
  expect(deriveState({ pid: 9, state: "error", startedAt: 5, lastError: "boom" }, alive)).toEqual({ running: false, lastError: "boom" });
});
