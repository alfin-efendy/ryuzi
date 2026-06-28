import { test, expect } from "bun:test";
import { openDb } from "../../src/store/db";
import { SettingsStore } from "../../src/config/store";
import { validateSetting } from "../../src/config/schema";

test("serve + oidc keys validate and apply defaults", () => {
  const db = openDb(":memory:");
  const settings = new SettingsStore(db);
  expect(settings.get("serve.host")).toBe("127.0.0.1");
  expect(settings.get("serve.port")).toBe("8787");
  expect(settings.get("serve.auth_mode")).toBe("loopback");
  settings.set("serve.enabled", "true");
  expect(settings.get("serve.enabled")).toBe("true");
});

test("serve.auth_mode rejects unknown values; serve.port must be int", () => {
  expect(validateSetting("serve.auth_mode", "loopback")).toBeNull();
  expect(validateSetting("serve.auth_mode", "oauth2")).toContain("one of");
  expect(validateSetting("serve.port", "notanint")).toContain("integer");
  expect(validateSetting("oidc.issuer", "https://idp.example.com")).toBeNull();
});
