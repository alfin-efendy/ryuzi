//! Device pairing: turns a single-use, TTL-bounded plaintext code into a
//! long-lived device bearer token (Phase 2 remote-runner bootstrap — a
//! device has no token yet, so `POST /pair` in `serve.rs` is the one
//! publicly reachable route that accepts one of these codes instead of a
//! bearer).
//!
//! Both the code and the resulting token are persisted HASHED
//! (`sha256_hex`) via the `Store` accessors added in P2-2 — the plaintext
//! values exist only transiently: `mint_code` hands the plaintext code back
//! to its caller (to display/transmit to the device being paired, exactly
//! once); `redeem` hands the plaintext token back to the just-paired
//! device. Neither plaintext is ever written to the database.
//!
//! `now_ms` is always an injected parameter, never read internally by this
//! module, so both functions are deterministically testable (see the tests
//! below for expiry/single-use coverage) without a real clock.

use crate::store::Store;
use crate::update::asset::sha256_hex;

/// Generate a fresh 64-hex-char secret. Same shape as `control_token`'s
/// (two concatenated UUIDv4s in their simple/no-hyphen form) — deliberately
/// reusing that pattern rather than inventing a new one.
fn gen_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Mint a new single-use pairing code, valid until `now_ms + ttl_ms`. Only
/// `sha256_hex(code)` is persisted (via `Store::insert_pairing_code`); the
/// returned plaintext `code` is the caller's one and only chance to
/// display/transmit it to the device being paired.
pub async fn mint_code(store: &Store, ttl_ms: i64, now_ms: i64) -> anyhow::Result<String> {
    let code = gen_secret();
    store
        .insert_pairing_code(&sha256_hex(code.as_bytes()), now_ms + ttl_ms)
        .await?;
    Ok(code)
}

/// Redeem a pairing code for a new device token. Returns `Some(token)`
/// (plaintext — only `sha256_hex(token)` is persisted, in `devices.
/// token_hash`) iff `code` matches an unexpired, not-yet-consumed pairing
/// code; `None` otherwise (wrong code, already consumed, or expired — all
/// three are indistinguishable to the caller, deliberately, so `POST /pair`
/// can map every `None` to a flat 401 without leaking which case applied).
///
/// Consumption is atomic at the store layer (`Store::consume_pairing_code`
/// is a single `DELETE ... WHERE code_hash = ? AND expires_at > ?`), so
/// concurrent redemptions racing on the same code can never both succeed —
/// a code is usable exactly once.
pub async fn redeem(
    store: &Store,
    code: &str,
    device_name: &str,
    now_ms: i64,
) -> anyhow::Result<Option<String>> {
    let code_hash = sha256_hex(code.as_bytes());
    if !store.consume_pairing_code(&code_hash, now_ms).await? {
        return Ok(None);
    }

    let token = gen_secret();
    store
        .insert_device(
            &uuid::Uuid::new_v4().to_string(),
            device_name,
            &sha256_hex(token.as_bytes()),
        )
        .await?;
    Ok(Some(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh in-memory-file-backed `Store`. Mirrors `serve.rs`'s
    /// `test_cp()`: the backing `NamedTempFile` is a local binding that
    /// outlives the whole test function body (dropped only at the end of
    /// scope), so the already-open connection stays valid for every
    /// operation the test performs, matching the pattern used throughout
    /// this crate's tests.
    async fn test_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        Store::open(tmp.path()).await.unwrap()
    }

    const NOW: i64 = 1_700_000_000_000;

    #[tokio::test]
    async fn mint_then_redeem_returns_a_token_that_authenticates_a_new_device() {
        let store = test_store().await;
        let code = mint_code(&store, 60_000, NOW).await.unwrap();

        let token = redeem(&store, &code, "alfin-laptop", NOW)
            .await
            .unwrap()
            .expect("a freshly minted, unexpired code redeems a token");
        assert_eq!(token.len(), 64);

        let device = store
            .find_device_by_token_hash(&sha256_hex(token.as_bytes()))
            .await
            .unwrap()
            .expect("the minted token's hash resolves to the new device row");
        assert_eq!(device.name, "alfin-laptop");
    }

    #[tokio::test]
    async fn redeem_with_the_wrong_code_is_none() {
        let store = test_store().await;
        mint_code(&store, 60_000, NOW).await.unwrap();

        let result = redeem(&store, "not-the-right-code", "some-device", NOW)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn redeem_is_single_use() {
        let store = test_store().await;
        let code = mint_code(&store, 60_000, NOW).await.unwrap();

        let first = redeem(&store, &code, "device-1", NOW).await.unwrap();
        assert!(first.is_some());

        // Same code, second attempt: already consumed.
        let second = redeem(&store, &code, "device-2", NOW).await.unwrap();
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn redeem_after_expiry_is_none() {
        let store = test_store().await;
        // A 1ms TTL, redeemed 2ms later, is unambiguously past expiry.
        let code = mint_code(&store, 1, NOW).await.unwrap();

        let result = redeem(&store, &code, "some-device", NOW + 2).await.unwrap();
        assert!(result.is_none());
    }
}
