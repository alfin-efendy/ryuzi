// Shared ed25519 helpers for the remote plugin catalog feed's sign/publish
// tooling (keygen.ts, build-feed.ts, build-feed.test.ts).
//
// Uses Bun's built-in WebCrypto Ed25519 support (`crypto.subtle`, algorithm
// "Ed25519") — no external dependency needed. Cross-checked byte-for-byte
// against `ed25519-dalek` (the crate `crates/core/src/plugins/remote_catalog.rs`
// verifies with): signing the same 32-byte seed + message with Bun's
// WebCrypto and with `ed25519-dalek::SigningKey::sign` produces the identical
// 64-byte signature, so a feed signed here verifies against
// `CATALOG_FEED_PUBKEY` with no format translation needed.
//
// Bun/WebCrypto only exports Ed25519 private key material as PKCS8 (DER) or
// JWK — never "raw". PKCS8 for Ed25519 is a fixed 16-byte ASN.1 prefix
// (RFC 8410) followed by the raw 32-byte seed, so a seed round-trips through
// PKCS8 with a constant prefix instead of a real ASN.1 encoder/decoder:
//   302e020100300506032b657004220420 <32-byte seed>
const PKCS8_ED25519_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

/** TS's DOM typings want `Uint8Array<ArrayBuffer>` for WebCrypto BufferSource params; our arrays are always plain (non-shared) buffers, so this is a type-only cast. */
function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes as Uint8Array<ArrayBuffer>;
}

/** Generates a fresh, extractable ed25519 keypair. */
export async function generateKeyPair(): Promise<CryptoKeyPair> {
  return (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as unknown as CryptoKeyPair;
}

/** The raw 32-byte public key — paste-ready for a Rust `[u8; 32]` array literal, or feed to `verifyBytes`. */
export async function exportPublicKeyRaw(publicKey: CryptoKey): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.exportKey("raw", publicKey));
}

/** The private key's raw 32-byte seed, base64-encoded — the `CATALOG_FEED_PRIVATE_KEY` CI secret format. */
export async function exportPrivateKeySeedBase64(privateKey: CryptoKey): Promise<string> {
  const jwk = await crypto.subtle.exportKey("jwk", privateKey);
  if (!jwk.d) throw new Error("exported private JWK is missing 'd' (seed)");
  const seed = Buffer.from(jwk.d, "base64url");
  if (seed.length !== 32) throw new Error(`private key seed must be 32 bytes, got ${seed.length}`);
  return seed.toString("base64");
}

/** Rebuilds a sign-only `CryptoKey` from the base64 seed `keygen.ts` prints / `CATALOG_FEED_PRIVATE_KEY` holds. */
export async function importSigningKeyFromSeedBase64(seedBase64: string): Promise<CryptoKey> {
  let seed: Buffer;
  try {
    seed = Buffer.from(seedBase64, "base64");
  } catch {
    throw new Error("CATALOG_FEED_PRIVATE_KEY is not valid base64");
  }
  if (seed.length !== 32) {
    throw new Error(`CATALOG_FEED_PRIVATE_KEY must decode to 32 bytes, got ${seed.length}`);
  }
  const pkcs8 = Buffer.concat([PKCS8_ED25519_PREFIX, seed]);
  return crypto.subtle.importKey("pkcs8", toBufferSource(pkcs8), { name: "Ed25519" }, false, ["sign"]);
}

/** Signs `bytes`, returning the raw 64-byte signature (`ed25519_dalek::Signature::to_bytes()` layout — no DER wrapping). */
export async function signBytes(bytes: Uint8Array, signingKey: CryptoKey): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, signingKey, toBufferSource(bytes)));
}

/** Verifies a raw 64-byte signature against a raw 32-byte public key. Used by the round-trip test; the shipped verifier is `verify_with` in `crates/core/src/plugins/remote_catalog.rs`. */
export async function verifyBytes(bytes: Uint8Array, signature: Uint8Array, publicKeyRaw: Uint8Array): Promise<boolean> {
  const key = await crypto.subtle.importKey("raw", toBufferSource(publicKeyRaw), { name: "Ed25519" }, false, ["verify"]);
  return crypto.subtle.verify({ name: "Ed25519" }, key, toBufferSource(signature), toBufferSource(bytes));
}

/** Formats a raw public key as a Rust `[u8; 32]` array literal, ready to paste into `catalog_feed_key.rs`. */
export function toRustByteArrayLiteral(bytes: Uint8Array): string {
  return `[${Array.from(bytes).join(", ")}]`;
}
