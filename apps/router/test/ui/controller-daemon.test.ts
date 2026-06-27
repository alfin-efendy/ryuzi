import { test, expect } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AppController } from "../../src/cli/ui/controller";
import { detectClaude, detectGit } from "../../src/harness/detect";
import { makeCatalog } from "../../src/providers/catalog";
import type { GatewayDescriptor, RuntimeDescriptor } from "../../src/providers/types";
import type { Gateway } from "../../src/gateways/types";
import type { Harness, HarnessEvent, HarnessRunInput } from "../../src/harness/types";

class FakeGateway implements Gateway {
  readonly id = "fake";
  constructor(private onStart: () => Promise<void> = async () => {}) {}
  start() { return this.onStart(); }
  async createWorkspace() { return "w"; }
  async createConversation() { return "c"; }
  async postStatus() { return { surface: { gateway: "fake", conversationId: "c" }, messageId: "m" }; }
  async editStatus() {}
  async postResult() {}
  async postError() {}
  async requestApproval() { return { decision: "deny" as const, actor: "x" }; }
}
class FakeHarness implements Harness { readonly id = "fake-rt"; async *run(_i: HarnessRunInput): AsyncIterable<HarnessEvent> { yield { type: "result", usage: {} }; } }

function fakeCatalog(onStart?: () => Promise<void>) {
  const gw: GatewayDescriptor = { id: "fake", label: "Fake", description: "", kind: "gateway", fields: [], build: () => new FakeGateway(onStart) };
  const rt: RuntimeDescriptor = { id: "fake-rt", label: "Fake RT", description: "", kind: "runtime", fields: [], detect: async () => ({ found: true }), build: () => new FakeHarness() };
  return makeCatalog([gw], [rt]);
}

function configured(onStart?: () => Promise<void>) {
  const root = mkdtempSync(join(tmpdir(), "hr-prov-"));
  const c = new AppController({ dbPath: join(root, "db.sqlite"), detect: { claude: detectClaude, git: detectGit }, catalog: fakeCatalog(onStart) });
  c.setEnabledGateways(["fake"]); c.setEnabledRuntimes(["fake-rt"]); c.setDefaultRuntime("fake-rt");
  c.set("workdir_root", root);
  return c;
}

test("startDaemon runs, stopDaemon stops, change events fire", async () => {
  const c = configured();
  let changes = 0; c.on("change", () => { changes++; });
  await c.startDaemon();
  expect(c.daemon().running).toBe(true);
  expect(c.daemon().startedAt).toBeGreaterThan(0);
  c.stopDaemon();
  expect(c.daemon().running).toBe(false);
  expect(changes).toBeGreaterThan(0);
});

test("startDaemon records lastError when nothing enabled / settings missing", async () => {
  const root = mkdtempSync(join(tmpdir(), "hr-prov2-"));
  const c = new AppController({ dbPath: join(root, "db.sqlite"), detect: { claude: detectClaude, git: detectGit }, catalog: fakeCatalog() });
  c.setEnabledGateways([]); // disable
  await c.startDaemon();
  expect(c.daemon().running).toBe(false);
  expect(c.daemon().lastError).toBeDefined();
});

test("startDaemon exposes a transient connecting state", async () => {
  let release = () => {};
  const gate = new Promise<void>((r) => { release = r; });
  const c = configured(() => gate);
  const p = c.startDaemon();
  await new Promise((r) => setTimeout(r, 10));
  expect(c.daemon().starting).toBe(true);
  release(); await p;
  expect(c.daemon().running).toBe(true);
  c.stopDaemon();
});
