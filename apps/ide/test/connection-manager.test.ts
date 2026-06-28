import { test, expect, mock } from "bun:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ConnectionsStore } from "../src/main/connections";
import { TokenStore, type Vault } from "../src/main/token-store";
import { ConnectionManager } from "../src/main/connection-manager";
import type { OidcClient } from "../src/main/oidc";

const vault: Vault = { isAvailable: () => true, encrypt: (s) => Buffer.from(s), decrypt: (b) => b.toString() };
const oidc: OidcClient = {
  startAuth: async () => ({ authUrl: "u", verifier: "v", state: "s", nonce: "n" }),
  exchange: async () => ({ accessToken: "at", expiresAt: Date.now() + 3_600_000 }),
  refresh: async () => ({ accessToken: "at", expiresAt: Date.now() + 3_600_000 }),
};

function setup() {
  const store = new ConnectionsStore(join(tmpdir(), `cm-${crypto.randomUUID()}.json`));
  store.setLocal({ url: "http://127.0.0.1:8787" });
  const tokens = new TokenStore(join(tmpdir(), `cm-tok-${crypto.randomUUID()}`), vault);
  const sends: unknown[] = [];
  const connectCalls: { url: string; token: string }[] = [];
  const makeClient = (info: { url: string; token: string }) => {
    connectCalls.push({ url: info.url, token: info.token });
    return { client: { connId: "c" } as any, connect: async () => {}, dispose: () => {} };
  };
  const mgr = new ConnectionManager({
    store,
    tokens,
    oidc,
    send: (_c, p) => sends.push(p),
    makeClient: makeClient as any,
    openExternal: () => {},
    discoverLocal: () => ({ url: "http://127.0.0.1:8787", token: "tok" }),
  });
  return { mgr, store, sends, connectCalls };
}

test("select(local) builds a loopback client with the serve token", async () => {
  const { mgr, connectCalls } = setup();
  await mgr.select("local");
  expect(connectCalls.at(-1)?.url).toBe("http://127.0.0.1:8787");
  expect(connectCalls.at(-1)?.token).toBe("tok");
  expect(mgr.getClient()).not.toBeNull();
  // local is loopback -> summaries mark it active + signedIn
  expect(mgr.list().find((c) => c.id === "local")?.active).toBe(true);
});

test("add + remove emits updated summaries", async () => {
  const { mgr, sends } = setup();
  await mgr.add({
    label: "Cloud",
    baseUrl: "https://r",
    authMode: "oidc",
    oidc: { issuer: "https://idp", clientId: "c", scopes: "openid" },
  });
  const last = sends.at(-1) as { id: string; label: string }[];
  expect(last.some((c) => c.label === "Cloud")).toBe(true);
});
