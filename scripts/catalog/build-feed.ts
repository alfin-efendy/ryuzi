// Usage: bun scripts/catalog/build-feed.ts
//
// Builds and ed25519-signs the remote plugin catalog feed (`catalog.json` +
// `catalog.json.sig`) the engine's `RemoteCatalogManager` fetches and
// verifies against `CATALOG_FEED_PUBKEY`
// (crates/core/src/plugins/catalog_feed_key.rs, crates/core/src/plugins/remote_catalog.rs).
//
// Reads every `crates/core/plugins/catalog/*.toml` manifest plus the
// optional `scripts/catalog/blocklist.json` denylist, assembles the feed,
// signs it with the `CATALOG_FEED_PRIVATE_KEY` env var (base64, the seed
// `scripts/catalog/keygen.ts` prints), and writes `catalog.json` +
// `catalog.json.sig` (a raw 64-byte detached signature, NOT base64) to the
// current directory — run this from the repo root, or override the output
// paths below.
//
// Byte-exactness matters: the engine verifies the signature over the EXACT
// bytes of the downloaded catalog.json. `buildFeedBytes` below serializes
// the feed exactly once; `main` signs and writes that same `Uint8Array` —
// never re-serializes between signing and writing.
import { importSigningKeyFromSeedBase64, signBytes } from "./ed25519.ts";

export const DEFAULT_CATALOG_DIR = "crates/core/plugins/catalog";
export const DEFAULT_BLOCKLIST_PATH = "scripts/catalog/blocklist.json";
export const DEFAULT_SEQUENCE_PATH = "scripts/catalog/sequence.txt";
export const DEFAULT_OUT_JSON_PATH = "catalog.json";
export const DEFAULT_OUT_SIG_PATH = "catalog.json.sig";

/** The schema version every shipped feed declares; must match `CatalogFeed::schema_version`'s accepted value (`remote_catalog.rs`, currently `1`). */
export const SCHEMA_VERSION = 1;

export interface CatalogFeedEntry {
  id: string;
  manifestToml: string;
}

export interface CatalogBlockedEntry {
  id: string;
  reason: string;
  sinceSequence: number;
}

export interface CatalogFeed {
  schemaVersion: number;
  sequence: number;
  generatedAt: number;
  entries: CatalogFeedEntry[];
  blocked: CatalogBlockedEntry[];
}

/** The shape of an entry in the optional `scripts/catalog/blocklist.json`: `sinceSequence` is optional (defaults to the sequence being built). */
interface RawBlocklistEntry {
  id: string;
  reason: string;
  sinceSequence?: number;
}

/**
 * Parses every `*.toml` manifest in `catalogDir` into a feed entry, deriving
 * `id` from the manifest's OWN `id` field (never the filename) — this is
 * what guarantees the feed invariant `merged_catalog_plugins` enforces
 * (`crates/core/src/plugins/catalog.rs`): an entry's declared feed id must
 * equal its manifest's `id`, or the engine skips it and logs a warning
 * rather than risk overwriting the wrong embedded slot. Files are read in
 * sorted-filename order for a deterministic feed across builds. Throws on a
 * missing/blank `id`, on unparsable TOML, or on two files declaring the same
 * `id` — all build-time bugs, not run-time conditions to warn-and-skip.
 */
export async function readCatalogEntries(catalogDir: string): Promise<CatalogFeedEntry[]> {
  const glob = new Bun.Glob("*.toml");
  const files = (await Array.fromAsync(glob.scan(catalogDir))).sort();
  const entries: CatalogFeedEntry[] = [];
  const seenIds = new Map<string, string>();
  for (const file of files) {
    const path = `${catalogDir}/${file}`;
    const manifestToml = await Bun.file(path).text();
    let parsed: Record<string, unknown>;
    try {
      parsed = Bun.TOML.parse(manifestToml) as Record<string, unknown>;
    } catch (e) {
      throw new Error(`${path}: failed to parse TOML: ${e instanceof Error ? e.message : e}`);
    }
    const id = parsed.id;
    if (typeof id !== "string" || id.length === 0) {
      throw new Error(`${path}: manifest has no (or a blank) top-level 'id'`);
    }
    const existing = seenIds.get(id);
    if (existing) {
      throw new Error(`duplicate catalog id ${JSON.stringify(id)}: ${existing} and ${path}`);
    }
    seenIds.set(id, path);
    entries.push({ id, manifestToml });
  }
  return entries;
}

/** Reads the optional blocklist file; `[]` if it doesn't exist. Throws on a file that exists but isn't a JSON array of `{id, reason}` objects. */
export async function readBlocklist(blocklistPath: string): Promise<RawBlocklistEntry[]> {
  const file = Bun.file(blocklistPath);
  if (!(await file.exists())) return [];
  let parsed: unknown;
  try {
    parsed = await file.json();
  } catch (e) {
    throw new Error(`${blocklistPath}: invalid JSON: ${e instanceof Error ? e.message : e}`);
  }
  if (!Array.isArray(parsed)) {
    throw new Error(`${blocklistPath}: expected a JSON array of {id, reason} entries`);
  }
  return parsed.map((entry, i) => {
    if (
      typeof entry !== "object" ||
      entry === null ||
      typeof (entry as { id?: unknown }).id !== "string" ||
      (entry as { id: string }).id.length === 0 ||
      typeof (entry as { reason?: unknown }).reason !== "string"
    ) {
      throw new Error(`${blocklistPath}[${i}]: expected {"id": string, "reason": string, "sinceSequence"?: number}`);
    }
    const e = entry as RawBlocklistEntry;
    return { id: e.id, reason: e.reason, sinceSequence: e.sinceSequence };
  });
}

/** The currently-recorded sequence (0 if `sequencePath` doesn't exist yet — the next build's sequence is one more than this). */
export async function readSequence(sequencePath: string): Promise<number> {
  const file = Bun.file(sequencePath);
  if (!(await file.exists())) return 0;
  const text = (await file.text()).trim();
  if (text === "") return 0;
  const n = Number(text);
  if (!Number.isInteger(n) || n < 0) {
    throw new Error(`${sequencePath}: expected a non-negative integer, got ${JSON.stringify(text)}`);
  }
  return n;
}

/** Persists the sequence this build used, so the next run continues from it. */
export async function writeSequence(sequencePath: string, sequence: number): Promise<void> {
  await Bun.write(sequencePath, `${sequence}\n`);
}

/** Assembles the `CatalogFeed` object — pure, no I/O. `entries`/`blocked` are used as given (already validated/defaulted by the caller). */
export function buildFeedObject(args: {
  entries: CatalogFeedEntry[];
  blocked: RawBlocklistEntry[];
  sequence: number;
  generatedAt: number;
}): CatalogFeed {
  return {
    schemaVersion: SCHEMA_VERSION,
    sequence: args.sequence,
    generatedAt: args.generatedAt,
    entries: args.entries,
    blocked: args.blocked.map((b) => ({
      id: b.id,
      reason: b.reason,
      sinceSequence: b.sinceSequence ?? args.sequence,
    })),
  };
}

/**
 * Serializes a `CatalogFeed` to its exact on-wire bytes — call this ONCE per
 * build. The result must be both the bytes handed to `signFeedBytes` and the
 * bytes written to `catalog.json`; re-serializing (even to byte-identical
 * JSON) after signing would defeat the purpose of a detached signature if
 * the two ever drifted, so every caller in this file threads the same
 * `Uint8Array` through both steps instead of calling this twice.
 */
export function serializeFeed(feed: CatalogFeed): Uint8Array {
  return new TextEncoder().encode(`${JSON.stringify(feed, null, 2)}\n`);
}

/** Signs the exact serialized feed bytes with the base64-seed private key (`CATALOG_FEED_PRIVATE_KEY` / `keygen.ts`'s output). Returns the raw 64-byte detached signature. */
export async function signFeedBytes(feedBytes: Uint8Array, privateKeySeedBase64: string): Promise<Uint8Array> {
  const signingKey = await importSigningKeyFromSeedBase64(privateKeySeedBase64);
  return signBytes(feedBytes, signingKey);
}

async function main() {
  const privateKeySeedBase64 = process.env.CATALOG_FEED_PRIVATE_KEY;
  if (!privateKeySeedBase64) {
    console.error(
      "CATALOG_FEED_PRIVATE_KEY is not set. Generate a keypair with `bun scripts/catalog/keygen.ts`, " +
        "store the private key as this env var (or the CI secret of the same name), and re-run.",
    );
    process.exit(1);
  }

  const catalogDir = process.env.CATALOG_DIR ?? DEFAULT_CATALOG_DIR;
  const blocklistPath = process.env.CATALOG_BLOCKLIST_PATH ?? DEFAULT_BLOCKLIST_PATH;
  const sequencePath = process.env.CATALOG_SEQUENCE_PATH ?? DEFAULT_SEQUENCE_PATH;
  const outJsonPath = process.env.CATALOG_OUT_JSON_PATH ?? DEFAULT_OUT_JSON_PATH;
  const outSigPath = process.env.CATALOG_OUT_SIG_PATH ?? DEFAULT_OUT_SIG_PATH;

  const entries = await readCatalogEntries(catalogDir);
  const blocked = await readBlocklist(blocklistPath);
  const sequence = (await readSequence(sequencePath)) + 1;

  const feed = buildFeedObject({ entries, blocked, sequence, generatedAt: Date.now() });
  const feedBytes = serializeFeed(feed);
  const signature = await signFeedBytes(feedBytes, privateKeySeedBase64);

  await Bun.write(outJsonPath, feedBytes);
  await Bun.write(outSigPath, signature);
  await writeSequence(sequencePath, sequence);

  console.log(`wrote ${outJsonPath} + ${outSigPath} — sequence ${sequence}, ${entries.length} entries, ${feed.blocked.length} blocked`);
}

if (import.meta.main) {
  await main();
}
