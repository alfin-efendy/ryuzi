//! Xiaomi MiMo free-tier wire protocol.
//!
//! The free chat endpoint (`…/api/free-ai/openai/chat`) sits behind an
//! anti-abuse gate with three requirements (verified live 2026-07-10, wire
//! protocol of the MiMoCode CLI):
//!
//! 1. a bootstrap JWT minted from `…/api/free-ai/bootstrap` (keyed by a
//!    stable device fingerprint), sent as the bearer token;
//! 2. a Chrome-like `User-Agent` plus `x-mimo-source` and
//!    `x-session-affinity` headers;
//! 3. a system message whose text carries the exact MiMoCode marker — a
//!    request missing any of these 403s with "Illegal access".
//!
//! The JWT is cached per process and re-minted on demand; callers
//! ([`client::send_upstream`] and the model probe) re-bootstrap once when
//! the upstream rejects a cached token.

use std::sync::{Mutex, OnceLock};

use base64::Engine;
use serde_json::{json, Value};

pub(crate) const BOOTSTRAP_URL: &str = "https://api.xiaomimimo.com/api/free-ai/bootstrap";

/// Anti-abuse gate marker: the free chat endpoint requires a system message
/// containing this exact MiMoCode signature substring.
pub(crate) const SYSTEM_MARKER: &str =
    "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks.";

/// The gate also rejects non-browser user agents with 403 "Illegal access".
pub(crate) const CHROME_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Lifetime assumed for a JWT whose `exp` claim can't be parsed.
const JWT_FALLBACK_TTL_MS: i64 = 3_000 * 1_000;
/// A token this close to expiry is re-minted proactively.
const JWT_EXPIRY_BUFFER_MS: i64 = 300 * 1_000;

/// Process-wide bootstrap JWT cache: `(token, expiry ms)`.
static JWT_CACHE: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// The cached JWT, if present and not within the expiry buffer.
pub(crate) fn cached_jwt() -> Option<String> {
    let guard = JWT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()
        .filter(|(_, exp)| crate::paths::now_ms() < exp - JWT_EXPIRY_BUFFER_MS)
        .map(|(jwt, _)| jwt.clone())
}

pub(crate) fn store_jwt(jwt: &str) {
    let exp = jwt_exp_ms(jwt);
    *JWT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((jwt.to_string(), exp));
}

pub(crate) fn invalidate_jwt() {
    *JWT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Serializes tests that assert on the process-wide JWT cache.
#[cfg(test)]
pub(crate) fn test_cache_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Expiry of a JWT in ms, from its `exp` claim (seconds); tokens that don't
/// parse get the fallback TTL from now.
fn jwt_exp_ms(jwt: &str) -> i64 {
    let claim = jwt.split('.').nth(1).and_then(|payload| {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?;
        serde_json::from_slice::<Value>(&bytes)
            .ok()?
            .get("exp")?
            .as_i64()
    });
    match claim {
        Some(exp_s) => exp_s * 1_000,
        None => crate::paths::now_ms() + JWT_FALLBACK_TTL_MS,
    }
}

/// Stable per-install device fingerprint, mirroring the MiMoCode CLI's
/// hostname|platform|arch|user seed. The server only needs an opaque stable
/// hex id per install; the anti-abuse gate keys its rate limits on it, so
/// uniqueness across installs matters (a shared seed means shared throttle
/// buckets). `$HOSTNAME` is a non-exported shell var on most launches, so we
/// prefer a genuinely per-install identifier — the Linux machine-id — and
/// fall back to hostname/user env when it's absent.
fn fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let machine_id = std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "ryuzi-host".to_string());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "ryuzi-user".to_string());
    let seed = format!(
        "{machine_id}|{host}|{}|{}|{user}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let digest = Sha256::digest(seed.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Per-process session-affinity id (`ses_` + 24 lowercase hex chars).
pub(crate) fn session_affinity() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| format!("ses_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]))
}

/// Prepend the MiMoCode marker as a system message unless one already
/// carries it. Bodies without a `messages` array are left untouched.
pub(crate) fn inject_system_marker(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    let has_marker = messages.iter().any(|m| {
        m.get("role").and_then(Value::as_str) == Some("system")
            && m.get("content")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains(SYSTEM_MARKER))
    });
    if has_marker {
        return;
    }
    messages.insert(0, json!({"role": "system", "content": SYSTEM_MARKER}));
}

/// Classify a MiMo non-2xx error body. The free tier signals a transient
/// abuse throttle as HTTP 400 with `{"error":{"code":"441","type":
/// "risk_control"}}`, and refuses ungated requests with `illegal_access` —
/// both are transient/environmental (rate limit, IP heat), never a permanent
/// "bad model" verdict for `mimo-auto` (the only, always-valid MiMo model).
/// Returns a user-facing message when the body is a known transient block so
/// the probe can report `unknown` (never persisted, never hidden) instead of
/// a misleading `invalid`.
pub(crate) fn transient_block_message(model: &str, body: &str) -> Option<String> {
    let b = body.to_ascii_lowercase();
    if b.contains("risk_control") || b.contains("\"441\"") {
        Some(format!(
            "Model {model} is temporarily rate-limited by MiMo (risk control) — try again in a few minutes."
        ))
    } else if b.contains("illegal_access") {
        Some(format!(
            "Model {model} was blocked by MiMo's access gate — try again shortly."
        ))
    } else {
        None
    }
}

/// The cached JWT, minting a fresh one from the bootstrap endpoint when the
/// cache is empty or stale. `url_override` is test plumbing
/// ([`client::UpstreamCtx::mimo_bootstrap_url_override`]).
pub(crate) async fn ensure_jwt(
    http: &reqwest::Client,
    url_override: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(jwt) = cached_jwt() {
        return Ok(jwt);
    }
    let url = url_override.unwrap_or(BOOTSTRAP_URL);
    let resp = http
        .post(url)
        .header("user-agent", CHROME_UA)
        .json(&json!({"client": fingerprint()}))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("MiMo bootstrap failed: {e}"))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "MiMo bootstrap failed: HTTP {}",
        resp.status()
    );
    let body: Value = resp.json().await?;
    let jwt = body
        .get("jwt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("MiMo bootstrap returned no JWT"))?;
    store_jwt(jwt);
    Ok(jwt.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal JWT with a controlled `exp` claim (seconds) — only the middle
    /// segment is ever parsed.
    fn tjwt(exp_ms: i64) -> String {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"exp": exp_ms / 1000})).unwrap());
        format!("h.{payload}.sig")
    }

    #[test]
    fn transient_block_message_flags_risk_control_and_illegal_access() {
        let risk = r#"{"error":{"code":"441","message":"Detected high-frequency non-compliant requests","type":"risk_control"}}"#;
        let m = transient_block_message("mimo-auto", risk).unwrap();
        assert!(m.contains("rate-limited"), "{m}");
        assert!(m.contains("mimo-auto"));

        let illegal =
            r#"{"error":{"code":"403","message":"Illegal access","type":"illegal_access"}}"#;
        assert!(transient_block_message("mimo-auto", illegal)
            .unwrap()
            .contains("access gate"));

        // A genuine bad-request body (not a transient block) is NOT reclassified.
        let real = r#"{"error":{"message":"model not found","type":"invalid_request_error"}}"#;
        assert!(transient_block_message("mimo-auto", real).is_none());
        assert!(transient_block_message("mimo-auto", "").is_none());
    }

    #[test]
    fn marker_injection_prepends_once_and_is_idempotent() {
        let mut body = json!({
            "model": "mimo-auto",
            "messages": [{"role": "user", "content": "ping"}]
        });
        inject_system_marker(&mut body);
        inject_system_marker(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], SYSTEM_MARKER);
        assert_eq!(messages[1]["content"], "ping");
    }

    #[test]
    fn marker_injection_respects_an_existing_marker_system_message() {
        let mut body = json!({"messages": [
            {"role": "system", "content": format!("{SYSTEM_MARKER} Follow the house style.")},
            {"role": "user", "content": "hi"}
        ]});
        inject_system_marker(&mut body);
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn marker_injection_leaves_non_chat_bodies_alone() {
        let mut body = json!({"model": "m"});
        inject_system_marker(&mut body);
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn jwt_expiry_comes_from_the_exp_claim_with_a_ttl_fallback() {
        assert_eq!(jwt_exp_ms(&tjwt(1_900_000_000_000)), 1_900_000_000_000);
        let now = crate::paths::now_ms();
        let fallback = jwt_exp_ms("not-a-jwt");
        assert!(fallback > now && fallback <= now + JWT_FALLBACK_TTL_MS + 1_000);
    }

    #[test]
    fn cache_respects_the_expiry_buffer() {
        let _lock = test_cache_lock();
        store_jwt(&tjwt(crate::paths::now_ms() + 10 * 60 * 1000));
        assert!(cached_jwt().is_some());
        // Ten seconds inside the 5-minute refresh buffer -> treated as stale.
        store_jwt(&tjwt(
            crate::paths::now_ms() + JWT_EXPIRY_BUFFER_MS - 10_000,
        ));
        assert!(cached_jwt().is_none());
        invalidate_jwt();
    }

    #[test]
    fn session_affinity_is_stable_and_well_formed() {
        let a = session_affinity();
        assert!(a.starts_with("ses_"));
        assert_eq!(a.len(), "ses_".len() + 24);
        assert_eq!(a, session_affinity());
    }

    #[test]
    fn fingerprint_is_a_stable_sha256_hex() {
        let f = fingerprint();
        assert_eq!(f.len(), 64);
        assert!(f.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(f, fingerprint());
    }

    #[tokio::test]
    // Test-only serialization of the process-wide JWT cache; the guard
    // legitimately spans awaits on the current_thread test runtime.
    #[allow(clippy::await_holding_lock)]
    async fn bootstrap_mints_and_caches_a_jwt() {
        use axum::{routing::post, Json, Router};

        let _lock = test_cache_lock();
        invalidate_jwt();

        let jwt = tjwt(crate::paths::now_ms() + 3_600_000);
        let served = jwt.clone();
        let app = Router::new().route(
            "/bootstrap",
            post(move |Json(body): Json<serde_json::Value>| {
                let served = served.clone();
                async move {
                    // The bootstrap payload carries the device fingerprint.
                    assert_eq!(body["client"].as_str().unwrap().len(), 64);
                    Json(json!({"jwt": served}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/bootstrap");
        let got = ensure_jwt(&http, Some(&url)).await.unwrap();
        assert_eq!(got, jwt);
        assert_eq!(cached_jwt().as_deref(), Some(jwt.as_str()));
        invalidate_jwt();
    }

    #[tokio::test]
    // Test-only serialization of the process-wide JWT cache; the guard
    // legitimately spans awaits on the current_thread test runtime.
    #[allow(clippy::await_holding_lock)]
    async fn bootstrap_failure_is_an_error_not_a_cached_value() {
        use axum::{http::StatusCode, routing::post, Json, Router};

        let _lock = test_cache_lock();
        invalidate_jwt();

        let app = Router::new().route(
            "/bootstrap",
            post(|| async { (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/bootstrap");
        let err = ensure_jwt(&http, Some(&url)).await.unwrap_err();
        assert!(err.to_string().contains("bootstrap"), "{err}");
        assert!(cached_jwt().is_none());
    }
}
