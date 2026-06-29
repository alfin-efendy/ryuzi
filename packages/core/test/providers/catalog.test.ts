import { test, expect } from "bun:test";
import { makeCatalog, catalog } from "../../src/providers/catalog";
import type { GatewayDescriptor, RuntimeDescriptor } from "../../src/providers/types";

const gw: GatewayDescriptor = { id: "g1", label: "G1", description: "d", kind: "gateway", fields: [], build: () => ({}) as any };
const rt: RuntimeDescriptor = {
  id: "r1",
  label: "R1",
  description: "d",
  kind: "runtime",
  fields: [],
  detect: async () => ({ found: false }),
  build: () => ({}) as any,
};

test("makeCatalog exposes arrays and id lookups", () => {
  const cat = makeCatalog([gw], [rt]);
  expect(cat.gateways).toEqual([gw]);
  expect(cat.runtimes).toEqual([rt]);
  expect(cat.gateway("g1")).toBe(gw);
  expect(cat.runtime("r1")).toBe(rt);
  expect(cat.gateway("nope")).toBeUndefined();
});

test("default catalog contains discord + claude-code", () => {
  expect(catalog.gateway("discord")?.label).toBe("Discord");
  expect(catalog.runtime("claude-code")?.label).toBe("Claude Code");
});
