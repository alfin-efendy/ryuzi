//! Remote plugin catalog: fetch + verify + cache a signed integration feed,
//! so new/updated catalog entries ship without a binary release. See
//! docs/superpowers/specs/2026-07-11-remote-plugin-catalog-design.md.

use serde::Deserialize;

use super::catalog_feed_key::CATALOG_FEED_PUBKEY;
use crate::store::{RemoteCatalogRow, Store};
use ryuzi_plugin_sdk::PluginManifest;

/// Default feed location: the `catalog.json` asset attached to the latest
/// GitHub release. Overridable via settings for self-hosted feeds.
pub const DEFAULT_CATALOG_FEED_URL: &str =
    "https://github.com/alfin-efendy/ryuzi/releases/latest/download/catalog.json";
/// Default cadence between background feed fetches (6 hours).
pub const DEFAULT_CATALOG_FETCH_INTERVAL_MS: u64 = 21_600_000;

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

/// HTTP GET seam for feed fetching, so tests can inject canned responses
/// without a real network call. A non-2xx status is a returned value, not an
/// error — only transport-level failures (DNS, connect, timeout, ...) are
/// `Err`.
#[async_trait::async_trait]
pub trait CatalogHttp: Send + Sync {
    /// GET `url`; returns `(status, body)`. Non-2xx is a returned status, not
    /// an error.
    async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)>;
}

/// Real `CatalogHttp` backed by `reqwest`.
pub struct ReqwestCatalogHttp {
    client: reqwest::Client,
}

impl ReqwestCatalogHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestCatalogHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl CatalogHttp for ReqwestCatalogHttp {
    async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
        let resp = self
            .client
            .get(url)
            .header("User-Agent", "ryuzi")
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?.to_vec();
        Ok((status, bytes))
    }
}

/// Outcome of a single feed fetch attempt. Never signals failure via `Err`;
/// callers (the cadence manager, doctor, RPC) inspect `applied` + `message`.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub applied: bool,
    pub sequence: u64,
    pub entries: usize,
    pub blocked: usize,
    pub message: String,
}

/// Fetch, verify, validate, and cache the signed remote catalog feed at
/// `feed_url`, using the production `CATALOG_FEED_PUBKEY`.
pub async fn fetch_and_cache(
    store: &Store,
    http: &dyn CatalogHttp,
    feed_url: &str,
) -> FetchOutcome {
    fetch_and_cache_with(store, http, feed_url, &CATALOG_FEED_PUBKEY).await
}

/// `fetch_and_cache` with an injectable verify key, so tests can sign with a
/// throwaway keypair instead of the (placeholder, all-zero) real one.
///
/// Never panics and never propagates fetch/parse/validation failures as an
/// `Err` — every failure path returns `FetchOutcome { applied: false, .. }`
/// with a human-readable `message`.
async fn fetch_and_cache_with(
    store: &Store,
    http: &dyn CatalogHttp,
    feed_url: &str,
    pubkey: &[u8; 32],
) -> FetchOutcome {
    let fail = |msg: String| FetchOutcome {
        applied: false,
        sequence: 0,
        entries: 0,
        blocked: 0,
        message: msg,
    };

    let sig_url = format!("{feed_url}.sig");
    let feed_bytes = match http.get(feed_url).await {
        Ok((s, b)) if (200..300).contains(&s) => b,
        Ok((s, _)) => return fail(format!("feed HTTP {s}")),
        Err(e) => return fail(format!("feed fetch failed: {e}")),
    };
    let sig_bytes = match http.get(&sig_url).await {
        Ok((s, b)) if (200..300).contains(&s) => b,
        Ok((s, _)) => return fail(format!("signature HTTP {s}")),
        Err(e) => return fail(format!("signature fetch failed: {e}")),
    };

    let last = store.get_catalog_feed_sequence().await.unwrap_or(0);
    let feed = match parse_and_check_with(&feed_bytes, &sig_bytes, last, pubkey) {
        Ok(f) => f,
        Err(e) => return fail(format!("feed rejected: {e:?}")),
    };

    // Validate each entry's manifest via the SDK; drop+log invalid ones.
    let now = crate::paths::now_ms();
    let mut rows: Vec<RemoteCatalogRow> = Vec::new();
    for e in &feed.entries {
        match PluginManifest::from_toml(&e.manifest_toml) {
            Ok(m) => rows.push(RemoteCatalogRow {
                id: e.id.clone(),
                manifest_toml: e.manifest_toml.clone(),
                version: m.version.clone(),
                sequence: feed.sequence,
                blocked: false,
                blocked_reason: None,
                fetched_at: now,
            }),
            Err(err) => tracing::warn!("catalog feed: dropping invalid entry {}: {err}", e.id),
        }
    }
    // Blocked entries: merge into an existing row if present, else record as
    // a standalone blocked row (id may or may not also be a valid entry).
    for b in &feed.blocked {
        if let Some(row) = rows.iter_mut().find(|r| r.id == b.id) {
            row.blocked = true;
            row.blocked_reason = Some(b.reason.clone());
        } else {
            rows.push(RemoteCatalogRow {
                id: b.id.clone(),
                manifest_toml: String::new(),
                version: String::new(),
                sequence: feed.sequence,
                blocked: true,
                blocked_reason: Some(b.reason.clone()),
                fetched_at: now,
            });
        }
    }

    let entries = rows.iter().filter(|r| !r.blocked).count();
    let blocked = rows.iter().filter(|r| r.blocked).count();
    if let Err(e) = store.upsert_remote_catalog(&rows).await {
        return fail(format!("cache write failed: {e}"));
    }
    let _ = store.set_catalog_feed_state(feed.sequence, "ok").await;
    FetchOutcome {
        applied: true,
        sequence: feed.sequence,
        entries,
        blocked,
        message: format!(
            "applied sequence {} ({entries} entries, {blocked} blocked)",
            feed.sequence
        ),
    }
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

    struct FakeHttp {
        // url suffix -> (status, bytes)
        feed: (u16, Vec<u8>),
        sig: (u16, Vec<u8>),
    }
    #[async_trait::async_trait]
    impl CatalogHttp for FakeHttp {
        async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
            if url.ends_with(".sig") {
                Ok(self.sig.clone())
            } else {
                Ok(self.feed.clone())
            }
        }
    }

    #[tokio::test]
    async fn fetch_and_cache_stores_verified_entries_and_sequence() {
        let bytes = feed_json(7).into_bytes();
        let sig = sign(&bytes);
        // NOTE: this test relies on the real CATALOG_FEED_PUBKEY matching the test
        // key; instead, fetch_and_cache takes an injected verify via a helper.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(outcome.applied);
        assert_eq!(outcome.sequence, 7);
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 7);
        assert_eq!(store.list_remote_catalog().await.unwrap().len(), 1);
    }

    // A non-2xx feed response must warn-and-continue: no apply, no cache write,
    // sequence untouched. A periodic fetcher hitting a 404 must not crash.
    #[tokio::test]
    async fn fetch_rejects_non_2xx_feed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let http = FakeHttp {
            feed: (404, vec![]),
            sig: (200, vec![]),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert!(store.list_remote_catalog().await.unwrap().is_empty());
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 0);
    }

    // A tampered feed served with the real signature must fail verification and
    // never be applied — the untrusted bytes must not reach the cache.
    #[tokio::test]
    async fn fetch_rejects_bad_signature() {
        let bytes = feed_json(7).into_bytes();
        let sig = sign(&bytes);
        let mut tampered = bytes.clone();
        tampered[40] ^= 0xff;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let http = FakeHttp {
            feed: (200, tampered),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert!(store.list_remote_catalog().await.unwrap().is_empty());
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 0);
    }

    // An older-sequence feed must be rejected by anti-rollback and must NOT
    // overwrite the already-accepted sequence.
    #[tokio::test]
    async fn fetch_rejects_rollback_feed_keeps_accepted_sequence() {
        let bytes = feed_json(3).into_bytes();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        store.set_catalog_feed_state(9, "ok").await.unwrap();
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 9);
    }

    // A feed with one manifest-invalid entry (empty name) and one valid entry
    // must drop the invalid one, still apply, and cache only the valid id.
    #[tokio::test]
    async fn fetch_drops_invalid_entry_but_applies_valid_ones() {
        let feed = concat!(
            r#"{"schemaVersion":1,"sequence":8,"generatedAt":0,"#,
            r#""entries":["#,
            r#"{"id":"bad","manifestToml":"contract=1\nid=\"bad\"\nname=\"\"\nversion=\"1.0.0\""},"#,
            r#"{"id":"good","manifestToml":"contract=1\nid=\"good\"\nname=\"Good\"\nversion=\"2.0.0\""}"#,
            r#"],"blocked":[]}"#,
        );
        let bytes = feed.as_bytes().to_vec();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(outcome.applied);
        assert_eq!(outcome.entries, 1);
        let rows = store.list_remote_catalog().await.unwrap();
        let visible: Vec<_> = rows.iter().filter(|r| !r.blocked).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "good");
    }
}
