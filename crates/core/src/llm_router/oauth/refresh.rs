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
    self, get_connection, is_oauth, update_connection, ConnectionData, ConnectionRow,
};
use crate::llm_router::registry::{self, oauth_config};
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

/// Resolve the proactive refresh lead (ms) for `provider` generically:
/// redirect-based OAuth providers (anthropic/openai) carry it on their
/// `OAuthConfig`; device-flow providers (kiro) carry it on their
/// `DeviceFlowConfig` instead — kiro has no `OAuthConfig` at all. Falls back
/// to a 5-minute buffer if neither is found.
fn refresh_lead_ms(provider: &str) -> i64 {
    if let Some(cfg) = registry::oauth_config(provider) {
        return cfg.refresh_lead_ms;
    }
    if let Some(df) = registry::device_flow_config(provider) {
        return df.refresh_lead_ms;
    }
    300_000
}

/// True if `data` needs a refresh before it can be used: expiry is within
/// the provider's refresh lead (see [`refresh_lead_ms`]) of `now` (or already
/// missing/expired), or the token has aged past `max_refresh_age_ms` since
/// its last refresh (if the provider enforces a max age — device-flow
/// providers like kiro don't have one, so that clause is skipped for them).
pub fn needs_refresh(provider: &str, data: &ConnectionData, now_ms: i64) -> bool {
    let lead = refresh_lead_ms(provider);
    let expiry_due = match data.expires_at {
        Some(exp) => exp - now_ms < lead,
        None => true,
    };
    if expiry_due {
        return true;
    }
    if let Some(max_age) = oauth_config(provider).and_then(|cfg| cfg.max_refresh_age_ms) {
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
    if !needs_refresh(&conn.provider, &conn.data, now_ms()) {
        return Ok(());
    }
    // kiro has no `OAuthConfig`/static token_url — `refresh_at_impl` dispatches
    // to `refresh_kiro`, which resolves its own endpoint(s) internally, so the
    // token_url passed through here is unused for it.
    let token_url = oauth_config(&conn.provider)
        .map(|cfg| cfg.token_url)
        .unwrap_or_default();
    refresh_at(store, http, conn, token_url).await
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
    // See `ensure_fresh`: kiro has no static token_url — `refresh_at_impl`
    // dispatches to `refresh_kiro` before this would-be-unused URL matters.
    let token_url = oauth_config(&conn.provider)
        .map(|cfg| cfg.token_url)
        .unwrap_or_default();
    force_refresh_with_token_url(store, http, conn, token_url).await
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
        let stale = needs_refresh(&conn.provider, &latest.data, now_ms());
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

    if conn.provider == "kiro" {
        return refresh_kiro(store, http, conn, token_url).await;
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

/// Kiro (free tier) token refresh. Ported from 9router (MIT, (c) 2024-2026
/// decolua and contributors). Dispatches to the AWS SSO-OIDC endpoint for
/// builder-id/idc connections (which carry `clientId`/`clientSecret` in
/// `provider_specific`), or the kiro.dev social endpoint otherwise
/// (google/github/imported — no client creds). A connection with no refresh
/// token on file (api_key auth) has nothing to refresh, so this is a no-op.
/// Delegates to [`refresh_kiro_at`] with the production URLs, UNLESS
/// `token_url_override` is non-empty — the router's per-`AppState`
/// `oauth_token_url_override` test seam (threaded through
/// `force_refresh_with_token_url` -> `refresh_at_impl`), which points BOTH
/// the AWS and social refresh URLs at the same mock endpoint so a 403-retry
/// test can drive the real refresh code path. Production always calls this
/// with an empty string (kiro has no `OAuthConfig`, so `ensure_fresh`/
/// `force_refresh` resolve an empty default token_url for it), so real
/// traffic is unaffected.
async fn refresh_kiro(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
    token_url_override: &str,
) -> Result<()> {
    if token_url_override.is_empty() {
        refresh_kiro_at(
            store,
            http,
            conn,
            "https://oidc.us-east-1.amazonaws.com/token",
            "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken",
        )
        .await
    } else {
        refresh_kiro_at(store, http, conn, token_url_override, token_url_override).await
    }
}

/// Same as [`refresh_kiro`] but against explicit AWS/social token URLs (so
/// tests can point them at mock axum servers instead of Kiro's real
/// endpoints). Ported from 9router (MIT, (c) 2024-2026 decolua and
/// contributors).
async fn refresh_kiro_at(
    store: &Arc<Store>,
    http: &reqwest::Client,
    conn: &mut ConnectionRow,
    aws_token_url: &str,
    social_url: &str,
) -> Result<()> {
    let Some(refresh_token) = conn.data.refresh_token.clone() else {
        return Ok(()); // api_key / no refresh token: nothing to do.
    };
    let auth_method = connections::kiro_auth_method(&conn.data);
    let (url, body, headers): (String, serde_json::Value, Vec<(&str, String)>) =
        if let Some((client_id, client_secret)) = connections::kiro_client_creds(&conn.data) {
            // builder-id / idc (AWS SSO-OIDC), camelCase JSON.
            let region = connections::kiro_region(&conn.data);
            let url = if auth_method == "idc" && region != "us-east-1" {
                format!("https://oidc.{region}.amazonaws.com/token")
            } else {
                aws_token_url.to_string()
            };
            (
                url,
                serde_json::json!({
                    "clientId": client_id,
                    "clientSecret": client_secret,
                    "refreshToken": refresh_token,
                    "grantType": "refresh_token",
                }),
                vec![],
            )
        } else {
            // social (google/github/imported): no client creds, kiro-cli UA.
            (
                social_url.to_string(),
                serde_json::json!({ "refreshToken": refresh_token }),
                vec![("user-agent", "kiro-cli/1.0.0".to_string())],
            )
        };

    let mut req = http
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&body);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));

    if !status.is_success() {
        conn.data.needs_relogin = Some(true);
        persist(store, conn).await?;
        anyhow::bail!("kiro refresh failed: {status}");
    }
    let Some(access) = json.get("accessToken").and_then(|v| v.as_str()) else {
        conn.data.needs_relogin = Some(true);
        persist(store, conn).await?;
        anyhow::bail!("kiro refresh response missing accessToken");
    };

    let expires_in = json
        .get("expiresIn")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    conn.data.access_token = Some(access.to_string());
    if let Some(rt) = json.get("refreshToken").and_then(|v| v.as_str()) {
        conn.data.refresh_token = Some(rt.to_string());
    }
    if let Some(arn) = json
        .get("profileArn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        merge_provider_specific(&mut conn.data, "profileArn", serde_json::json!(arn));
    }
    conn.data.expires_at = Some(now_ms() + expires_in * 1000);
    conn.data.last_refresh_at = Some(now_ms());
    conn.data.needs_relogin = Some(false);
    persist(store, conn).await
}

/// Insert `val` under `key` into `data.provider_specific`, creating the JSON
/// object if it doesn't exist yet instead of clobbering any existing keys.
fn merge_provider_specific(data: &mut ConnectionData, key: &str, val: serde_json::Value) {
    let obj = data
        .provider_specific
        .get_or_insert_with(|| serde_json::json!({}));
    if let Some(map) = obj.as_object_mut() {
        map.insert(key.to_string(), val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::ConnectionData;

    #[test]
    fn needs_refresh_respects_lead_and_max_age() {
        // openai-oauth: lead 5d, max age 8d.
        let now = 10_000_000_000_i64;
        let mut d = ConnectionData {
            expires_at: Some(now + 6 * 24 * 3600 * 1000), // 6d out > 5d lead -> fresh...
            last_refresh_at: Some(now - 1000),
            ..Default::default()
        };
        assert!(!needs_refresh("openai-oauth", &d, now));
        d.expires_at = Some(now + 4 * 24 * 3600 * 1000); // 4d < 5d lead -> refresh
        assert!(needs_refresh("openai-oauth", &d, now));
        // max-age forces refresh even if expiry is far
        d.expires_at = Some(now + 100 * 24 * 3600 * 1000);
        d.last_refresh_at = Some(now - 9 * 24 * 3600 * 1000); // 9d > 8d max
        assert!(needs_refresh("openai-oauth", &d, now));
    }

    #[test]
    fn needs_refresh_uses_device_flow_lead_for_kiro() {
        // kiro has no `OAuthConfig` — the lead must come from
        // `device_flow_config("kiro").refresh_lead_ms` (300_000ms), and it
        // must NOT apply any `max_refresh_age_ms` clause (device flow has
        // none), even when `last_refresh_at` is very old.
        let now = 10_000_000_000_i64;
        let mut d = ConnectionData {
            expires_at: Some(now + 400_000), // 400s out > 300s lead -> fresh
            last_refresh_at: Some(now - 1000),
            ..Default::default()
        };
        assert!(!needs_refresh("kiro", &d, now));
        d.expires_at = Some(now + 200_000); // 200s < 300s lead -> refresh due
        assert!(needs_refresh("kiro", &d, now));
        // Far-out expiry + a very old last_refresh_at must NOT force a
        // refresh for kiro (no max_refresh_age_ms clause to trip).
        d.expires_at = Some(now + 100 * 24 * 3600 * 1000);
        d.last_refresh_at = Some(now - 365 * 24 * 3600 * 1000);
        assert!(!needs_refresh("kiro", &d, now));
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
        assert!(!needs_refresh(
            &conn.provider,
            &conn.data,
            crate::paths::now_ms()
        ));

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

    fn kiro_conn(id: &str, data: ConnectionData) -> ConnectionRow {
        let now = crate::paths::now_ms();
        ConnectionRow {
            id: id.into(),
            provider: "kiro".into(),
            auth_type: "oauth".into(),
            label: "kiro".into(),
            priority: 0,
            enabled: true,
            data,
            created_at: now,
            updated_at: now,
        }
    }

    /// (a) builder-id/idc connection with client creds refreshes against the
    /// AWS SSO-OIDC mock, sending the exact camelCase body, and adopts the
    /// new access token / expiry / refresh token from the response.
    #[tokio::test]
    async fn refresh_kiro_builder_id_refreshes_via_aws_sso_oidc() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["clientId"], "cid-1");
                assert_eq!(b["clientSecret"], "secret-1");
                assert_eq!(b["refreshToken"], "rt-old");
                assert_eq!(b["grantType"], "refresh_token");
                Json(
                    serde_json::json!({"accessToken":"at-new","refreshToken":"rt-new","expiresIn":1800}),
                )
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
        let mut conn = kiro_conn(
            "kiro-1",
            ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                provider_specific: Some(serde_json::json!({
                    "authMethod": "builder-id",
                    "clientId": "cid-1",
                    "clientSecret": "secret-1",
                })),
                ..Default::default()
            },
        );
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let before = crate::paths::now_ms();
        refresh_kiro_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
            "http://127.0.0.1:1/unused-social",
        )
        .await
        .unwrap();

        assert_eq!(conn.data.access_token.as_deref(), Some("at-new"));
        assert_eq!(conn.data.refresh_token.as_deref(), Some("rt-new"));
        assert_eq!(conn.data.needs_relogin, Some(false));
        let expires_at = conn.data.expires_at.expect("expires_at set");
        assert!(expires_at >= before + 1800 * 1000);
        assert!(expires_at <= before + 1800 * 1000 + 5000);
        let stored = crate::llm_router::connections::get_connection(&store, "kiro-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.access_token.as_deref(), Some("at-new"));
    }

    /// (b) social connection (no client creds — e.g. google/github) refreshes
    /// against the kiro.dev mock with the kiro-cli User-Agent, and preserves
    /// the OLD refresh token when the response omits `refreshToken`.
    #[tokio::test]
    async fn refresh_kiro_social_preserves_old_refresh_token_when_response_omits_one() {
        use axum::{extract::Json, http::HeaderMap, routing::post, Router};
        let app = Router::new().route(
            "/refreshToken",
            post(
                |headers: HeaderMap, Json(b): Json<serde_json::Value>| async move {
                    assert_eq!(b["refreshToken"], "rt-keep");
                    assert!(b.get("clientId").is_none());
                    assert_eq!(
                        headers.get("user-agent").and_then(|v| v.to_str().ok()),
                        Some("kiro-cli/1.0.0")
                    );
                    Json(serde_json::json!({"accessToken":"at-social-new","expiresIn":3600}))
                },
            ),
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
        let mut conn = kiro_conn(
            "kiro-2",
            ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-keep".into()),
                expires_at: Some(now - 1),
                provider_specific: Some(serde_json::json!({ "authMethod": "google" })),
                ..Default::default()
            },
        );
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        refresh_kiro_at(
            &store,
            &http,
            &mut conn,
            "http://127.0.0.1:1/unused-aws",
            &format!("http://127.0.0.1:{port}/refreshToken"),
        )
        .await
        .unwrap();

        assert_eq!(conn.data.access_token.as_deref(), Some("at-social-new"));
        // old refresh token preserved since the mock response omitted one.
        assert_eq!(conn.data.refresh_token.as_deref(), Some("rt-keep"));
        assert_eq!(conn.data.needs_relogin, Some(false));
    }

    /// (c) a non-2xx response from the token endpoint is terminal: sets
    /// `needs_relogin`, persists it, and returns an `Err`.
    #[tokio::test]
    async fn refresh_kiro_sets_needs_relogin_on_non_2xx() {
        use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
        let app = Router::new().route(
            "/token",
            post(|Json(_b): Json<serde_json::Value>| async move {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":"boom"})),
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
        let mut conn = kiro_conn(
            "kiro-3",
            ConnectionData {
                access_token: Some("at-old".into()),
                refresh_token: Some("rt-old".into()),
                expires_at: Some(now - 1),
                provider_specific: Some(serde_json::json!({
                    "authMethod": "builder-id",
                    "clientId": "cid-3",
                    "clientSecret": "secret-3",
                })),
                ..Default::default()
            },
        );
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let err = refresh_kiro_at(
            &store,
            &http,
            &mut conn,
            &format!("http://127.0.0.1:{port}/token"),
            "http://127.0.0.1:1/unused-social",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("kiro refresh failed"));
        assert_eq!(conn.data.needs_relogin, Some(true));
        let stored = crate::llm_router::connections::get_connection(&store, "kiro-3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.needs_relogin, Some(true));
    }

    /// A kiro connection with no refresh token (api_key auth) has nothing to
    /// refresh — `refresh_kiro_at` must be a pure no-op, never touching the
    /// network (both URLs point at nothing listening).
    #[tokio::test]
    async fn refresh_kiro_is_noop_when_no_refresh_token() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let mut conn = kiro_conn(
            "kiro-4",
            ConnectionData {
                access_token: Some("at-only".into()),
                refresh_token: None,
                ..Default::default()
            },
        );
        crate::llm_router::connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        refresh_kiro_at(
            &store,
            &http,
            &mut conn,
            "http://127.0.0.1:1/unused-aws",
            "http://127.0.0.1:1/unused-social",
        )
        .await
        .unwrap();

        assert_eq!(conn.data.access_token.as_deref(), Some("at-only"));
        assert_eq!(conn.data.needs_relogin, None);
    }
}
