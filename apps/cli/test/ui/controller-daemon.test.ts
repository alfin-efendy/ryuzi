import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AppController } from "../../src/cli/ui/controller";
import { detectClaude, detectGit } from "@harness/core";
import { writeStatus } from "../../src/cli/daemon-status";

function configured() {
  const root = mkdtempSync(join(tmpdir(), "hr-ctl-"));
  const spawns: string[][] = [];
  const kills: Array<[number, string | number]> = [];
  const c = new AppController({
    dbPath: join(root, "db.sqlite"),
    detect: { claude: detectClaude, git: detectGit },
    dataDir: root,
    spawnDaemon: (cmd) => {
      spawns.push(cmd);
      return { pid: 4242 };
    },
    killDaemon: (pid, sig) => {
      kills.push([pid, sig]);
    },
  });
  for (const [k, v] of [
    ["discord.token", "t"],
    ["discord.app_id", "a"],
    ["discord.guild_id", "g"],
    ["workdir_root", root],
  ] as const)
    c.set(k, v);
  return { c, root, spawns, kills };
}

test("startDaemon spawns the detached __daemon process when configured", async () => {
  const { c, spawns } = configured();
  await c.startDaemon();
  expect(spawns).toHaveLength(1);
  expect(spawns[0]![0]).toBe(process.execPath);
  expect(spawns[0]![1]).toBe(Bun.main); // dev mode passes the script path
  expect(spawns[0]!.at(-1)).toBe("__daemon");
});

test("daemon() reflects the status file the daemon writes", async () => {
  const { c, root } = configured();
  expect(c.daemon().running).toBe(false);
  writeStatus(root, { pid: process.pid, state: "connecting", startedAt: 1 });
  expect(c.daemon().starting).toBe(true);
  writeStatus(root, { pid: process.pid, state: "running", startedAt: 1 });
  expect(c.daemon().running).toBe(true);
  writeStatus(root, { pid: process.pid, state: "error", startedAt: 1, lastError: "boom" });
  expect(c.daemon().lastError).toBe("boom");
});

test("startDaemon records an error (no spawn) when required settings missing", async () => {
  const root = mkdtempSync(join(tmpdir(), "hr-ctl2-"));
  const spawns: string[][] = [];
  const c = new AppController({
    dbPath: join(root, "db.sqlite"),
    detect: { claude: detectClaude, git: detectGit },
    dataDir: root,
    spawnDaemon: (cmd) => {
      spawns.push(cmd);
      return { pid: 1 };
    },
  }); // migration seeds enabled_gateways=discord, but discord.token unset
  await c.startDaemon();
  expect(spawns).toHaveLength(0);
  expect(c.daemon().lastError).toMatch(/missing/i);
});

test("stopDaemon SIGTERMs the running daemon pid", () => {
  const { c, root, kills } = configured();
  writeStatus(root, { pid: process.pid, state: "running", startedAt: 1 });
  c.stopDaemon();
  expect(kills).toEqual([[process.pid, "SIGTERM"]]);
});
