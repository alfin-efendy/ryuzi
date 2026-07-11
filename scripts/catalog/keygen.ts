// Usage: bun scripts/catalog/keygen.ts
//
// Generates a fresh ed25519 keypair for signing the remote plugin catalog
// feed (see build-feed.ts and crates/core/src/plugins/catalog_feed_key.rs).
//
// Run this ONCE per key (or once per rotation):
//   1. Run this script.
//   2. Store the printed base64 PRIVATE key as the CI secret
//      `CATALOG_FEED_PRIVATE_KEY` (repo Settings -> Secrets and variables ->
//      Actions -> New repository secret). Do NOT commit it, paste it into a
//      PR, or store it anywhere else.
//   3. Paste the printed `[u8; 32]` array into `catalog_feed_key.rs`'s
//      `CATALOG_FEED_PUBKEY` constant, replacing the all-zero placeholder,
//      and ship that change in a normal PR — the public key is not a secret.
//
// Rotating the key later means every previously-cached feed on every install
// verifies against whichever pubkey is compiled into ITS binary — an old
// binary won't trust a feed signed with a new key until it upgrades. Publish
// a fresh signed `catalog.json` immediately after a binary carrying the new
// pubkey ships.
import { exportPrivateKeySeedBase64, exportPublicKeyRaw, generateKeyPair, toRustByteArrayLiteral } from "./ed25519.ts";

async function main() {
  const keyPair = await generateKeyPair();
  const publicKeyRaw = await exportPublicKeyRaw(keyPair.publicKey);
  const privateKeySeedBase64 = await exportPrivateKeySeedBase64(keyPair.privateKey);

  console.log("=== ed25519 keypair for the remote plugin catalog feed ===");
  console.log("");
  console.log("1) Paste into crates/core/src/plugins/catalog_feed_key.rs");
  console.log("   (replaces the all-zero placeholder — PUBLIC, safe to commit):");
  console.log("");
  console.log(`    pub const CATALOG_FEED_PUBKEY: [u8; 32] = ${toRustByteArrayLiteral(publicKeyRaw)};`);
  console.log("");
  console.log("2) Store as the CI secret CATALOG_FEED_PRIVATE_KEY");
  console.log("   (repo Settings -> Secrets and variables -> Actions).");
  console.log("   SECRET — do not commit, do not paste anywhere else:");
  console.log("");
  console.log(`    ${privateKeySeedBase64}`);
  console.log("");
  console.log("Run this script exactly once per key (or rotation) — a second run produces");
  console.log("an unrelated keypair, not a recovery of the first.");
}

if (import.meta.main) {
  await main();
}
