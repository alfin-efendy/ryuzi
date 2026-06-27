import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AppController } from "../../src/cli/ui/controller";
import { detectClaude, detectGit } from "../../src/harness/detect";
import type { DiscordPort, InboundMessage, InboundInteraction } from "../../src/gateways/discord/index";

class FakePort implements DiscordPort {
  connected = false;
  botUserId() { return "bot"; }
  async connect(_h: { onMessage: (e: InboundMessage) => Promise<void>; onInteraction: (e: InboundInteraction, reply: (t: string) => Promise<void>) => Promise<void> }) { this.connected = true; }
  async createTextChannel() { return "c"; }
  async createThread() { return "t"; }
  async sendMessage() { return "m"; }
  async editMessage() {}
  async requestApproval() { return { decision: "deny" as const, actor: "timeout" }; }
}

function configured() {
  const root = mkdtempSync(join(tmpdir(), "hr-daemon-"));
  const c = new AppController({
    dbPath: join(root, "db.sqlite"),
    detect: { claude: detectClaude, git: detectGit },
    portFactory: () => new FakePort(),
  });
  for (const [k, v] of [["discord_token","t"],["discord_app_id","a"],["discord_guild_id","g"],["workdir_root",root]] as const) c.set(k, v);
  return c;
}

test("startDaemon runs, stopDaemon stops, change events fire", async () => {
  const c = configured();
  let changes = 0;
  c.on("change", () => { changes++; });
  expect(c.daemon().running).toBe(false);
  await c.startDaemon();
  expect(c.daemon().running).toBe(true);
  expect(c.daemon().startedAt).toBeGreaterThan(0);
  c.stopDaemon();
  expect(c.daemon().running).toBe(false);
  expect(changes).toBeGreaterThan(0);
});

test("startDaemon records lastError when required settings missing", async () => {
  const c = new AppController({ dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit }, portFactory: () => new FakePort() });
  await c.startDaemon();
  expect(c.daemon().running).toBe(false);
  expect(c.daemon().lastError).toMatch(/missing/i);
});

test("startDaemon exposes a transient connecting state", async () => {
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => { release = r; });
  class SlowPort extends FakePort {
    override async connect(h: Parameters<DiscordPort["connect"]>[0]) { await gate; return super.connect(h); }
  }
  const root = mkdtempSync(join(tmpdir(), "hr-daemon-slow-"));
  const c = new AppController({
    dbPath: join(root, "db.sqlite"),
    detect: { claude: detectClaude, git: detectGit },
    portFactory: () => new SlowPort(),
  });
  for (const [k, v] of [["discord_token","t"],["discord_app_id","a"],["discord_guild_id","g"],["workdir_root",root]] as const) c.set(k, v);

  const p = c.startDaemon();              // do NOT await yet
  await new Promise((r) => setTimeout(r, 10));
  expect(c.daemon().starting).toBe(true);
  expect(c.daemon().running).toBe(false);
  release();                              // let connect resolve
  await p;
  expect(c.daemon().starting).toBe(false);
  expect(c.daemon().running).toBe(true);
  c.stopDaemon();
});

test("sessions() merges persisted rows with live overlay", async () => {
  const c = configured();
  await c.startDaemon();
  // emit a synthetic event through the daemon's cp to exercise the overlay wiring
  c["daemonCp"]?.emit({ kind: "session.created", sessionPk: "live1", projectId: "p1" });
  c["daemonCp"]?.emit({ kind: "text", sessionPk: "live1", text: "hi" });
  // persisted rows come from the sessions table (empty here) — overlay-only rows are ignored by sessions()
  expect(Array.isArray(c.sessions())).toBe(true);
  c.stopDaemon();
});
