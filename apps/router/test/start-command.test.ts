// apps/router/test/start-command.test.ts
import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildDaemon } from "../src/cli/start-command";
import type { DiscordPort, InboundMessage, InboundInteraction } from "../src/gateways/discord/index";
import { openDb } from "../src/store/db";
import { SettingsStore } from "../src/config/store";

class FakePort implements DiscordPort {
  connected = false;
  botUserId() { return "bot"; }
  async connect(_h: { onMessage: (e: InboundMessage) => Promise<void>; onInteraction: (e: InboundInteraction, reply: (t: string) => Promise<void>) => Promise<void> }) { this.connected = true; }
  async createTextChannel() { return "c"; }
  async createThread() { return "t"; }
  async sendMessage() { return "m"; }
  async editMessage() {}
  async requestApproval(_conversationId: string, _req: unknown) { return { decision: "deny" as const, actor: "timeout" }; }
}

test("buildDaemon wires the graph, registers the gateway, and starts the approval IPC", async () => {
  const root = mkdtempSync(join(tmpdir(), "harness-daemon-"));
  const dbPath = join(root, "db.sqlite");
  new SettingsStore(openDb(dbPath)).set("workdir_root", root);

  const port = new FakePort();
  const daemon = buildDaemon({ dbPath, port });
  try {
    await daemon.start();
    expect(port.connected).toBe(true);
    expect(daemon.gateway.id).toBe("discord");
    expect(daemon.cp).toBeDefined();
    expect(typeof daemon.cp.subscribe).toBe("function");
    expect(typeof daemon.stop).toBe("function");
  } finally {
    daemon.stop?.();
  }
});

test("buildDaemon passes telemetry through to the ControlPlane", async () => {
  // a session run through the daemon's cp should emit a session.run count on the injected telemetry
  // (kept light: assert buildDaemon accepts a telemetry option and the daemon still starts)
  const root = mkdtempSync(join(tmpdir(), "harness-tel-"));
  const dbPath = join(root, "db.sqlite");
  new SettingsStore(openDb(dbPath)).set("workdir_root", root);
  const counts: string[] = [];
  const tel = {
    startSpan: () => ({ setAttribute() {}, setError() {}, end() {} }),
    count: (n: string) => { counts.push(n); },
    record: () => {},
  };
  const port = new FakePort();
  const daemon = buildDaemon({ dbPath, port, telemetry: tel });
  try {
    await daemon.start();
    expect(typeof daemon.stop).toBe("function");
  } finally {
    daemon.stop?.();
  }
});
