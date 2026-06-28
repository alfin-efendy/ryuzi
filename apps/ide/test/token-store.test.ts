import { test, expect } from "bun:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { rmSync } from "node:fs";
import { TokenStore, type Vault, type TokenSet } from "../src/main/token-store";

// In-memory reversible "encryption" stand-in for safeStorage.
const fakeVault = (available = true): Vault => ({
  isAvailable: () => available,
  encrypt: (s) => Buffer.from(s, "utf8"),
  decrypt: (b) => b.toString("utf8"),
});
const dir = () => join(tmpdir(), `tok-${crypto.randomUUID()}`);
const set = (over: Partial<TokenSet> = {}): TokenSet => ({
  accessToken: "at",
  refreshToken: "rt",
  expiresAt: Date.now() + 3_600_000,
  ...over,
});

test("save/load round-trip persists when vault available", () => {
  const d = dir();
  const ts = new TokenStore(d, fakeVault(true));
  ts.save("p1", set());
  expect(new TokenStore(d, fakeVault(true)).load("p1")?.accessToken).toBe("at"); // reload from disk
  rmSync(d, { recursive: true, force: true });
});

test("no keyring -> in-memory only, nothing written to disk", () => {
  const d = dir();
  const ts = new TokenStore(d, fakeVault(false));
  ts.save("p1", set());
  expect(ts.load("p1")?.accessToken).toBe("at"); // in-memory works this session
  expect(new TokenStore(d, fakeVault(false)).load("p1")).toBeNull(); // not persisted
});

test("getAccessToken refreshes near-expiry and re-stores", async () => {
  const ts = new TokenStore(dir(), fakeVault(true));
  ts.save("p1", set({ expiresAt: Date.now() + 10_000 })); // <60s -> refresh
  const refresh = async (rt: string) =>
    ({
      accessToken: "at2",
      refreshToken: rt,
      expiresAt: Date.now() + 3_600_000,
    }) as TokenSet;
  expect(await ts.getAccessToken("p1", refresh)).toBe("at2");
  expect(ts.load("p1")?.accessToken).toBe("at2");
});

test("getAccessToken returns null and clears on refresh failure", async () => {
  const ts = new TokenStore(dir(), fakeVault(true));
  ts.save("p1", set({ expiresAt: Date.now() + 10_000 }));
  const refresh = async () => {
    throw new Error("expired");
  };
  expect(await ts.getAccessToken("p1", refresh)).toBeNull();
  expect(ts.has("p1")).toBe(false);
});
