//! Host-side OAuth *profile* logic (Task 8 slice 2a). A plugin bundle may
//! declare more than one `[[oauth]]` profile (see
//! `ryuzi_plugin_sdk::OAuthProfile`), so every operation here is keyed by
//! `(ctx.plugin_id, profile_id)` against the profile-scoped tables added in
//! `store.rs` migration 44 (`plugin_oauth_profile_tokens` /
//! `plugin_oauth_profile_clients`).
//!
//! # A component never sees the raw token
//! [`ProfileOauth::begin_pkce`] returns the PKCE verifier and authorize URL
//! to the **host caller** (Cockpit), not to the guest component — this
//! module has no WIT-facing entry point yet (that wiring is slice 2b).
//! [`ProfileOauth::authorized_request`] looks up the stored access token and
//! injects it into the outbound request host-side via
//! [`super::http::AllowedHttpClient::request_with_bearer`], which strips any
//! component-supplied `Authorization` header before adding the host's
//! bearer last — see that method's doc for the exact ordering guarantee.
//! The component only ever receives the upstream [`SafeHttpResponse`]; the
//! access token itself never crosses back into adapter-returned data.

use super::http::{AllowedHttpClient, SafeHttpResponse};
use super::PluginCapabilityContext;
use crate::plugins::oauth::{generate_pkce_verifier, needs_refresh, pkce_challenge_s256};
use ryuzi_plugin_sdk::OAuthProfile;

/// A capability-adapter-local error, mapped to the generated WIT
/// `OauthError` variants by the runtime's `Host` trait impl (slice 2b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OauthErr {
    InvalidRequest(String),
    Denied,
    Expired,
    Failed(String),
}

/// The result of [`ProfileOauth::begin_pkce`]: everything the host caller
/// needs to send the user to the authorize URL and later exchange the
/// returned `code` for a token. `verifier` must be handed back to the token
/// exchange step and must never be persisted to durable telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceStart {
    pub authorize_url: String,
    pub state: String,
    pub verifier: String,
}

/// The result of [`ProfileOauth::begin_device_flow`]: everything the host
/// caller needs to show the user a code and a place to enter it, plus the
/// poll cadence. `user_code` is shown to the user once and must never be
/// written to durable telemetry or logs — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFlowStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    /// Minimum seconds between polls; defaults to 5 when the server omits
    /// `interval` (RFC 8628 §3.2).
    pub interval_secs: u64,
    /// `now_ms()` at request time plus `expires_in * 1000`.
    pub expires_at: i64,
}

/// The result of a single [`ProfileOauth::poll_device_flow`] call. The
/// caller drives the poll loop (sleeping `interval_secs` between calls,
/// growing it on `SlowDown`) — see that method's doc for why polling is one
/// call at a time rather than a built-in loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePollOutcome {
    /// The user hasn't completed the browser step yet — keep polling.
    Pending,
    /// The server asked for a slower poll cadence — increase the interval
    /// and keep polling.
    SlowDown,
    /// A token was issued and has been persisted as `profile`'s token.
    Ready,
    /// The device code expired before the user completed the flow.
    Expired,
    /// The user declined the authorization request.
    Denied,
}

/// One plugin's view of one of its declared OAuth profiles.
pub struct ProfileOauth<'a> {
    pub ctx: &'a PluginCapabilityContext,
}

impl<'a> ProfileOauth<'a> {
    pub fn new(ctx: &'a PluginCapabilityContext) -> Self {
        Self { ctx }
    }

    /// Resolves the OAuth client id for `profile`: prefer the plugin's own
    /// setting named by `client_id_setting`, then the cached profile client
    /// row (filled by discovery/DCR), else `InvalidRequest`.
    async fn resolve_client_id(&self, profile: &OAuthProfile) -> Result<String, OauthErr> {
        if let Some(setting) = &profile.client_id_setting {
            if let Ok(Some(value)) = self.ctx.settings.get(setting).await {
                if !value.is_empty() {
                    return Ok(value);
                }
            }
        }
        if let Ok(Some(client)) = self
            .ctx
            .store
            .get_plugin_oauth_profile_client(&self.ctx.plugin_id, &profile.id)
            .await
        {
            if let Some(client_id) = client.client_id {
                return Ok(client_id);
            }
        }
        Err(OauthErr::InvalidRequest("missing client id".to_string()))
    }

    /// Builds a PKCE (S256) authorize URL for `profile`. Generates a fresh
    /// verifier/challenge and state on every call, so two calls for the same
    /// profile never share a verifier or state.
    pub async fn begin_pkce(
        &self,
        profile: &OAuthProfile,
        redirect_uri: &str,
    ) -> Result<PkceStart, OauthErr> {
        let authorize_url = profile
            .authorize_url
            .as_deref()
            .ok_or_else(|| OauthErr::InvalidRequest("profile has no authorize_url".to_string()))?;
        let client_id = self.resolve_client_id(profile).await?;

        let verifier = generate_pkce_verifier();
        let challenge = pkce_challenge_s256(&verifier);
        let state = generate_pkce_verifier();

        let mut url = url::Url::parse(authorize_url)
            .map_err(|error| OauthErr::InvalidRequest(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &profile.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");

        Ok(PkceStart {
            authorize_url: url.to_string(),
            state,
            verifier,
        })
    }

    /// Issues one HTTP request against `url` authenticated with the stored
    /// token for `profile_id`, without ever exposing the token to the
    /// caller. See the module doc for exactly how the bearer is injected.
    ///
    /// Refresh is out of scope for this slice: an expired token with no
    /// further handling here simply reports [`OauthErr::Expired`] (refresh
    /// lands in slice 2b).
    pub async fn authorized_request(
        &self,
        profile_id: &str,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        allowlist: Vec<String>,
    ) -> Result<SafeHttpResponse, OauthErr> {
        let token = self
            .ctx
            .store
            .get_plugin_oauth_profile_token(&self.ctx.plugin_id, profile_id)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?
            .ok_or(OauthErr::Denied)?;

        if needs_refresh(now_ms(), token.expires_at) {
            return Err(OauthErr::Expired);
        }

        let client = AllowedHttpClient::new(allowlist);
        client
            .request_with_bearer(method, url, headers, body, &token.access_token)
            .await
            .map_err(|error| OauthErr::Failed(format!("{error:?}")))
    }

    /// Revokes the local record of `profile_id`'s token — a subsequent
    /// [`Self::authorized_request`] for the same profile then returns
    /// [`OauthErr::Denied`]. Does not attempt vendor-side token revocation
    /// (out of scope for this slice).
    pub async fn disconnect_profile(&self, profile_id: &str) -> Result<(), OauthErr> {
        self.ctx
            .store
            .delete_plugin_oauth_profile_token(&self.ctx.plugin_id, profile_id)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))
    }

    /// Starts an RFC 8628 device-authorization flow for `profile` against
    /// `device_authorization_url`. The caller (Cockpit / the setup wizard)
    /// supplies that URL explicitly — `OAuthProfile` has no dedicated field
    /// for it, since device-authorization endpoints vary by provider and
    /// discovery is out of scope for this slice.
    ///
    /// The returned `user_code` is meant to be shown to the user once and
    /// must never be written to durable telemetry or logs; this method
    /// itself never logs it or puts it in an [`OauthErr`].
    pub async fn begin_device_flow(
        &self,
        profile: &OAuthProfile,
        device_authorization_url: &str,
    ) -> Result<DeviceFlowStart, OauthErr> {
        let client_id = self.resolve_client_id(profile).await?;

        let mut form: Vec<(&str, String)> = vec![("client_id", client_id)];
        let scope = profile.scopes.join(" ");
        if !scope.is_empty() {
            form.push(("scope", scope));
        }

        let response = reqwest::Client::new()
            .post(device_authorization_url)
            .form(&form)
            .send()
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        if !status.is_success() {
            return Err(OauthErr::Failed(format!(
                "device authorization failed with status {status}"
            )));
        }

        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| OauthErr::Failed(error.to_string()))?;
        let device_code = json
            .get("device_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OauthErr::Failed("device authorization response missing `device_code`".into())
            })?
            .to_string();
        let user_code = json
            .get("user_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OauthErr::Failed("device authorization response missing `user_code`".into())
            })?
            .to_string();
        let verification_uri = json
            .get("verification_uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OauthErr::Failed("device authorization response missing `verification_uri`".into())
            })?
            .to_string();
        let verification_uri_complete = json
            .get("verification_uri_complete")
            .and_then(|v| v.as_str())
            .map(String::from);
        let expires_in = json
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                OauthErr::Failed("device authorization response missing `expires_in`".into())
            })?;
        let interval_secs = json.get("interval").and_then(|v| v.as_u64()).unwrap_or(5);

        Ok(DeviceFlowStart {
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
            interval_secs,
            expires_at: now_ms() + (expires_in as i64) * 1000,
        })
    }

    /// Polls the token endpoint once for `device_code`. This is a single
    /// poll, not a loop: the caller sleeps `interval_secs` (from
    /// [`DeviceFlowStart`]) between calls, grows the interval on
    /// [`DevicePollOutcome::SlowDown`], and simply stops calling to cancel —
    /// there is no cancellation token to thread through, and the method
    /// stays trivial to unit test one poll at a time.
    ///
    /// Checks `expires_at` before issuing any request, so a poll after
    /// expiry never reaches the network.
    ///
    /// On [`DevicePollOutcome::Ready`] the issued token has already been
    /// persisted as `profile`'s token via
    /// `Store::upsert_plugin_oauth_profile_token`, reusing the same
    /// encrypt-on-write path as [`Self::begin_pkce`]'s token exchange.
    pub async fn poll_device_flow(
        &self,
        profile: &OAuthProfile,
        token_url: &str,
        device_code: &str,
        expires_at: i64,
    ) -> Result<DevicePollOutcome, OauthErr> {
        if now_ms() > expires_at {
            return Ok(DevicePollOutcome::Expired);
        }

        let client_id = self.resolve_client_id(profile).await?;

        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", client_id.as_str()),
        ];
        let response = reqwest::Client::new()
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        let body = response
            .text()
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| OauthErr::Failed(error.to_string()))?;

        if let Some(error) = json.get("error").and_then(|v| v.as_str()) {
            return match error {
                "authorization_pending" => Ok(DevicePollOutcome::Pending),
                "slow_down" => Ok(DevicePollOutcome::SlowDown),
                "expired_token" => Ok(DevicePollOutcome::Expired),
                "access_denied" => Ok(DevicePollOutcome::Denied),
                _ => Err(OauthErr::Failed("device token request failed".to_string())),
            };
        }

        let access_token = match json.get("access_token").and_then(|v| v.as_str()) {
            Some(token) => token.to_string(),
            None => {
                return Err(OauthErr::Failed(
                    "token response has neither `error` nor `access_token`".into(),
                ))
            }
        };
        let refresh_token = json
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(String::from);
        let token_type = json
            .get("token_type")
            .and_then(|v| v.as_str())
            .unwrap_or("Bearer")
            .to_string();
        let expires_at = json
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .map(|secs| now_ms() + (secs as i64) * 1000);

        let token = crate::plugins::oauth::PluginOauthToken {
            plugin_id: self.ctx.plugin_id.clone(),
            access_token,
            refresh_token,
            token_type,
            expires_at,
            scopes: profile.scopes.clone(),
            reconnect_required: false,
        };
        self.ctx
            .store
            .upsert_plugin_oauth_profile_token(&self.ctx.plugin_id, &profile.id, &token)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;

        Ok(DevicePollOutcome::Ready)
    }
}

fn now_ms() -> i64 {
    crate::paths::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::oauth::PluginOauthToken;
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use crate::telemetry::NoopTelemetry;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use std::sync::Arc;

    async fn open_test_store() -> (Arc<Store>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        (Arc::new(store), tmp)
    }

    fn ctx_for(store: Arc<Store>, plugin_id: &str) -> PluginCapabilityContext {
        PluginCapabilityContext {
            plugin_id: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            settings: SettingsStore::new(store.clone()),
            store,
            telemetry: Arc::new(NoopTelemetry),
        }
    }

    fn test_profile(id: &str) -> OAuthProfile {
        OAuthProfile {
            id: id.to_string(),
            authorize_url: Some("https://example.test/authorize".to_string()),
            token_url: Some("https://example.test/token".to_string()),
            scopes: vec!["repo".to_string(), "issues:read".to_string()],
            client_id_setting: None,
            client_secret_setting: None,
            resource: None,
            dynamic_registration: false,
        }
    }

    async fn spawn_server(app: Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn begin_pkce_builds_a_well_formed_authorize_url() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let start = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await
            .unwrap();

        assert!(!start.verifier.is_empty());
        assert!(start.authorize_url.contains("code_challenge_method=S256"));
        assert!(start
            .authorize_url
            .contains(&format!("state={}", start.state)));
        assert!(start.authorize_url.contains("client_id=client-abc"));
        assert!(start
            .authorize_url
            .starts_with("https://example.test/authorize?"));
    }

    #[tokio::test]
    async fn begin_pkce_yields_different_verifiers_and_state_each_call() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let first = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await
            .unwrap();
        let second = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await
            .unwrap();

        assert_ne!(first.verifier, second.verifier);
        assert_ne!(first.state, second.state);
    }

    #[tokio::test]
    async fn begin_pkce_without_a_resolvable_client_id_is_invalid_request() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let result = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await;
        assert!(matches!(result, Err(OauthErr::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn authorized_request_with_no_stored_token_is_denied() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let result = oauth
            .authorized_request(
                "default",
                "GET",
                "https://example.test/me",
                vec![],
                None,
                vec!["example.test".to_string()],
            )
            .await;
        assert_eq!(result, Err(OauthErr::Denied));
    }

    #[tokio::test]
    async fn authorized_request_injects_the_host_token_and_overrides_a_forged_one() {
        let seen_bearer: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let handler_seen = seen_bearer.clone();
        async fn echo_auth(
            State(seen): State<Arc<std::sync::Mutex<Option<String>>>>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            let value = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            *seen.lock().unwrap() = value.clone();
            value.unwrap_or_default()
        }
        let app = Router::new()
            .route("/me", get(echo_auth))
            .with_state(handler_seen);
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store.clone(), "github");
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "default",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "real-host-token".into(),
                    refresh_token: None,
                    token_type: "Bearer".into(),
                    expires_at: Some(crate::paths::now_ms() + 3_600_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();

        let oauth = ProfileOauth::new(&ctx);
        let response = oauth
            .authorized_request(
                "default",
                "GET",
                &format!("http://127.0.0.1:{port}/me"),
                vec![("Authorization".to_string(), "Bearer sneaky".to_string())],
                None,
                vec!["127.0.0.1".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(response.body, b"Bearer real-host-token");
        assert_eq!(
            *seen_bearer.lock().unwrap(),
            Some("Bearer real-host-token".to_string())
        );
        // Never surfaced anywhere else in the response.
        let body_text = String::from_utf8_lossy(&response.body);
        assert!(!body_text.contains("sneaky"));
    }

    #[tokio::test]
    async fn authorized_request_is_isolated_by_plugin_id() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "p1",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "github-token".into(),
                    refresh_token: None,
                    token_type: "Bearer".into(),
                    expires_at: Some(crate::paths::now_ms() + 3_600_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();

        let other_ctx = ctx_for(store, "atlassian");
        let oauth = ProfileOauth::new(&other_ctx);
        let result = oauth
            .authorized_request(
                "p1",
                "GET",
                "https://example.test/me",
                vec![],
                None,
                vec!["example.test".to_string()],
            )
            .await;
        assert_eq!(
            result,
            Err(OauthErr::Denied),
            "a different plugin id must not reach another plugin's profile token"
        );
    }

    #[tokio::test]
    async fn disconnect_profile_deletes_the_token_and_a_later_request_is_denied() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "default",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "real-host-token".into(),
                    refresh_token: None,
                    token_type: "Bearer".into(),
                    expires_at: Some(crate::paths::now_ms() + 3_600_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        oauth.disconnect_profile("default").await.unwrap();

        let result = oauth
            .authorized_request(
                "default",
                "GET",
                "https://example.test/me",
                vec![],
                None,
                vec!["example.test".to_string()],
            )
            .await;
        assert_eq!(result, Err(OauthErr::Denied));
    }

    #[tokio::test]
    async fn authorized_request_with_an_expired_token_is_expired() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "default",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "real-host-token".into(),
                    refresh_token: None,
                    token_type: "Bearer".into(),
                    // Already past — `needs_refresh` returns true, and with no
                    // refresh token the adapter cannot renew it.
                    expires_at: Some(crate::paths::now_ms() - 3_600_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let result = oauth
            .authorized_request(
                "default",
                "GET",
                "https://example.test/me",
                vec![],
                None,
                vec!["example.test".to_string()],
            )
            .await;
        assert_eq!(result, Err(OauthErr::Expired));
    }

    #[tokio::test]
    async fn begin_device_flow_returns_the_server_fields() {
        use axum::{routing::post, Form, Json};
        use std::collections::HashMap;

        let app = Router::new().route(
            "/device_authorization",
            post(|Form(form): Form<HashMap<String, String>>| async move {
                assert_eq!(
                    form.get("client_id").map(String::as_str),
                    Some("client-abc")
                );
                assert_eq!(
                    form.get("scope").map(String::as_str),
                    Some("repo issues:read")
                );
                Json(serde_json::json!({
                    "device_code": "dc-123",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://example.test/device",
                    "verification_uri_complete": "https://example.test/device?user_code=ABCD-EFGH",
                    "expires_in": 600,
                    "interval": 7,
                }))
            }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let before = now_ms();
        let start = oauth
            .begin_device_flow(
                &profile,
                &format!("http://127.0.0.1:{port}/device_authorization"),
            )
            .await
            .unwrap();

        assert_eq!(start.device_code, "dc-123");
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.verification_uri, "https://example.test/device");
        assert_eq!(
            start.verification_uri_complete,
            Some("https://example.test/device?user_code=ABCD-EFGH".to_string())
        );
        assert_eq!(start.interval_secs, 7);
        assert!(start.expires_at >= before + 600_000);
    }

    #[tokio::test]
    async fn begin_device_flow_never_places_a_provider_user_code_in_an_error() {
        use axum::routing::post;

        let app = Router::new().route(
            "/device_authorization",
            post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    r#"{"error":"invalid_request","user_code":"NEVER-EXPOSE"}"#,
                )
            }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let error = oauth
            .begin_device_flow(
                &test_profile("default"),
                &format!("http://127.0.0.1:{port}/device_authorization"),
            )
            .await
            .expect_err("the provider returned a non-success response");
        let OauthErr::Failed(message) = error else {
            panic!("expected a failed device-authorization error");
        };
        assert!(
            !message.contains("NEVER-EXPOSE"),
            "a provider response must not leak a transient user code through OauthErr"
        );
    }

    #[tokio::test]
    async fn begin_device_flow_defaults_interval_to_five_when_omitted() {
        use axum::routing::post;

        let app = Router::new().route(
            "/device_authorization",
            post(|| async move {
                axum::Json(serde_json::json!({
                    "device_code": "dc-123",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://example.test/device",
                    "expires_in": 600,
                }))
            }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let start = oauth
            .begin_device_flow(
                &profile,
                &format!("http://127.0.0.1:{port}/device_authorization"),
            )
            .await
            .unwrap();

        assert_eq!(start.interval_secs, 5);
        assert_eq!(start.verification_uri_complete, None);
    }

    #[tokio::test]
    async fn poll_device_flow_pending_then_ready_persists_the_token() {
        use axum::{routing::post, Form, Json};
        use std::collections::HashMap;
        use std::sync::{Arc as StdArc, Mutex};

        let poll_count = StdArc::new(Mutex::new(0u32));
        let handler_count = poll_count.clone();
        let app = Router::new().route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let poll_count = handler_count.clone();
                async move {
                    assert_eq!(
                        form.get("grant_type").map(String::as_str),
                        Some("urn:ietf:params:oauth:grant-type:device_code")
                    );
                    assert_eq!(form.get("device_code").map(String::as_str), Some("dc-123"));
                    let mut n = poll_count.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        Json(serde_json::json!({"error": "authorization_pending"}))
                    } else {
                        Json(serde_json::json!({
                            "access_token": "at",
                            "refresh_token": "rt",
                            "expires_in": 3600,
                        }))
                    }
                }
            }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store.clone(), "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");
        let token_url = format!("http://127.0.0.1:{port}/token");
        let expires_at = now_ms() + 600_000;

        let first = oauth
            .poll_device_flow(&profile, &token_url, "dc-123", expires_at)
            .await
            .unwrap();
        assert_eq!(first, DevicePollOutcome::Pending);

        let second = oauth
            .poll_device_flow(&profile, &token_url, "dc-123", expires_at)
            .await
            .unwrap();
        assert_eq!(second, DevicePollOutcome::Ready);

        let stored = store
            .get_plugin_oauth_profile_token("github", "default")
            .await
            .unwrap()
            .expect("token should be persisted on Ready");
        assert_eq!(stored.access_token, "at");
        assert_eq!(stored.refresh_token, Some("rt".to_string()));
    }

    #[tokio::test]
    async fn poll_device_flow_slow_down_maps_to_slow_down_outcome() {
        use axum::routing::post;

        let app = Router::new().route(
            "/token",
            post(|| async move { axum::Json(serde_json::json!({"error": "slow_down"})) }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let outcome = oauth
            .poll_device_flow(
                &profile,
                &format!("http://127.0.0.1:{port}/token"),
                "dc-123",
                now_ms() + 600_000,
            )
            .await
            .unwrap();
        assert_eq!(outcome, DevicePollOutcome::SlowDown);
    }

    #[tokio::test]
    async fn poll_device_flow_access_denied_and_expired_token_map_correctly() {
        use axum::extract::State;
        use axum::routing::post;
        use std::sync::{Arc as StdArc, Mutex};

        let which = StdArc::new(Mutex::new("access_denied".to_string()));
        let handler_which = which.clone();
        let app = Router::new().route(
            "/token",
            post(
                move |State(which): State<StdArc<Mutex<String>>>| async move {
                    let error = which.lock().unwrap().clone();
                    axum::Json(serde_json::json!({"error": error}))
                },
            ),
        );
        let app = app.with_state(handler_which);
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");
        let token_url = format!("http://127.0.0.1:{port}/token");

        let denied = oauth
            .poll_device_flow(&profile, &token_url, "dc-123", now_ms() + 600_000)
            .await
            .unwrap();
        assert_eq!(denied, DevicePollOutcome::Denied);

        *which.lock().unwrap() = "expired_token".to_string();
        let expired = oauth
            .poll_device_flow(&profile, &token_url, "dc-123", now_ms() + 600_000)
            .await
            .unwrap();
        assert_eq!(expired, DevicePollOutcome::Expired);
    }

    #[tokio::test]
    async fn poll_device_flow_after_expires_at_is_expired_without_contacting_the_server() {
        use axum::routing::post;
        use std::sync::{Arc as StdArc, Mutex};

        let hits = StdArc::new(Mutex::new(0u32));
        let handler_hits = hits.clone();
        let app = Router::new().route(
            "/token",
            post(move || {
                let hits = handler_hits.clone();
                async move {
                    *hits.lock().unwrap() += 1;
                    axum::Json(serde_json::json!({"access_token": "should-not-be-reached"}))
                }
            }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("client-abc".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let profile = test_profile("default");

        let outcome = oauth
            .poll_device_flow(
                &profile,
                &format!("http://127.0.0.1:{port}/token"),
                "dc-123",
                now_ms() - 1_000,
            )
            .await
            .unwrap();
        assert_eq!(outcome, DevicePollOutcome::Expired);
        assert_eq!(
            *hits.lock().unwrap(),
            0,
            "expired poll must not hit the network"
        );
    }
}
