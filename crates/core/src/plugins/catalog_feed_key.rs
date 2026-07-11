//! The ed25519 public key that signs the remote catalog feed. The matching
//! PRIVATE key is a release/CI secret (see scripts/catalog/build-feed.ts) and
//! is never committed. Replace the placeholder below with the real 32-byte
//! public key emitted by the keygen step before shipping a signed feed.
pub const CATALOG_FEED_PUBKEY: [u8; 32] = [0u8; 32]; // TODO(ops): real key from keygen
