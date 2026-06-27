import { test, expect } from "bun:test";
import { isCompiledExecutable, daemonRelaunchCmd } from "../../src/cli/daemon-spawn";

test("isCompiledExecutable detects the Bun standalone virtual paths", () => {
  expect(isCompiledExecutable("/$bunfs/root/hr")).toBe(true);
  expect(isCompiledExecutable("B:\\~BUN\\root\\hr")).toBe(true);
  expect(isCompiledExecutable("/home/me/app/src/cli/index.ts")).toBe(false);
});

test("daemonRelaunchCmd drops the script path when compiled", () => {
  expect(daemonRelaunchCmd({ execPath: "/usr/local/bin/hr", main: "/$bunfs/root/hr", compiled: true }))
    .toEqual(["/usr/local/bin/hr", "__daemon"]);
});

test("daemonRelaunchCmd keeps the script path in dev mode", () => {
  expect(daemonRelaunchCmd({ execPath: "/home/me/.bun/bin/bun", main: "/home/me/app/index.ts", compiled: false }))
    .toEqual(["/home/me/.bun/bin/bun", "/home/me/app/index.ts", "__daemon"]);
});
