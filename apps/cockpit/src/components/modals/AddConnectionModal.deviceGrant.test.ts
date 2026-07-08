import { expect, test } from "bun:test";
import { usesDeviceSignin } from "./deviceSignin";
import type { CatalogEntry } from "@/bindings";

const base: CatalogEntry = {
  id: "x",
  name: "X",
  family: "x",
  color: "#000",
  initial: "X",
  category: "api_key",
  format: "openai",
  requiresBaseUrl: false,
  models: [],
  freeTier: false,
  riskNotice: false,
  usesDeviceGrant: false,
};

test("kiro (device category) uses device sign-in", () => {
  expect(usesDeviceSignin({ ...base, id: "kiro", category: "device" })).toBe(true);
});

test("device-grant oauth provider uses device sign-in", () => {
  expect(usesDeviceSignin({ ...base, id: "qwen", category: "oauth", usesDeviceGrant: true })).toBe(true);
});

test("redirect oauth provider does NOT use device sign-in", () => {
  expect(usesDeviceSignin({ ...base, id: "anthropic-oauth", category: "oauth" })).toBe(false);
});

test("api_key provider does not use device sign-in", () => {
  expect(usesDeviceSignin(base)).toBe(false);
});
