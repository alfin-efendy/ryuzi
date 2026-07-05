//! AWS SSO-OIDC device-code flow (Kiro): register a public client, start a
//! device authorization, and poll the token endpoint until the user
//! completes the browser step. Distinct from `oauth::flow` (redirect+PKCE):
//! there's no loopback listener here, just three HTTP calls plus a
//! best-effort CodeWhisperer profile-ARN lookup used once tokens exist.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
use anyhow::{bail, Result};

use crate::llm_router::registry::DeviceFlowConfig;
use crate::paths::now_ms;

/// Public OIDC client registered for the device flow (no client secret is
/// kept private — this is the "public" client type).
#[derive(Debug, Clone)]
pub struct RegisteredClient {
    pub client_id: String,
    pub client_secret: String,
}

/// Device-authorization response: the code+URL pair shown to the user, plus
/// the poll cadence.
#[derive(Debug, Clone)]
pub struct DeviceAuth {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// Tokens returned once the user has completed the browser step.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix epoch milliseconds.
    pub expires_at: i64,
}

/// Result of a single poll of the token endpoint.
#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    /// User hasn't completed the browser step yet — keep polling.
    Pending,
    /// Server asked us to back off before the next poll.
    SlowDown,
    /// Tokens are ready.
    Ready(DeviceTokens),
    /// User declined.
    Denied,
    /// Device code expired before the user completed the flow.
    Expired,
}

/// Register a public OIDC client against the registry's configured
/// `register_url` for `cfg`.
pub async fn register_client(
    http: &reqwest::Client,
    cfg: &DeviceFlowConfig,
) -> Result<RegisteredClient> {
    register_client_at(http, cfg.register_url, cfg).await
}

/// Same as [`register_client`] but against an explicit `url` (so tests can
/// point this at a mock server).
pub async fn register_client_at(
    http: &reqwest::Client,
    url: &str,
    cfg: &DeviceFlowConfig,
) -> Result<RegisteredClient> {
    let body = serde_json::json!({
        "clientName": cfg.client_name,
        "clientType": "public",
        "scopes": cfg.scopes,
        "grantTypes": cfg.grant_types,
        "issuerUrl": cfg.issuer_url,
    });
    let resp = http.post(url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("client registration failed with status {status}: {text}");
    }
    let json: serde_json::Value = resp.json().await?;
    let client_id = json
        .get("clientId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("client registration response missing `clientId`"))?;
    let client_secret = json
        .get("clientSecret")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("client registration response missing `clientSecret`"))?;
    Ok(RegisteredClient {
        client_id,
        client_secret,
    })
}

/// Start a device authorization for the already-registered `client`, against
/// the registry's configured `device_auth_url` for `cfg`.
pub async fn start_device_authorization(
    http: &reqwest::Client,
    cfg: &DeviceFlowConfig,
    client: &RegisteredClient,
) -> Result<DeviceAuth> {
    start_device_authorization_at(http, cfg.device_auth_url, cfg, client).await
}

/// Same as [`start_device_authorization`] but against an explicit `url` (so
/// tests can point this at a mock server).
pub async fn start_device_authorization_at(
    http: &reqwest::Client,
    url: &str,
    cfg: &DeviceFlowConfig,
    client: &RegisteredClient,
) -> Result<DeviceAuth> {
    let body = serde_json::json!({
        "clientId": client.client_id,
        "clientSecret": client.client_secret,
        "startUrl": cfg.start_url,
    });
    let resp = http.post(url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("device authorization failed with status {status}: {text}");
    }
    let json: serde_json::Value = resp.json().await?;
    let device_code = json
        .get("deviceCode")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("device authorization response missing `deviceCode`"))?;
    let user_code = json
        .get("userCode")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("device authorization response missing `userCode`"))?;
    let verification_uri = json
        .get("verificationUri")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| {
            anyhow::anyhow!("device authorization response missing `verificationUri`")
        })?;
    let verification_uri_complete = json
        .get("verificationUriComplete")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| {
            anyhow::anyhow!("device authorization response missing `verificationUriComplete`")
        })?;
    let expires_in = json
        .get("expiresIn")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("device authorization response missing `expiresIn`"))?;
    let interval = json.get("interval").and_then(|v| v.as_i64()).unwrap_or(5);
    Ok(DeviceAuth {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval,
    })
}

/// Poll the token endpoint once for `device_code`. Callers loop this on
/// `cfg`'s (or the server-reported) `interval` until it returns
/// [`PollOutcome::Ready`], [`PollOutcome::Denied`], or
/// [`PollOutcome::Expired`].
pub async fn poll_token_once(
    http: &reqwest::Client,
    token_url: &str,
    client: &RegisteredClient,
    device_code: &str,
) -> Result<PollOutcome> {
    let body = serde_json::json!({
        "clientId": client.client_id,
        "clientSecret": client.client_secret,
        "deviceCode": device_code,
        "grantType": "urn:ietf:params:oauth:grant-type:device_code",
    });
    let resp = http.post(token_url).json(&body).send().await?;
    let json: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if let Some(access) = json.get("accessToken").and_then(|v| v.as_str()) {
        let expires_in = json
            .get("expiresIn")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        return Ok(PollOutcome::Ready(DeviceTokens {
            access_token: access.to_string(),
            refresh_token: json
                .get("refreshToken")
                .and_then(|v| v.as_str())
                .map(String::from),
            expires_at: now_ms() + expires_in * 1000,
        }));
    }
    match json.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => Ok(PollOutcome::Pending),
        Some("slow_down") => Ok(PollOutcome::SlowDown),
        Some("access_denied") => Ok(PollOutcome::Denied),
        Some("expired_token") => Ok(PollOutcome::Expired),
        _ => Ok(PollOutcome::Pending), // unknown transient → keep polling
    }
}

/// Best-effort lookup of the CodeWhisperer profile ARN to use for
/// completions, once `access_token` is available. Any network/parse error —
/// including a non-2xx response — is swallowed and yields `None`; the
/// connection can still work without a resolved profile ARN (some Kiro
/// accounts don't need one).
pub async fn resolve_profile_arn(http: &reqwest::Client, access_token: &str) -> Option<String> {
    let resp = http
        .post("https://codewhisperer.us-east-1.amazonaws.com/ListAvailableProfiles")
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "maxResults": 10 }))
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    first_nonempty_profile_arn(&json)
}

/// Pick the first non-empty `profiles[].arn` out of a `ListAvailableProfiles`
/// response body. Split out from [`resolve_profile_arn`] so the picking
/// logic (skip blank ARNs, don't just stop at the first entry) can be unit
/// tested without a mock HTTP server — `resolve_profile_arn`'s URL is a
/// fixed AWS endpoint, not overridable for tests.
fn first_nonempty_profile_arn(json: &serde_json::Value) -> Option<String> {
    json.get("profiles")?
        .as_array()?
        .iter()
        .find_map(|p| {
            p.get("arn")
                .and_then(|v| v.as_str())
                .filter(|arn| !arn.is_empty())
        })
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::registry::KIRO_DEVICE_FLOW;

    #[tokio::test]
    async fn register_client_at_posts_verbatim_body_and_reads_ids() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/client/register",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["clientName"], "kiro-oauth-client");
                assert_eq!(b["clientType"], "public");
                assert_eq!(
                    b["scopes"],
                    serde_json::json!([
                        "codewhisperer:completions",
                        "codewhisperer:analysis",
                        "codewhisperer:conversations"
                    ])
                );
                assert_eq!(
                    b["grantTypes"],
                    serde_json::json!([
                        "urn:ietf:params:oauth:grant-type:device_code",
                        "refresh_token"
                    ])
                );
                assert_eq!(
                    b["issuerUrl"],
                    "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6"
                );
                Json(serde_json::json!({
                    "clientId": "client-123",
                    "clientSecret": "secret-456",
                    "clientSecretExpiresAt": 9_999_999_999_i64,
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let http = reqwest::Client::new();
        let client = register_client_at(
            &http,
            &format!("http://127.0.0.1:{port}/client/register"),
            &KIRO_DEVICE_FLOW,
        )
        .await
        .unwrap();
        assert_eq!(client.client_id, "client-123");
        assert_eq!(client.client_secret, "secret-456");
    }

    #[tokio::test]
    async fn register_client_at_bails_with_body_text_on_error_status() {
        use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};
        let app =
            Router::new().route(
                "/client/register",
                post(|| async move {
                    (StatusCode::BAD_REQUEST, "boom: invalid request").into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let http = reqwest::Client::new();
        let err = register_client_at(
            &http,
            &format!("http://127.0.0.1:{port}/client/register"),
            &KIRO_DEVICE_FLOW,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("boom: invalid request"));
    }

    #[tokio::test]
    async fn start_device_authorization_at_reads_all_six_fields() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/device_authorization",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["clientId"], "client-123");
                assert_eq!(b["clientSecret"], "secret-456");
                assert_eq!(b["startUrl"], "https://view.awsapps.com/start");
                Json(serde_json::json!({
                    "deviceCode": "dc-1",
                    "userCode": "ABCD-EFGH",
                    "verificationUri": "https://device.sso.us-east-1.amazonaws.com/",
                    "verificationUriComplete": "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-EFGH",
                    "expiresIn": 600,
                    // interval omitted -> should default to 5
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let http = reqwest::Client::new();
        let client = RegisteredClient {
            client_id: "client-123".into(),
            client_secret: "secret-456".into(),
        };
        let auth = start_device_authorization_at(
            &http,
            &format!("http://127.0.0.1:{port}/device_authorization"),
            &KIRO_DEVICE_FLOW,
            &client,
        )
        .await
        .unwrap();
        assert_eq!(auth.device_code, "dc-1");
        assert_eq!(auth.user_code, "ABCD-EFGH");
        assert_eq!(
            auth.verification_uri,
            "https://device.sso.us-east-1.amazonaws.com/"
        );
        assert_eq!(
            auth.verification_uri_complete,
            "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-EFGH"
        );
        assert_eq!(auth.expires_in, 600);
        assert_eq!(auth.interval, 5);
    }

    #[tokio::test]
    async fn start_device_authorization_at_reads_explicit_interval_when_present() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/device_authorization",
            post(|Json(_b): Json<serde_json::Value>| async move {
                Json(serde_json::json!({
                    "deviceCode": "dc-2",
                    "userCode": "WXYZ-1234",
                    "verificationUri": "https://device.sso.us-east-1.amazonaws.com/",
                    "verificationUriComplete": "https://device.sso.us-east-1.amazonaws.com/?user_code=WXYZ-1234",
                    "expiresIn": 300,
                    "interval": 10,
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let http = reqwest::Client::new();
        let client = RegisteredClient {
            client_id: "client-123".into(),
            client_secret: "secret-456".into(),
        };
        let auth = start_device_authorization_at(
            &http,
            &format!("http://127.0.0.1:{port}/device_authorization"),
            &KIRO_DEVICE_FLOW,
            &client,
        )
        .await
        .unwrap();
        assert_eq!(auth.interval, 10);
    }

    /// Mock `/token` that pops one queued response body per request, in
    /// order — lets a single test drive `poll_token_once` through a
    /// sequence of outcomes against one server.
    async fn spawn_queued_token_server(
        bodies: Vec<serde_json::Value>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use axum::{routing::post, Json, Router};
        let queue = std::sync::Arc::new(std::sync::Mutex::new(bodies));
        let queue_for_handler = queue.clone();
        let app = Router::new().route(
            "/token",
            post(move |Json(b): Json<serde_json::Value>| {
                let queue = queue_for_handler.clone();
                async move {
                    assert_eq!(
                        b["grantType"],
                        "urn:ietf:params:oauth:grant-type:device_code"
                    );
                    let mut q = queue.lock().unwrap();
                    let body = if q.is_empty() {
                        serde_json::json!({})
                    } else {
                        q.remove(0)
                    };
                    Json(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}/token"), queue)
    }

    #[tokio::test]
    async fn poll_maps_outcomes() {
        let (url, _queue) = spawn_queued_token_server(vec![
            serde_json::json!({"error": "authorization_pending"}),
            serde_json::json!({"error": "slow_down"}),
            serde_json::json!({"error": "access_denied"}),
            serde_json::json!({"error": "expired_token"}),
            serde_json::json!({
                "accessToken": "at-1",
                "refreshToken": "rt-1",
                "expiresIn": 3600,
            }),
        ])
        .await;
        let http = reqwest::Client::new();
        let client = RegisteredClient {
            client_id: "client-123".into(),
            client_secret: "secret-456".into(),
        };

        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        assert_eq!(outcome, PollOutcome::Pending);

        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        assert_eq!(outcome, PollOutcome::SlowDown);

        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        assert_eq!(outcome, PollOutcome::Denied);

        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        assert_eq!(outcome, PollOutcome::Expired);

        let before = now_ms();
        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        match outcome {
            PollOutcome::Ready(tokens) => {
                assert_eq!(tokens.access_token, "at-1");
                assert_eq!(tokens.refresh_token.as_deref(), Some("rt-1"));
                let expected = before + 3600 * 1000;
                assert!(
                    (tokens.expires_at - expected).abs() < 1000,
                    "expires_at {} should be close to {}",
                    tokens.expires_at,
                    expected
                );
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn poll_maps_unknown_error_to_pending() {
        let (url, _queue) =
            spawn_queued_token_server(vec![serde_json::json!({"error": "something_else"})]).await;
        let http = reqwest::Client::new();
        let client = RegisteredClient {
            client_id: "client-123".into(),
            client_secret: "secret-456".into(),
        };
        let outcome = poll_token_once(&http, &url, &client, "dc-1").await.unwrap();
        assert_eq!(outcome, PollOutcome::Pending);
    }

    #[test]
    fn first_nonempty_profile_arn_skips_blank_and_picks_first_nonempty() {
        let json = serde_json::json!({
            "profiles": [
                {"arn": ""},
                {"arn": "arn:aws:codewhisperer:us-east-1:123:profile/abc"},
                {"arn": "arn:aws:codewhisperer:us-east-1:123:profile/def"},
            ]
        });
        assert_eq!(
            first_nonempty_profile_arn(&json),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/abc".to_string())
        );
    }

    #[test]
    fn first_nonempty_profile_arn_none_when_missing_empty_or_all_blank() {
        assert_eq!(first_nonempty_profile_arn(&serde_json::json!({})), None);
        assert_eq!(
            first_nonempty_profile_arn(&serde_json::json!({"profiles": []})),
            None
        );
        assert_eq!(
            first_nonempty_profile_arn(&serde_json::json!({"profiles": [{"arn": ""}]})),
            None
        );
    }

    #[tokio::test]
    async fn resolve_profile_arn_returns_none_on_network_error() {
        // No server listening on this port -> connection error -> None.
        let http = reqwest::Client::new();
        let result = resolve_profile_arn(&http, "at-1").await;
        assert!(result.is_none());
    }
}
