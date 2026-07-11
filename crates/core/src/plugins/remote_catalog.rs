//! Remote plugin catalog: fetch + verify + cache a signed integration feed,
//! so new/updated catalog entries ship without a binary release. See
//! docs/superpowers/specs/2026-07-11-remote-plugin-catalog-design.md.

use serde::Deserialize;

use super::catalog_feed_key::CATALOG_FEED_PUBKEY;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFeed {
    pub schema_version: u32,
    pub sequence: u64,
    #[serde(default)]
    pub generated_at: i64,
    #[serde(default)]
    pub entries: Vec<CatalogFeedEntry>,
    #[serde(default)]
    pub blocked: Vec<CatalogBlockedEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFeedEntry {
    pub id: String,
    pub manifest_toml: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogBlockedEntry {
    pub id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub since_sequence: u64,
}

#[derive(Debug)]
pub enum CatalogFeedError {
    BadSignature,
    ParseError(String),
    UnsupportedSchema(u32),
    Rollback { got: u64, have: u64 },
}

pub fn verify_feed_signature(feed_bytes: &[u8], sig_bytes: &[u8]) -> bool {
    verify_with(feed_bytes, sig_bytes, &CATALOG_FEED_PUBKEY)
}

fn verify_with(feed_bytes: &[u8], sig_bytes: &[u8], pubkey: &[u8; 32]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig_arr);
    vk.verify(feed_bytes, &sig).is_ok()
}

/// Verify the detached signature over `feed_bytes`, then parse, then enforce
/// schema + anti-rollback. Returns the parsed feed only when fully trusted.
pub fn parse_and_check(
    feed_bytes: &[u8],
    sig_bytes: &[u8],
    last_sequence: u64,
) -> Result<CatalogFeed, CatalogFeedError> {
    parse_and_check_with(feed_bytes, sig_bytes, last_sequence, &CATALOG_FEED_PUBKEY)
}

fn parse_and_check_with(
    feed_bytes: &[u8],
    sig_bytes: &[u8],
    last_sequence: u64,
    pubkey: &[u8; 32],
) -> Result<CatalogFeed, CatalogFeedError> {
    if !verify_with(feed_bytes, sig_bytes, pubkey) {
        return Err(CatalogFeedError::BadSignature);
    }
    let feed: CatalogFeed = serde_json::from_slice(feed_bytes)
        .map_err(|e| CatalogFeedError::ParseError(e.to_string()))?;
    if feed.schema_version != 1 {
        return Err(CatalogFeedError::UnsupportedSchema(feed.schema_version));
    }
    if feed.sequence < last_sequence {
        return Err(CatalogFeedError::Rollback {
            got: feed.sequence,
            have: last_sequence,
        });
    }
    Ok(feed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // A deterministic test keypair; the test overrides the verify key.
    fn test_keypair() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn sign(bytes: &[u8]) -> Vec<u8> {
        test_keypair().sign(bytes).to_bytes().to_vec()
    }

    fn feed_json(seq: u64) -> String {
        format!(
            r#"{{"schemaVersion":1,"sequence":{seq},"generatedAt":0,
                "entries":[{{"id":"acme","manifestToml":"contract=1\nid=\"acme\"\nname=\"Acme\"\nversion=\"1.0.0\""}}],
                "blocked":[]}}"#
        )
    }

    #[test]
    fn valid_signed_feed_parses() {
        let bytes = feed_json(5).into_bytes();
        let sig = sign(&bytes);
        let pubkey = test_keypair().verifying_key().to_bytes();
        let feed = parse_and_check_with(&bytes, &sig, 0, &pubkey).unwrap();
        assert_eq!(feed.sequence, 5);
        assert_eq!(feed.entries[0].id, "acme");
    }

    #[test]
    fn tampered_bytes_rejected() {
        let bytes = feed_json(5).into_bytes();
        let sig = sign(&bytes);
        let mut tampered = bytes.clone();
        tampered[40] ^= 0xff;
        let pubkey = test_keypair().verifying_key().to_bytes();
        assert!(matches!(
            parse_and_check_with(&tampered, &sig, 0, &pubkey),
            Err(CatalogFeedError::BadSignature)
        ));
    }

    #[test]
    fn lower_sequence_rejected_anti_rollback() {
        let bytes = feed_json(3).into_bytes();
        let sig = sign(&bytes);
        let pubkey = test_keypair().verifying_key().to_bytes();
        assert!(matches!(
            parse_and_check_with(&bytes, &sig, 5, &pubkey),
            Err(CatalogFeedError::Rollback { got: 3, have: 5 })
        ));
    }
}
