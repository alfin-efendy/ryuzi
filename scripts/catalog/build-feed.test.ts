import { afterEach, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { exportPrivateKeySeedBase64, exportPublicKeyRaw, generateKeyPair, verifyBytes } from "./ed25519.ts";
import {
  buildFeedObject,
  DEFAULT_CATALOG_DIR,
  readBlocklist,
  readCatalogEntries,
  readSequence,
  serializeFeed,
  signFeedBytes,
  writeSequence,
} from "./build-feed.ts";

const tempDirs: string[] = [];
async function tempDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "ryuzi-catalog-feed-test-"));
  tempDirs.push(dir);
  return dir;
}

afterEach(async () => {
  await Promise.all(tempDirs.splice(0).map((d) => rm(d, { recursive: true, force: true })));
});

// The core requirement: a feed signed with build-feed.ts's signing path must
// verify against the SAME keypair's public key, over the EXACT bytes that
// would be written to catalog.json (byte-exactness — see serializeFeed's
// doc). Also proves a single tampered byte breaks verification.
test("round trip: sign a fixture feed, verify with the matching public key", async () => {
  const keyPair = await generateKeyPair();
  const publicKeyRaw = await exportPublicKeyRaw(keyPair.publicKey);
  const privateKeySeedBase64 = await exportPrivateKeySeedBase64(keyPair.privateKey);

  const feed = buildFeedObject({
    entries: [{ id: "acme", manifestToml: 'contract=1\nid="acme"\nname="Acme"\nversion="1.0.0"' }],
    blocked: [{ id: "evil", reason: "revoked" }],
    sequence: 7,
    generatedAt: 1234567890,
  });
  const feedBytes = serializeFeed(feed);
  const signature = await signFeedBytes(feedBytes, privateKeySeedBase64);

  expect(signature.length).toBe(64);
  expect(await verifyBytes(feedBytes, signature, publicKeyRaw)).toBe(true);

  // A single flipped byte anywhere in the signed payload must break
  // verification — this is what protects the engine from a tampered feed.
  const tampered = new Uint8Array(feedBytes);
  tampered[0]! ^= 0xff;
  expect(await verifyBytes(tampered, signature, publicKeyRaw)).toBe(false);

  // A signature made with an unrelated keypair must not verify either.
  const otherKeyPair = await generateKeyPair();
  const otherPublicKeyRaw = await exportPublicKeyRaw(otherKeyPair.publicKey);
  expect(await verifyBytes(feedBytes, signature, otherPublicKeyRaw)).toBe(false);
});

test("serializeFeed emits the camelCase field names the engine's CatalogFeed deserializer expects", async () => {
  const feed = buildFeedObject({
    entries: [{ id: "acme", manifestToml: "contract=1" }],
    blocked: [{ id: "evil", reason: "revoked", sinceSequence: 3 }],
    sequence: 9,
    generatedAt: 42,
  });
  const bytes = serializeFeed(feed);
  const parsed = JSON.parse(new TextDecoder().decode(bytes));
  expect(parsed).toEqual({
    schemaVersion: 1,
    sequence: 9,
    generatedAt: 42,
    entries: [{ id: "acme", manifestToml: "contract=1" }],
    blocked: [{ id: "evil", reason: "revoked", sinceSequence: 3 }],
  });
});

test("buildFeedObject defaults a blocked entry's sinceSequence to the feed's own sequence when omitted", () => {
  const feed = buildFeedObject({
    entries: [],
    blocked: [{ id: "evil", reason: "revoked" }],
    sequence: 5,
    generatedAt: 0,
  });
  expect(feed.blocked).toEqual([{ id: "evil", reason: "revoked", sinceSequence: 5 }]);
});

// Exercises the real embedded catalog directory end to end — the same
// directory the release workflow points build-feed.ts at — so a broken
// manifest or an id/filename drift is caught by `bun test`, not only at
// release time.
test("readCatalogEntries parses every real embedded catalog manifest with a matching id", async () => {
  const entries = await readCatalogEntries(DEFAULT_CATALOG_DIR);
  expect(entries.length).toBeGreaterThanOrEqual(24);
  const ids = entries.map((e) => e.id);
  expect(new Set(ids).size).toBe(ids.length); // no duplicates
  expect(ids).toContain("github");
  for (const entry of entries) {
    const parsed = Bun.TOML.parse(entry.manifestToml) as { id: string };
    expect(parsed.id).toBe(entry.id);
  }
});

test("readCatalogEntries throws on a manifest with no id", async () => {
  const dir = await tempDir();
  await Bun.write(join(dir, "bad.toml"), 'name="Bad"\nversion="1.0.0"');
  await expect(readCatalogEntries(dir)).rejects.toThrow(/no \(or a blank\) top-level 'id'/);
});

test("readCatalogEntries throws on two manifests declaring the same id", async () => {
  const dir = await tempDir();
  await Bun.write(join(dir, "a.toml"), 'id="dup"\nname="A"');
  await Bun.write(join(dir, "b.toml"), 'id="dup"\nname="B"');
  await expect(readCatalogEntries(dir)).rejects.toThrow(/duplicate catalog id "dup"/);
});

test("readBlocklist returns [] when the file doesn't exist", async () => {
  const dir = await tempDir();
  expect(await readBlocklist(join(dir, "missing.json"))).toEqual([]);
});

test("readBlocklist parses a valid blocklist file", async () => {
  const dir = await tempDir();
  const path = join(dir, "blocklist.json");
  await Bun.write(path, JSON.stringify([{ id: "evil", reason: "revoked", sinceSequence: 3 }]));
  expect(await readBlocklist(path)).toEqual([{ id: "evil", reason: "revoked", sinceSequence: 3 }]);
});

test("readBlocklist rejects an entry missing a required field", async () => {
  const dir = await tempDir();
  const path = join(dir, "blocklist.json");
  await Bun.write(path, JSON.stringify([{ id: "evil" }]));
  await expect(readBlocklist(path)).rejects.toThrow(/expected \{"id"/);
});

test("readSequence/writeSequence round trip, defaulting to 0 when the file is missing", async () => {
  const dir = await tempDir();
  const path = join(dir, "sequence.txt");
  expect(await readSequence(path)).toBe(0);
  await writeSequence(path, 5);
  expect(await readSequence(path)).toBe(5);
  await writeSequence(path, 6);
  expect(await readSequence(path)).toBe(6);
});
