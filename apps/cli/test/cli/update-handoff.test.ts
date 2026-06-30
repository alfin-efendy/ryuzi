import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readHandoff, writeHandoff, clearHandoff } from "../../src/cli/update-handoff";

function dir() {
  return mkdtempSync(join(tmpdir(), "hr-handoff-"));
}

test("read returns null when absent; write then read round-trips; clear removes", () => {
  const d = dir();
  expect(readHandoff(d)).toBeNull();
  writeHandoff(d, { phase: "probing", pid: 4242, version: "0.3.0" });
  expect(readHandoff(d)).toEqual({ phase: "probing", pid: 4242, version: "0.3.0" });
  writeHandoff(d, { phase: "failed", pid: 4242, version: "0.3.0", detail: "db open failed" });
  expect(readHandoff(d)?.phase).toBe("failed");
  expect(readHandoff(d)?.detail).toBe("db open failed");
  clearHandoff(d);
  expect(readHandoff(d)).toBeNull();
});

test("read returns null on corrupt json", () => {
  const d = dir();
  writeHandoff(d, { phase: "promoted", pid: 1, version: "0.3.0" });
  Bun.write(join(d, "update.json"), "{not json");
  expect(readHandoff(d)).toBeNull();
});
