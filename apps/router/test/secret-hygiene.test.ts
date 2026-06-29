import { test, expect } from "bun:test";
import { mkdtempSync, statSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, SettingsStore } from "@harness/core";
import { runCli, type CliDeps } from "../src/cli/run";

test("openDb tightens the DB parent directory to 0700", () => {
  const dir = mkdtempSync(join(tmpdir(), "hr-perm-"));
  chmodSync(dir, 0o755); // simulate a umask-default (world-traversable) dir
  const db = openDb(join(dir, "harness.sqlite"));
  db.close();
  expect(statSync(dir).mode & 0o777).toBe(0o700);
});

function cliDeps(dbPath: string, out: string[]): CliDeps {
  return {
    io: { out: (s) => out.push(s), err: () => {}, prompt: async () => "" },
    dbPath,
    detect: {
      claude: async () => ({ found: true, version: "x" }),
      git: async () => ({ found: true, version: "x" }),
    },
  };
}

test("hr config get masks a secret by default and reveals with --reveal", async () => {
  const dir = mkdtempSync(join(tmpdir(), "hr-cfg-"));
  const dbPath = join(dir, "harness.sqlite");
  new SettingsStore(openDb(dbPath)).set("discord.token", "supersecret");

  const masked: string[] = [];
  await runCli(["config", "get", "discord.token"], cliDeps(dbPath, masked));
  expect(masked.join("\n")).not.toContain("supersecret");
  expect(masked.join("\n")).toContain("••••••••");

  const revealed: string[] = [];
  await runCli(["config", "get", "discord.token", "--reveal"], cliDeps(dbPath, revealed));
  expect(revealed.join("\n")).toContain("supersecret");

  // non-secret keys are unaffected by masking
  const plain: string[] = [];
  new SettingsStore(openDb(dbPath)).set("default_effort", "high");
  await runCli(["config", "get", "default_effort"], cliDeps(dbPath, plain));
  expect(plain.join("\n")).toContain("high");
});
