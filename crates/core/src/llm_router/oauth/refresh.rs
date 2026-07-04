//! Live-traffic OAuth token refresh: proactive (`ensure_fresh`) ahead of a
//! request, and reactive (`force_refresh`) on a 401. Both funnel through a
//! process-global single-flight lock keyed by connection id so concurrent
//! callers don't hammer the token endpoint or race writes to the store.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! concept from open-sse/services/tokenRefresh/*.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Result};

use crate::llm_router::connections::{
    get_connection, is_oauth, update_connection, ConnectionData, ConnectionRow,
};
use crate::llm_router::registry::oauth_config;
use crate::paths::now_ms;
use crate::store::Store;

/// Refresh-token error codes that mean the token is dead for good — no retry
/// will help, the user has to re-login.
const TERMINAL_ERRORS: &[&str] = &["invalid_grant", "refresh_token_reused", "invalid_request"];

/// Per-connection-id single-flight registry. Each entry is a
/// `tokio::sync::Mutex<()>` — holding its guard means "I'm the one refreshing
/// this connection right now." The outer `std::sync::Mutex` only guards the
/// HashMap itself and is never held across an `.await`.
static REFRESH_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn lock_for(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let registry = REFRESH_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// True if `data` needs a refresh before it can be used: expiry is within
/// `refresh_lead_ms` of `now` (or already missing/expired), or the token has
/// aged past `max_refresh_age_ms` since its last refresh (if the provider
/// enforces a max age).
pub fn needs_refresh(
    cfg: &crate::llm_router::registry::OAuthConfig,
    data: &ConnectionData,
    now_ms: i64,
) -> bool {
    let expiry_due = match data.expires_at {
        Some(exp) => exp - now_ms < cfg.refresh_lead_ms,
        None => true,
    };
    if expiry_due {
        return true;
    }
    if let Some(max_age) = cfg.max_refresh_age_ms {
        let age_due = match data.last_refresh_at {
            Some(last) => now_ms - last >= max_age,
            None => true,
        };
        if age_due {
            return true;
        }
    }
    false
}

/// Proactive refresh: no-op unless `conn` is an OAuth connection that
/// currently needs one. On success `conn.data` is updated in place and
/// persisted.
pub async fn ensure_fresh(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
) -> Result<()> {
    if !is_oauth(conn) {
        return Ok(());
    }
    let cfg = oauth_config(&conn.provider)
        .ok_or_else(|| anyhow!("no OAuth config for provider `{}`", conn.provider))?;
    if !needs_refresh(cfg, &conn.data, now_ms()) {
        return Ok(());
    }
    refresh_at(store, http, conn, cfg.token_url).await
}

/// Reactive refresh: unconditional, single-flight refresh — used after a
/// request comes back 401. Unlike [`ensure_fresh`], this ALWAYS performs the
/// network round-trip, even if the connection's `expires_at`/`last_refresh_at`
/// make it look fresh: a 401 means the upstream rejected the token (commonly
/// revoked server-side while still technically unexpired), so re-sending the
/// same token would just 401 again. On a terminal provider error this marks
/// the connection `needs_relogin`, persists that, and returns `Err`.
pub async fn force_refresh(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
) -> Result<()> {
    let cfg = oauth_config(&conn.provider)
        .ok_or_else(|| anyhow!("no OAuth config for provider `{}`", conn.provider))?;
    force_refresh_with_token_url(store, http, conn, cfg.token_url).await
}

/// Same as [`force_refresh`] but against an explicit `token_url` (so tests —
/// and the router's per-`AppState` override — can point it at a mock server
/// instead of the provider's real, static registry endpoint).
pub async fn force_refresh_with_token_url(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
    token_url: &str,
) -> Result<()> {
    refresh_at_impl(store, http, conn, token_url, true).await
}

/// Single-flight refresh against an explicit `token_url` (so tests can point
/// this at a mock server). Acquires the per-connection-id lock, re-reads the
/// connection from the store (another task may have already refreshed it
/// while we were waiting), and skips the HTTP round-trip if it's fresh now.
/// This is the PROACTIVE seam — it still honors the freshness short-circuit.
/// For the reactive (post-401) seam that must never short-circuit, use
/// [`force_refresh`] / [`force_refresh_with_token_url`].
pub async fn refresh_at(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
    token_url: &str,
) -> Result<()> {
    refresh_at_impl(store, http, conn, token_url, false).await
}

/// Shared implementation behind [`refresh_at`] (proactive, `force = false`)
/// and [`force_refresh_with_token_url`] (reactive, `force = true`). When
/// `force` is true the freshness short-circuit below is skipped, so the
/// network round-trip always happens.
async fn refresh_at_impl(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
    token_url: &str,
    force: bool,
) -> Result<()> {
    let lock = lock_for(&conn.id);
    let _guard = lock.lock().await;

    // Someone else may have refreshed this connection while we waited for
    // the lock — re-read and, if it's fresh now (per the provider's own
    // policy) AND we're not forcing, just adopt that state instead of
    // hitting the network again. The forced (reactive) path always adopts
    // the latest persisted state as its refresh base below, but never skips
    // the network round-trip on freshness alone — a 401 already told us the
    // "fresh" token was rejected.
    if let Some(latest) = get_connection(store, &conn.id).await? {
        let stale = match oauth_config(&conn.provider) {
            Some(cfg) => needs_refresh(cfg, &latest.data, now_ms()),
            None => true,
        };
        if !force && !stale {
            conn.data = latest.data;
            conn.updated_at = latest.updated_at;
            return Ok(());
        }
        // Adopt the latest persisted state as our refresh base (keeps the
        // refresh token current even if we're not fresh yet, or if we're
        // forcing regardless of freshness).
        conn.data = latest.data;
        conn.updated_at = latest.updated_at;
    }

    let refresh_token = match conn.data.refresh_token.clone() {
        Some(rt) => rt,
        None => {
            conn.data.needs_relogin = Some(true);
            persist(store, conn).await?;
            return Err(anyhow!(
                "re-login required for {}: no refresh token on file",
                conn.provider
            ));
        }
    };
    let cfg = oauth_config(&conn.provider)
        .ok_or_else(|| anyhow!("no OAuth config for provider `{}`", conn.provider))?;

    let resp = http
        .post(token_url)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": cfg.client_id,
        }))
        .send()
        .await?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);

    let error_code = body.get("error").and_then(|v| v.as_str());
    let is_terminal = matches!(error_code, Some(code) if TERMINAL_ERRORS.contains(&code));
    if is_terminal {
        conn.data.needs_relogin = Some(true);
        persist(store, conn).await?;
        return Err(anyhow!("re-login required for {}", conn.provider));
    }
    if !status.is_success() {
        return Err(anyhow!(
            "refresh request for {} failed with status {status}",
            conn.provider
        ));
    }

    let now = now_ms();
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let Some(access_token) = access_token else {
        return Err(anyhow!(
            "refresh response for {} was missing `access_token`",
            conn.provider
        ));
    };
    let new_refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);

    conn.data.access_token = Some(access_token);
    conn.data.refresh_token = new_refresh_token.or(conn.data.refresh_token.clone());
    conn.data.expires_at = Some(now + expires_in * 1000);
    conn.data.last_refresh_at = Some(now);
    conn.data.needs_relogin = Some(false);

    persist(store, conn).await
}

async fn persist(store: &Arc<Store>, conn: &mut ConnectionRow) -> Result<()> {
    conn.updated_at = now_ms();
    update_connection(store, conn.clone()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::ConnectionData;
    use crate::llm_router::registry::oauth_config;

    #[test]
    fn needs_refresh_respects_lead_and_max_age() {
        let cfg = oauth_config("openai-oauth").unwrap(); // lead 5d, max age 8d
        let now = 10_000_000_000_i64;
        let mut d = ConnectionData {
            expires_at: Some(now + 6 * 24 * 3600 * 1000), // 6d out > 5d lead -> fresh...
            last_refresh_at: Some(now - 1000),
            ..Default::default()
        };
        assert!(!needs_refresh(cfg, &d, now));
        d.expires_at = Some(now + 4 * 24 * 3600 * 1000); // 4d < 5d lead -> refresh
        assert!(needs_refresh(cfg, &d, now));
        // max-age forces refresh even if expiry is far
        d.expires_at = Some(now + 100 * 24 * 3600 * 1000);
        d.last_refresh_at = Some(now - 9 * 24 * 3600 * 1000); // 9d > 8d max
        assert!(needs_refresh(cfg, &d, now));
    }

    #[tokio::test]
    async fn ensure_fresh_refreshes_and_persists_new_token() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["grant_type"], "refresh_token");
                Json(serde_json::json!({"access_token":"at-new","refresh_token":"rt-new","expires_in":3600}))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        // seed an expired oauth connection
        let now = crate::paths::now_ms();
        let mut conn = crate::llm_router::connections::ConnectionRow {
            id: "c1".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "x".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();
        refresh_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await
        .unwrap();
        assert_eq!(conn.data.access_token.as_deref(), Some("at-new"));
        assert!(conn.data.expires_at.unwrap() > now);
        // persisted
        let stored = crate::llm_router::connections::get_connection(&store, "c1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.access_token.as_deref(), Some("at-new"));
    }

    #[tokio::test]
    async fn ensure_fresh_is_noop_for_non_oauth_and_fresh_connections() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();

        // non-oauth: no-op regardless of expiry.
        let mut conn = ConnectionRow {
            id: "c2".into(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            label: "x".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                api_key: Some("sk".into()),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();
        ensure_fresh(&store, &http, &mut conn).await.unwrap();
        assert_eq!(conn.data.api_key.as_deref(), Some("sk"));

        // oauth but fresh: no-op, no network call attempted (would panic/err
        // if it tried since there's no server listening on this URL).
        let mut fresh_conn = ConnectionRow {
            id: "c3".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "y".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at".into()),
                refresh_token: Some("rt".into()),
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, fresh_conn.clone())
            .await
            .unwrap();
        ensure_fresh(&store, &http, &mut fresh_conn).await.unwrap();
        assert_eq!(fresh_conn.data.access_token.as_deref(), Some("at"));
    }

    #[tokio::test]
    async fn force_refresh_sets_needs_relogin_on_terminal_error() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(_b): Json<serde_json::Value>| async move {
                Json(serde_json::json!({"error":"invalid_grant"}))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();
        let mut conn = ConnectionRow {
            id: "c4".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "z".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let err = refresh_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("re-login required"));
        assert_eq!(conn.data.needs_relogin, Some(true));
        let stored = crate::llm_router::connections::get_connection(&store, "c4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.needs_relogin, Some(true));
    }

    #[tokio::test]
    async fn force_refresh_does_not_set_needs_relogin_on_non_allowlisted_400_error() {
        use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(_b): Json<serde_json::Value>| async move {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error":"temporarily_unavailable"})),
                )
                    .into_response()
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();
        let mut conn = ConnectionRow {
            id: "c7".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "u".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let err = refresh_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("re-login required"));
        assert_ne!(conn.data.needs_relogin, Some(true));
        let stored = crate::llm_router::connections::get_connection(&store, "c7")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(stored.data.needs_relogin, Some(true));
    }

    #[tokio::test]
    async fn refresh_keeps_old_refresh_token_when_response_omits_one() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(_b): Json<serde_json::Value>| async move {
                Json(serde_json::json!({"access_token":"at-new2","expires_in":60}))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();
        let mut conn = ConnectionRow {
            id: "c5".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "w".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-keep".into()),
                expires_at: Some(now - 1),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        refresh_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await
        .unwrap();
        assert_eq!(conn.data.access_token.as_deref(), Some("at-new2"));
        assert_eq!(conn.data.refresh_token.as_deref(), Some("rt-keep"));
    }

    #[tokio::test]
    async fn refresh_is_panic_free_on_malformed_response() {
        use axum::{routing::post, Router};
        // Server returns a non-JSON body — reqwest's `.json()` parse fails
        // and refresh_at must surface an Err, not panic.
        let app = Router::new().route("/token", post(|| async move { "not json" }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();
        let mut conn = ConnectionRow {
            id: "c6".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "v".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let result = refresh_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await;
        assert!(result.is_err());
    }

    /// The reactive (post-401) path must ALWAYS hit the network, even when
    /// the connection's own `expires_at`/`last_refresh_at` make it look
    /// fresh — a 401 already told the caller the "fresh" token was rejected
    /// (e.g. revoked server-side), so re-adopting the same stored token
    /// without a network round trip would just 401 again on retry.
    #[tokio::test]
    async fn force_refresh_hits_network_even_when_token_looks_fresh() {
        use axum::{routing::post, Json, Router};
        use std::sync::atomic::{AtomicBool, Ordering};

        let hit = std::sync::Arc::new(AtomicBool::new(false));
        let hit_for_handler = hit.clone();
        let app = Router::new().route(
            "/token",
            post(move |Json(_b): Json<serde_json::Value>| {
                let hit = hit_for_handler.clone();
                async move {
                    hit.store(true, Ordering::SeqCst);
                    Json(serde_json::json!({"access_token":"at-forced-new","refresh_token":"rt-forced-new","expires_in":3600}))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let now = crate::paths::now_ms();
        let mut conn = ConnectionRow {
            id: "c8".into(),
            provider: "anthropic-oauth".into(),
            auth_type: "oauth".into(),
            label: "forced".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                access_token: Some("at-fresh".into()),
                refresh_token: Some("rt-fresh".into()),
                // FAR in the future -> needs_refresh is false, i.e. the
                // proactive check would consider this connection fresh.
                expires_at: Some(now + 30 * 24 * 3600 * 1000),
                last_refresh_at: Some(now),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        };
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        // Sanity: this connection genuinely looks fresh under the proactive
        // policy — the bug this test guards against is force_refresh
        // re-using that same short-circuit.
        let cfg = oauth_config(&conn.provider).unwrap();
        assert!(!needs_refresh(cfg, &conn.data, crate::paths::now_ms()));

        force_refresh_with_token_url(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
        )
        .await
        .unwrap();

        assert!(
            hit.load(Ordering::SeqCst),
            "force_refresh must hit the network even when the token looks fresh"
        );
        assert_eq!(conn.data.access_token.as_deref(), Some("at-forced-new"));
        let stored = crate::llm_router::connections::get_connection(&store, "c8")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.access_token.as_deref(), Some("at-forced-new"));
    }
}
