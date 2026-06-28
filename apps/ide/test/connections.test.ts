import { test, expect } from "bun:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { rmSync, existsSync } from "node:fs";
import { ConnectionsStore } from "../src/main/connections";

function freshFile() {
  return join(tmpdir(), `conns-${crypto.randomUUID()}.json`);
}

test("add/list/remove + persistence round-trip", () => {
  const f = freshFile();
  const s = new ConnectionsStore(f);
  const p = s.add({
    label: "Cloud",
    baseUrl: "https://r.example.com",
    authMode: "oidc",
    oidc: { issuer: "https://idp", clientId: "c", scopes: "openid" },
  });
  expect(s.list().map((x) => x.id)).toEqual([p.id]);
  expect(existsSync(f)).toBe(true);
  const reloaded = new ConnectionsStore(f);
  expect(reloaded.list()[0]?.label).toBe("Cloud");
  reloaded.remove(p.id);
  expect(reloaded.list()).toEqual([]);
  rmSync(f, { force: true });
});

test("synthetic local is summarized first and not persisted", () => {
  const f = freshFile();
  const s = new ConnectionsStore(f);
  s.setLocal({ url: "http://127.0.0.1:8787" });
  s.add({
    label: "Cloud",
    baseUrl: "https://r",
    authMode: "oidc",
    oidc: { issuer: "https://idp", clientId: "c", scopes: "openid" },
  });
  const sums = s.summaries(() => false);
  expect(sums[0]?.id).toBe("local");
  expect(sums[0]?.authMode).toBe("loopback");
  expect(sums.length).toBe(2);
  expect(new ConnectionsStore(f).list().some((p) => p.id === "local")).toBe(false);
  rmSync(f, { force: true });
});

test("setActive/getActiveId persists", () => {
  const f = freshFile();
  const s = new ConnectionsStore(f);
  const p = s.add({
    label: "C",
    baseUrl: "https://r",
    authMode: "loopback",
  });
  s.setActive(p.id);
  expect(new ConnectionsStore(f).getActiveId()).toBe(p.id);
  rmSync(f, { force: true });
});
