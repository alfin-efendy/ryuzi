import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { migrate } from "../../src/store/db";
import { SettingsStore } from "../../src/config/store";
import { makeCatalog } from "../../src/providers/catalog";
import type { GatewayDescriptor, RuntimeDescriptor } from "../../src/providers/types";
import { missingRequiredSettings, isConfigured, csv } from "../../src/config/required";

const gw: GatewayDescriptor = {
  id: "g",
  label: "G",
  description: "",
  kind: "gateway",
  fields: [{ key: "g.tok", label: "Tok", help: "h", required: true, secret: true }],
  build: () => ({}) as any,
};
const rt: RuntimeDescriptor = {
  id: "r",
  label: "R",
  description: "",
  kind: "runtime",
  fields: [],
  detect: async () => ({ found: true }),
  build: () => ({}) as any,
};
const cat = makeCatalog([gw], [rt]);

function setup() {
  const db = new Database(":memory:");
  migrate(db);
  const raw = (k: string, v: string) =>
    db.run("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [k, v]);
  return { s: new SettingsStore(db), raw };
}

test("csv splits and trims", () => {
  expect(csv("a, b ,,c")).toEqual(["a", "b", "c"]);
  expect(csv(undefined)).toEqual([]);
});

test("missingRequired counts only enabled providers + global", () => {
  const { s, raw } = setup();
  raw("enabled_gateways", "");
  raw("enabled_runtimes", "r");
  // g not enabled -> g.tok not required; workdir_root (global required) missing
  expect(missingRequiredSettings(s, cat)).toEqual(["workdir_root"]);
  raw("enabled_gateways", "g");
  expect(missingRequiredSettings(s, cat).sort()).toEqual(["g.tok", "workdir_root"]);
  expect(isConfigured(s, cat)).toBe(false);
  raw("g.tok", "x");
  raw("workdir_root", "/r");
  expect(isConfigured(s, cat)).toBe(true);
});
