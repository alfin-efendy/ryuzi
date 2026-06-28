import { test, expect } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { serveInfoPath, writeServeInfo, removeServeInfo } from "../../src/cli/serve-command";

test("writeServeInfo writes {url,token} json next to the db, removeServeInfo deletes it", () => {
  const dir = join(tmpdir(), `hr-serveinfo-${crypto.randomUUID()}`);
  const dbPath = join(dir, "harness.sqlite");
  expect(serveInfoPath(dbPath)).toBe(join(dir, "serve.json"));
  writeServeInfo(dbPath, {
    url: "http://127.0.0.1:8787",
    token: "tok123",
  });
  const parsed = JSON.parse(require("node:fs").readFileSync(serveInfoPath(dbPath), "utf8"));
  expect(parsed).toEqual({
    url: "http://127.0.0.1:8787",
    token: "tok123",
  });
  removeServeInfo(dbPath);
  expect(existsSync(serveInfoPath(dbPath))).toBe(false);
  rmSync(dir, { recursive: true, force: true });
});

test("removeServeInfo is a no-op when the file is absent", () => {
  const dir = join(tmpdir(), `hr-serveinfo-${crypto.randomUUID()}`);
  expect(() => removeServeInfo(join(dir, "harness.sqlite"))).not.toThrow();
});
