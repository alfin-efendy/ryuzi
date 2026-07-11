//! The ed25519 public key that signs the remote catalog feed. The matching
//! PRIVATE key is a release/CI secret (`CATALOG_FEED_PRIVATE_KEY`, consumed
//! by `scripts/catalog/build-feed.ts`) and is never committed.
//!
//! This is still the all-zero placeholder — a deliberate, one-time HUMAN ops
//! step, not something this crate can safely invent a key for. The all-zero
//! key is a valid *low-order* Edwards point, so a non-strict verify could be
//! tricked into accepting a forged signature against it; `verify_with`
//! (`crates/core/src/plugins/remote_catalog.rs`) therefore rejects it two
//! ways — an explicit all-zero guard AND `verify_strict` (which rejects
//! low-order keys). While this placeholder is in place, EVERY feed is rejected
//! (the remote catalog is fail-closed), so the engine still ships and enables
//! the embedded catalog either way.
//!
//! To go live:
//! 1. Run `bun scripts/catalog/keygen.ts` ONCE (a second run makes an
//!    unrelated keypair, not a recovery of the first).
//! 2. Store its printed base64 PRIVATE key as the CI secret
//!    `CATALOG_FEED_PRIVATE_KEY` (repo Settings -> Secrets and variables ->
//!    Actions). Never commit it.
//! 3. Paste its printed `[u8; 32]` PUBLIC key below, replacing the
//!    all-zero placeholder, and ship that change in a normal PR — the
//!    public key is not a secret.
//! 4. The next release's `catalog-feed` job (`.github/workflows/release.yml`)
//!    then builds and publishes a feed the new binary can verify. See
//!    docs/development/plugins.md#remote-catalog for the full picture.
pub const CATALOG_FEED_PUBKEY: [u8; 32] = [0u8; 32]; // TODO(ops): real key from `bun scripts/catalog/keygen.ts`
