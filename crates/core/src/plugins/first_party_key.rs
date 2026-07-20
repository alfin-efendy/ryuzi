//! The ed25519 public key that signs first-party component-plugin *bundles*
//! (the `plugin.sig` envelope over `release.json`; see `plugins::bundle`). The
//! matching PRIVATE key is a release/CI secret generated and consumed by unit
//! 11b's signer (`scripts/plugins/build-first-party.ts`) and is never
//! committed.
//!
//! This mirrors `plugins::catalog_feed_key` exactly, one layer up: the catalog
//! *feed* key signs which integrations exist; this key signs each downloadable
//! component release before it is installed. It is still the all-zero
//! placeholder — a deliberate, one-time HUMAN ops step, not something this
//! crate can safely invent a key for.
//!
//! # Fail-closed while the placeholder is in place
//! The all-zero key is a valid *low-order* Edwards point, so a non-strict
//! verify could be tricked into accepting a forged signature against it.
//! [`first_party_trusted_keys`] therefore NEVER hands the placeholder to
//! `verify_bundle`: while the constant is all-zero it returns an EMPTY trusted
//! set, so every bundle fails the untrusted-key check and NOTHING installs.
//! (`verify_bundle` itself also uses `verify_strict`, which rejects low-order
//! keys, as a second line of defense — see `plugins::bundle`.) The daemon's
//! first-party bootstrap detects the empty set and does nothing (no network,
//! no retry state), so the engine still ships; the first-party bundles simply
//! do not land until the real key is filled in below.
//!
//! To go live (done by unit 11b or a later rollout):
//! 1. Generate the signing keypair ONCE (11b's `build-first-party.ts` keygen).
//! 2. Store its base64 PRIVATE key as the CI secret consumed by the signer.
//!    Never commit it.
//! 3. Paste its printed `[u8; 32]` PUBLIC key below, replacing the all-zero
//!    placeholder, and ship that change in a normal PR — the public key is not
//!    a secret. Nothing else in this crate changes: [`first_party_trusted_keys`]
//!    starts returning a non-empty map and installs begin verifying against it.

use std::collections::HashMap;

/// The `key_id` first-party `plugin.sig` envelopes name (see
/// `plugins::bundle`'s signature protocol). 11b's signer MUST emit this exact
/// id in every first-party bundle's `plugin.sig`.
pub const FIRST_PARTY_KEY_ID: &str = "first-party";

/// The first-party bundle-signing public key. All-zero placeholder until the
/// real key is rolled out (see the module docs). Not a secret.
pub const FIRST_PARTY_PUBKEY: [u8; 32] = [0u8; 32]; // TODO(ops): real key from 11b's signer keygen

/// The trusted-key map passed to
/// [`crate::plugins::bundle::verify_bundle`] in production. Keyed by
/// [`FIRST_PARTY_KEY_ID`].
///
/// While [`FIRST_PARTY_PUBKEY`] is the all-zero placeholder this returns an
/// EMPTY map — the fail-closed property described in the module docs: no
/// bundle can be trusted until a real key ships. Tests never call this; they
/// inject their own generated verifying key directly.
pub fn first_party_trusted_keys() -> HashMap<String, [u8; 32]> {
    let mut map = HashMap::new();
    if FIRST_PARTY_PUBKEY != [0u8; 32] {
        map.insert(FIRST_PARTY_KEY_ID.to_string(), FIRST_PARTY_PUBKEY);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fail-closed guarantee: while the compiled-in key is the all-zero
    // placeholder, the trusted set is EMPTY, so `verify_bundle` rejects every
    // first-party bundle on the untrusted-key check before any crypto. A
    // silent revert to the placeholder can therefore never make a forged
    // first-party bundle installable.
    #[test]
    fn placeholder_key_yields_an_empty_trusted_set() {
        assert_eq!(FIRST_PARTY_PUBKEY, [0u8; 32], "still the placeholder");
        assert!(
            first_party_trusted_keys().is_empty(),
            "the all-zero placeholder must never be handed to verify_bundle"
        );
    }

    // The key id the map WOULD use once a real key ships must match the id
    // 11b's signer emits in `plugin.sig`, so a live key resolves rather than
    // being rejected as unknown.
    #[test]
    fn key_id_is_first_party() {
        assert_eq!(FIRST_PARTY_KEY_ID, "first-party");
    }
}
