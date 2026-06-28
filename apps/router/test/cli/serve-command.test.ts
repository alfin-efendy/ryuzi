// apps/router/test/cli/serve-command.test.ts
import { test, expect } from "bun:test";
import { ensureLocalToken, serveTokenPath } from "../../src/cli/serve-command";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { rmSync } from "node:fs";

test("ensureLocalToken creates a stable token next to the db", () => {
  const dir = join(tmpdir(), `hr-serve-${crypto.randomUUID()}`);
  const dbPath = join(dir, "harness.sqlite");
  const t1 = ensureLocalToken(dbPath);
  const t2 = ensureLocalToken(dbPath);
  expect(t1).toBe(t2); // stable across calls
  expect(t1.length).toBeGreaterThan(10);
  expect(serveTokenPath(dbPath)).toBe(join(dir, "serve-token"));
  rmSync(dir, { recursive: true, force: true });
});
