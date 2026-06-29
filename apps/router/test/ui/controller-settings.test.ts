import { test, expect } from "bun:test";
import { AppController } from "../../src/cli/ui/controller";
import { detectClaude, detectGit } from "@harness/core";

function make() {
  return new AppController({ dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit } });
}

test("set persists and get reads back; invalid throws", () => {
  const c = make();
  c.set("default_effort", "high");
  expect(c.get("default_effort")).toBe("high");
  expect(() => c.set("default_perm_mode", "bogus")).toThrow();
});

test("isConfigured false until enabled providers' required + global set", () => {
  const c = make(); // migration seeds enabled_gateways=discord, enabled_runtimes=claude-code
  expect(c.isConfigured()).toBe(false);
  expect(c.missingRequired()).toContain("discord.token");
  for (const [k, v] of [
    ["discord.token", "t"],
    ["discord.app_id", "a"],
    ["discord.guild_id", "g"],
    ["workdir_root", "/repos"],
  ] as const)
    c.set(k, v);
  expect(c.isConfigured()).toBe(true);
  expect(c.missingRequired()).toEqual([]);
});

test("settingKeys + isSecret reflect composed schema", () => {
  const c = make();
  expect(c.settingKeys()).toContain("discord.token");
  expect(c.isSecret("discord.token")).toBe(true);
  expect(c.isSecret("workdir_root")).toBe(false);
});

test("checkEnv reports tool detection via injected detectors", async () => {
  const c = new AppController({
    dbPath: ":memory:",
    detect: { claude: async () => ({ found: true, version: "2.1.0" }), git: async () => ({ found: true, version: "2.45.0" }) },
  });
  const env = await c.checkEnv();
  expect(env.git.found).toBe(true);
  expect(env.claude.found).toBe(true);
});
