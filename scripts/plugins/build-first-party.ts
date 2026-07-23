// Usage:
//   bun scripts/plugins/build-first-party.ts            # build + sign every component
//   bun scripts/plugins/build-first-party.ts mimo       # build + sign one component
//   bun scripts/plugins/build-first-party.ts keygen      # generate a dev signing keypair
//
// Reproducibly builds the first-party provider *components* (plugins/mimo,
// plugins/opencode), then emits — per component — the four release artifacts the
// engine's signed install pipeline fetches (crates/core/src/plugins/remote_catalog.rs
// `install_component_release`):
//
//   <id>.ryuzi-plugin.toml    (the committed manifest, verbatim)
//   <id>.wasm                 (the compiled component, `component_url` points here)
//   <id>.release.json         (a `ryuzi_plugin_sdk::PluginRelease` descriptor)
//   <id>.release.json.sig     (the `plugin.sig` envelope: JSON {key_id, signature})
//
// The signature is an ed25519 signature over `release.json`'s EXACT raw bytes,
// base64url-no-pad, wrapped in the JSON envelope `plugins::bundle::verify_bundle`
// expects: {"key_id":"first-party","signature":"<b64url>"}. NOTE the encoding
// differs from the catalog feed (`scripts/catalog/build-feed.ts` writes a RAW
// 64-byte detached .sig; here the .sig is a JSON envelope with the signature
// base64url-encoded inside it).
//
// The signing seed comes from the `FIRST_PARTY_PRIVATE_KEY` env var (base64,
// exactly what `keygen` prints) — the private key is NEVER read from or written
// to a committed file. The matching PUBLIC key is pasted into
// crates/core/src/plugins/first_party_key.rs by a human at rollout; this script
// never touches that file.
import { readdir, rm } from "node:fs/promises";
import {
  exportPrivateKeySeedBase64,
  exportPublicKeyRaw,
  generateKeyPair,
  importSigningKeyFromSeedBase64,
  signBytes,
  toRustByteArrayLiteral,
} from "../catalog/ed25519.ts";

/** The `key_id` every first-party `plugin.sig` names — MUST match `first_party_key::FIRST_PARTY_KEY_ID`. */
export const FIRST_PARTY_KEY_ID = "first-party";

/**
 * The concrete WIT contract version each component is built against. Unlike the
 * manifest's `wit-api` RANGE (`>=0.1.0, <0.2.0`), a `PluginRelease.wit-api` is a
 * single semver (`plugins::bundle::PluginRelease::validate`). Bump this when the
 * shipped `ryuzi:*` WIT contracts move to a new concrete version.
 */
export const WIT_API_VERSION = "0.1.0";

/**
 * Base URL the four release artifacts are published under. `component_url` is
 * built as `<base>/<id>.wasm`; the installer's `require_same_origin` check
 * requires the wasm URL to share scheme+host+port with this base, and the 11a
 * default (`DEFAULT_COMPONENT_RELEASE_BASE_URL`) is this same GitHub host, so a
 * same-host asset URL always passes. Override with `FIRST_PARTY_RELEASE_BASE_URL`.
 */
export const DEFAULT_RELEASE_BASE_URL = "https://github.com/alfin-efendy/ryuzi/releases/latest/download";

/** The SDK WIT source the components' `wit/deps/` is materialized from (mirrors `crates/core/tests/fixtures/build-components.sh`). */
const SDK_WIT_DIR = "crates/plugin-sdk/wit";
const WASM_TARGET = "wasm32-wasip2";

/** One first-party component to build + sign. `crateWasmStem` is cargo's output name (crate name with `-`→`_`). */
export interface ComponentSpec {
  id: string;
  dir: string;
  crateWasmStem: string;
}

export const COMPONENTS: ComponentSpec[] = [
  { id: "mimo", dir: "plugins/mimo", crateWasmStem: "ryuzi_plugin_mimo" },
  { id: "opencode", dir: "plugins/opencode", crateWasmStem: "ryuzi_plugin_opencode" },
  { id: "openai", dir: "plugins/openai", crateWasmStem: "ryuzi_plugin_openai" },
  { id: "github", dir: "plugins/github", crateWasmStem: "ryuzi_plugin_github" },
  { id: "discord", dir: "plugins/discord", crateWasmStem: "ryuzi_plugin_discord" },
  { id: "atlassian", dir: "plugins/atlassian", crateWasmStem: "ryuzi_plugin_atlassian" },
  { id: "bitbucket", dir: "plugins/bitbucket", crateWasmStem: "ryuzi_plugin_bitbucket" },
];

/** The `PluginRelease` JSON shape (crates/plugin-sdk/src/bundle.rs). `wit-api` is kebab in the wire form. */
export interface PluginReleaseJson {
  id: string;
  version: string;
  "wit-api": string;
  component_url: string;
  component_sha256: string;
  size_bytes?: number;
  published_at?: string;
}

/** Lowercase-hex SHA-256 of `bytes` (matches `plugins::bundle`'s `format!("{:x}", Sha256::digest(..))`). */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as Uint8Array<ArrayBuffer>);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Strip the `world plugin { ... }` block from the SDK's `plugin.wit`, leaving
 * its `interface` definitions (types/lifecycle) — wit-bindgen 0.57 can't parse
 * the production world's named imports, but its interfaces remain the canonical
 * contract. A faithful port of `build-components.sh`'s `awk` filter.
 */
export function stripPluginWorld(pluginWit: string): string {
  const out: string[] = [];
  let depth = 0;
  let skipping = false;
  for (const line of pluginWit.split("\n")) {
    if (!skipping && /^world plugin\s*\{/.test(line)) {
      skipping = true;
      depth = 1;
      continue;
    }
    if (skipping) {
      depth += (line.match(/\{/g)?.length ?? 0) - (line.match(/\}/g)?.length ?? 0);
      if (depth === 0) skipping = false;
      continue;
    }
    out.push(line);
  }
  return out.join("\n");
}

/** Materialize `<dir>/wit/deps/` from the SDK (stripped plugin.wit + every dep interface), mirroring `materialize_deps`. */
export async function materializeDeps(dir: string): Promise<void> {
  const depsDir = `${dir}/wit/deps`;
  await rm(depsDir, { recursive: true, force: true });
  await Bun.write(`${depsDir}/plugin.wit`, stripPluginWorld(await Bun.file(`${SDK_WIT_DIR}/plugin.wit`).text()));
  for (const entry of await readdir(`${SDK_WIT_DIR}/deps`)) {
    if (entry.endsWith(".wit")) {
      await Bun.write(`${depsDir}/${entry}`, await Bun.file(`${SDK_WIT_DIR}/deps/${entry}`).arrayBuffer());
    }
  }
}

/** `cargo build --target wasm32-wasip2 --release` in `dir`; throws on a non-zero exit. */
export function buildComponent(dir: string): void {
  const result = Bun.spawnSync(["cargo", "build", "--target", WASM_TARGET, "--release"], {
    cwd: dir,
    stdout: "inherit",
    stderr: "inherit",
  });
  if (result.exitCode !== 0) {
    throw new Error(`cargo build failed for ${dir} (exit ${result.exitCode})`);
  }
}

/** Minimal fields the signer needs from a component's manifest. */
interface ManifestFields {
  id: string;
  version: string;
  component: string;
}

/** Read + minimally validate `<dir>/ryuzi-plugin.toml`, returning the id/version/component the release descriptor mirrors. */
export async function readManifest(dir: string): Promise<ManifestFields> {
  const path = `${dir}/ryuzi-plugin.toml`;
  const parsed = Bun.TOML.parse(await Bun.file(path).text()) as Record<string, unknown>;
  const id = parsed.id;
  const version = parsed.version;
  const component = parsed.component;
  if (typeof id !== "string" || id.length === 0) throw new Error(`${path}: missing 'id'`);
  if (typeof version !== "string" || version.length === 0) throw new Error(`${path}: missing 'version'`);
  if (typeof component !== "string" || component.length === 0) throw new Error(`${path}: missing 'component'`);
  return { id, version, component };
}

/** Assemble the `PluginRelease` object — pure, no I/O. `published_at` is only set when given (omitted keeps release.json byte-reproducible for a given wasm). */
export function buildReleaseObject(args: {
  id: string;
  version: string;
  componentUrl: string;
  sha256: string;
  sizeBytes: number;
  publishedAt?: string;
}): PluginReleaseJson {
  const release: PluginReleaseJson = {
    id: args.id,
    version: args.version,
    "wit-api": WIT_API_VERSION,
    component_url: args.componentUrl,
    component_sha256: args.sha256,
    size_bytes: args.sizeBytes,
  };
  if (args.publishedAt !== undefined && args.publishedAt !== "") {
    release.published_at = args.publishedAt;
  }
  return release;
}

/**
 * Serialize a release to its EXACT signed-and-published bytes — call ONCE per
 * component. The result is both the bytes signed and the bytes written to
 * `<id>.release.json`; verification is byte-for-byte
 * (`plugins::bundle::verify_bundle`), so these must never diverge.
 */
export function serializeRelease(release: PluginReleaseJson): Uint8Array {
  return new TextEncoder().encode(`${JSON.stringify(release, null, 2)}\n`);
}

/** Base64url without padding (matches Rust's `URL_SAFE_NO_PAD`). */
export function base64UrlNoPad(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}

/** Build the `plugin.sig` envelope over `releaseBytes`: `{"key_id":"first-party","signature":"<b64url ed25519 sig>"}`. */
export async function buildSignatureEnvelope(releaseBytes: Uint8Array, privateKeySeedBase64: string): Promise<string> {
  const signingKey = await importSigningKeyFromSeedBase64(privateKeySeedBase64);
  const signature = await signBytes(releaseBytes, signingKey);
  return `${JSON.stringify({ key_id: FIRST_PARTY_KEY_ID, signature: base64UrlNoPad(signature) }, null, 2)}\n`;
}

/** Build + sign one component, writing its four artifacts into `outDir`. Returns the release descriptor for logging. */
async function processComponent(
  spec: ComponentSpec,
  privateKeySeedBase64: string,
  baseUrl: string,
  outDir: string,
  publishedAt: string | undefined,
): Promise<PluginReleaseJson> {
  const manifest = await readManifest(spec.dir);
  if (manifest.id !== spec.id) {
    throw new Error(`${spec.dir}/ryuzi-plugin.toml declares id ${JSON.stringify(manifest.id)}, expected ${JSON.stringify(spec.id)}`);
  }

  await materializeDeps(spec.dir);
  buildComponent(spec.dir);

  const wasmPath = `${spec.dir}/target/${WASM_TARGET}/release/${spec.crateWasmStem}.wasm`;
  const wasmBytes = new Uint8Array(await Bun.file(wasmPath).arrayBuffer());
  const sha256 = await sha256Hex(wasmBytes);

  const release = buildReleaseObject({
    id: manifest.id,
    version: manifest.version,
    componentUrl: `${baseUrl}/${manifest.component}`,
    sha256,
    sizeBytes: wasmBytes.byteLength,
    publishedAt,
  });
  const releaseBytes = serializeRelease(release);
  const signatureEnvelope = await buildSignatureEnvelope(releaseBytes, privateKeySeedBase64);

  await Bun.write(`${outDir}/${spec.id}.ryuzi-plugin.toml`, await Bun.file(`${spec.dir}/ryuzi-plugin.toml`).arrayBuffer());
  await Bun.write(`${outDir}/${manifest.component}`, wasmBytes);
  await Bun.write(`${outDir}/${spec.id}.release.json`, releaseBytes);
  await Bun.write(`${outDir}/${spec.id}.release.json.sig`, signatureEnvelope);

  return release;
}

/** `keygen` mode: print a dev signing keypair (private base64 seed + Rust pubkey literal). */
async function keygen(): Promise<void> {
  const keyPair = await generateKeyPair();
  const publicKeyRaw = await exportPublicKeyRaw(keyPair.publicKey);
  const privateKeySeedBase64 = await exportPrivateKeySeedBase64(keyPair.privateKey);

  console.log("=== ed25519 keypair for FIRST-PARTY component-bundle signing ===\n");
  console.log("1) Paste into crates/core/src/plugins/first_party_key.rs");
  console.log("   (replaces the all-zero FIRST_PARTY_PUBKEY placeholder — PUBLIC, safe to commit):\n");
  console.log(`    pub const FIRST_PARTY_PUBKEY: [u8; 32] = ${toRustByteArrayLiteral(publicKeyRaw)};\n`);
  console.log("2) Store as the CI secret / local env var FIRST_PARTY_PRIVATE_KEY");
  console.log("   (a gitignored path or the shell env — NEVER commit it):\n");
  console.log(`    ${privateKeySeedBase64}\n`);
  console.log("Run this exactly once per key (or rotation); a second run is an unrelated keypair.");
}

async function main(argv: string[]): Promise<void> {
  if (argv[0] === "keygen") {
    await keygen();
    return;
  }

  const privateKeySeedBase64 = process.env.FIRST_PARTY_PRIVATE_KEY;
  if (!privateKeySeedBase64) {
    console.error(
      "FIRST_PARTY_PRIVATE_KEY is not set. Generate a keypair with " +
        "`bun scripts/plugins/build-first-party.ts keygen`, store the private seed as this env var " +
        "(or the CI secret of the same name), and re-run. Never commit the private key.",
    );
    process.exit(1);
  }

  const baseUrl = (process.env.FIRST_PARTY_RELEASE_BASE_URL ?? DEFAULT_RELEASE_BASE_URL).replace(/\/+$/, "");
  const outDir = process.env.FIRST_PARTY_OUT_DIR ?? "dist/plugins";
  const publishedAt = process.env.FIRST_PARTY_PUBLISHED_AT;

  const requested = argv.filter((a) => !a.startsWith("-"));
  const specs = requested.length > 0 ? COMPONENTS.filter((c) => requested.includes(c.id)) : COMPONENTS;
  if (specs.length === 0) {
    throw new Error(`no matching components for ${JSON.stringify(requested)} (known: ${COMPONENTS.map((c) => c.id).join(", ")})`);
  }

  for (const spec of specs) {
    const release = await processComponent(spec, privateKeySeedBase64, baseUrl, outDir, publishedAt);
    console.log(
      `signed ${spec.id} ${release.version} -> ${outDir}/${spec.id}.{ryuzi-plugin.toml,wasm,release.json,release.json.sig} (sha256 ${release.component_sha256})`,
    );
  }
}

if (import.meta.main) {
  await main(Bun.argv.slice(2));
}
