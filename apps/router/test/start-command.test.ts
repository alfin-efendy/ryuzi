import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildDaemon } from "../src/cli/start-command";
import { openDb } from "../src/store/db";
import { SettingsStore } from "../src/config/store";
import { makeCatalog } from "../src/providers/catalog";
import type { GatewayDescriptor } from "../src/providers/types";
import type { Gateway } from "../src/gateways/types";

class FakeGateway implements Gateway {
  readonly id = "fake"; started = false;
  async start() { this.started = true; }
  async createWorkspace() { return "w"; } async createConversation() { return "c"; }
  async postStatus() { return { surface: { gateway: "fake", conversationId: "c" }, messageId: "m" }; }
  async editStatus() {} async postResult() {} async postError() {}
  async requestApproval() { return { decision: "deny" as const, actor: "x" }; }
}

test("buildDaemon builds + starts only enabled gateways and exposes cp", async () => {
  const root = mkdtempSync(join(tmpdir(), "hr-daemon-"));
  const dbPath = join(root, "db.sqlite");
  const s = new SettingsStore(openDb(dbPath));
  s.set("workdir_root", root); s.set("enabled_gateways", "fake"); s.set("enabled_runtimes", "");
  const built: FakeGateway[] = [];
  const gw: GatewayDescriptor = { id: "fake", label: "Fake", description: "", kind: "gateway", fields: [], build: () => { const g = new FakeGateway(); built.push(g); return g; } };
  const daemon = buildDaemon({ dbPath, catalog: makeCatalog([gw], []) });
  try {
    await daemon.start();
    expect(built[0]!.started).toBe(true);
    expect(daemon.gateways.map((g) => g.id)).toEqual(["fake"]);
    expect(typeof daemon.cp.subscribe).toBe("function");
  } finally { daemon.stop(); }
});

test("buildDaemon accepts injected telemetry and builds/starts", async () => {
  const root = mkdtempSync(join(tmpdir(), "hr-daemon-tel-"));
  const dbPath = join(root, "db.sqlite");
  const s = new SettingsStore(openDb(dbPath));
  s.set("workdir_root", root); s.set("enabled_gateways", "fake"); s.set("enabled_runtimes", "");
  const gw: GatewayDescriptor = { id: "fake", label: "Fake", description: "", kind: "gateway", fields: [], build: () => new FakeGateway() };
  const telemetry = {
    startSpan: () => ({ setAttribute() {}, setError() {}, end() {} }),
    count: () => {},
    record: () => {},
  };
  const daemon = buildDaemon({ dbPath, catalog: makeCatalog([gw], []), telemetry });
  try {
    await daemon.start();
    expect(typeof daemon.stop).toBe("function");
  } finally { daemon.stop(); }
});
