//! Host-side OAuth *profile* logic (Task 8 slice 2a). A plugin bundle may
//! declare more than one `[[oauth]]` profile (see
//! `ryuzi_plugin_sdk::OAuthProfile`), so every operation here is keyed by
//! `(ctx.plugin_id, profile_id)` against the profile-scoped
//! `plugin_oauth_profile_tokens` / `plugin_oauth_profile_clients` tables in
//! `store.rs`.
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

use std::time::Duration;

use super::http::{AllowedHttpClient, SafeHttpResponse, DEFAULT_HTTP_TIMEOUT};
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
    /// Bounds the outbound [`Self::authorized_request`] (connect + whole
    /// request). The runtime's `ryuzi:oauth` host threads the component's own
    /// per-call epoch budget in via [`Self::with_timeout`] so a stalled
    /// allowlisted upstream surfaces promptly as [`OauthErr::Failed`] instead of
    /// hanging the host function past an epoch deadline it can never preempt —
    /// the SAME budget `ryuzi:provider-auth` already honours. The
    /// host-caller-driven flows (PKCE / device) do not use this — [`Self::new`]
    /// keeps the [`DEFAULT_HTTP_TIMEOUT`] default, and the device flows bound
    /// themselves separately (see [`bounded_http_client`]).
    timeout: Duration,
}

impl<'a> ProfileOauth<'a> {
    pub fn new(ctx: &'a PluginCapabilityContext) -> Self {
        Self {
            ctx,
            timeout: DEFAULT_HTTP_TIMEOUT,
        }
    }

    /// Like [`Self::new`], but bounds [`Self::authorized_request`] by `timeout`
    /// — the component's per-call epoch budget, threaded in by the runtime's
    /// `ryuzi:oauth` host so an OAuth egress honours the SAME budget
    /// `ryuzi:provider-auth`'s `ProviderAuth::with_timeout` already does.
    pub fn with_timeout(ctx: &'a PluginCapabilityContext, timeout: Duration) -> Self {
        Self { ctx, timeout }
    }

    /// Rejects an OAuth profile ID that the installed bundle did not declare.
    fn ensure_declared_profile(&self, profile_id: &str) -> Result<(), OauthErr> {
        if self
            .ctx
            .oauth_profile_ids
            .iter()
            .any(|declared| declared == profile_id)
        {
            Ok(())
        } else {
            Err(OauthErr::Denied)
        }
    }

    /// Resolves the OAuth client id for `profile`, in priority order: the
    /// plugin's own setting named by `client_id_setting`, then the cached
    /// profile client row (a per-install override filled by discovery/DCR),
    /// then the manifest's baked-in first-party public `client_id` (the `gh`
    /// CLI model — zero-config connect), else `InvalidRequest`.
    async fn resolve_client_id(&self, profile: &OAuthProfile) -> Result<String, OauthErr> {
        // Priority: a user-set setting, then a stored per-install override, then
        // the manifest's baked-in first-party public client id (the `gh` CLI
        // model). A user/enterprise override always wins over the default.
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
                if !client_id.is_empty() {
                    return Ok(client_id);
                }
            }
        }
        if let Some(client_id) = &profile.client_id {
            if !client_id.is_empty() {
                return Ok(client_id.clone());
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
        self.ensure_declared_profile(&profile.id)?;
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
    /// caller. The client is constrained to this context's immutable
    /// manifest-derived network allowlist; callers cannot widen it. See the
    /// module doc for exactly how the bearer is injected.
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
    ) -> Result<SafeHttpResponse, OauthErr> {
        self.ensure_declared_profile(profile_id)?;
        let mut token = self
            .ctx
            .store
            .get_plugin_oauth_profile_token(&self.ctx.plugin_id, profile_id)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?
            .ok_or(OauthErr::Denied)?;

        // Token validity + refresh. A `None` expiry is a non-expiring token
        // (e.g. a GitHub OAuth App user token — the device-flow response carries
        // no `expires_in`), always usable. For a KNOWN expiry:
        //   - near/past expiry AND a refresh token → refresh it (persisting the
        //     new token) and use the fresh one; a refresh failure while the
        //     current token is still valid is tolerated (use the current one),
        //     but a refresh failure on an already-elapsed token is terminal.
        //   - past expiry AND no refresh token → unusable (`Expired`).
        if let Some(exp) = token.expires_at {
            if needs_refresh(now_ms(), Some(exp)) {
                if token.refresh_token.is_some() {
                    match self.refresh_profile_token(profile_id, &token).await {
                        Ok(refreshed) => token = refreshed,
                        Err(error) => {
                            if now_ms() >= exp {
                                return Err(error);
                            }
                            // else: refresh failed but the current token has not
                            // elapsed yet — proceed with it this time.
                        }
                    }
                } else if now_ms() >= exp {
                    return Err(OauthErr::Expired);
                }
            }
        }

        // Bound the request by this profile's timeout (the component's per-call
        // budget when constructed via `with_timeout`); `false` = never honour a
        // component-supplied `Authorization`, the host injects its own bearer.
        let client = AllowedHttpClient::with_self_auth(
            self.ctx.network_allowlist.clone(),
            false,
            self.timeout,
        );
        client
            .request_with_bearer(method, url, headers, body, &token.access_token)
            .await
            .map_err(|error| OauthErr::Failed(format!("{error:?}")))
    }

    /// Exchanges the stored refresh token for a fresh access token via the OAuth
    /// 2.0 `refresh_token` grant, persists it, and returns it. The token
    /// endpoint + client id come from the `(plugin_id, profile_id)`
    /// `plugin_oauth_profile_clients` row, which the device-flow connect
    /// persisted — so this needs neither the manifest nor a widened `ctx`.
    ///
    /// A definitive provider rejection (a JSON `error`, i.e. the refresh token
    /// is dead) marks the stored token `reconnect_required` so the UI surfaces
    /// that a reconnect is needed, and returns [`OauthErr::Expired`]. A
    /// transient transport error returns [`OauthErr::Failed`] WITHOUT flagging
    /// reconnect — the caller keeps the still-valid current token.
    ///
    /// Refresh needs no `client_secret`: it is only reachable for a profile
    /// connected via the public device-flow client, which has none.
    async fn refresh_profile_token(
        &self,
        profile_id: &str,
        current: &crate::plugins::oauth::PluginOauthToken,
    ) -> Result<crate::plugins::oauth::PluginOauthToken, OauthErr> {
        let refresh_token = current
            .refresh_token
            .as_deref()
            .ok_or_else(|| OauthErr::InvalidRequest("profile has no refresh token".to_string()))?;
        let client = self
            .ctx
            .store
            .get_plugin_oauth_profile_client(&self.ctx.plugin_id, profile_id)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?
            .ok_or_else(|| {
                OauthErr::InvalidRequest("no stored client to refresh against".to_string())
            })?;
        let token_url = client
            .token_url
            .ok_or_else(|| OauthErr::InvalidRequest("no stored token endpoint".to_string()))?;
        let client_id = client
            .client_id
            .ok_or_else(|| OauthErr::InvalidRequest("no stored client id".to_string()))?;

        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
        ];
        let response = bounded_http_client()?
            .post(&token_url)
            // Same reason as the device-flow endpoints: without this GitHub (and
            // others) default to a form-urlencoded body the JSON parse rejects.
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|error| OauthErr::Failed(describe_reqwest_error(&error)))?;
        let body = response
            .text()
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| OauthErr::Failed(error.to_string()))?;

        if json.get("error").and_then(|v| v.as_str()).is_some() {
            // The refresh token is dead (revoked/expired). Flag reconnect so the
            // UI stops showing "connected", then report expired.
            let mut dead = current.clone();
            dead.reconnect_required = true;
            let _ = self
                .ctx
                .store
                .upsert_plugin_oauth_profile_token(&self.ctx.plugin_id, profile_id, &dead)
                .await;
            return Err(OauthErr::Expired);
        }

        let access_token = json
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| OauthErr::Failed("refresh response missing `access_token`".into()))?
            .to_string();
        // A provider MAY rotate the refresh token; keep the old one if it didn't.
        let refreshed_refresh = json
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| current.refresh_token.clone());
        let token_type = json
            .get("token_type")
            .and_then(|v| v.as_str())
            .unwrap_or("Bearer")
            .to_string();
        let expires_at = json
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .map(|secs| now_ms() + (secs as i64) * 1000);

        let refreshed = crate::plugins::oauth::PluginOauthToken {
            plugin_id: self.ctx.plugin_id.clone(),
            access_token,
            refresh_token: refreshed_refresh,
            token_type,
            expires_at,
            scopes: current.scopes.clone(),
            reconnect_required: false,
        };
        self.ctx
            .store
            .upsert_plugin_oauth_profile_token(&self.ctx.plugin_id, profile_id, &refreshed)
            .await
            .map_err(|error| OauthErr::Failed(error.to_string()))?;
        Ok(refreshed)
    }

    /// Revokes the local record of `profile_id`'s token — a subsequent
    /// [`Self::authorized_request`] for the same profile then returns
    /// [`OauthErr::Denied`]. Does not attempt vendor-side token revocation
    /// (out of scope for this slice).
    pub async fn disconnect_profile(&self, profile_id: &str) -> Result<(), OauthErr> {
        self.ensure_declared_profile(profile_id)?;
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
        self.ensure_declared_profile(&profile.id)?;
        let client_id = self.resolve_client_id(profile).await?;

        let mut form: Vec<(&str, String)> = vec![("client_id", client_id)];
        let scope = profile.scopes.join(" ");
        if !scope.is_empty() {
            form.push(("scope", scope));
        }

        let response = bounded_http_client()?
            .post(device_authorization_url)
            // Without this, GitHub (and other providers) default to a
            // `application/x-www-form-urlencoded` body, which then fails JSON
            // parsing with "expected value at line 1 column 1".
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|error| OauthErr::Failed(describe_reqwest_error(&error)))?;
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

        self.ensure_declared_profile(&profile.id)?;
        let client_id = self.resolve_client_id(profile).await?;

        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", client_id.as_str()),
        ];
        let response = bounded_http_client()?
            .post(token_url)
            // GitHub's token endpoint also defaults to a form-urlencoded body
            // without this; the poll parses JSON, so ask for JSON explicitly.
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|error| OauthErr::Failed(describe_reqwest_error(&error)))?;
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

        // Persist the endpoints refresh will need later. `authorized_request`'s
        // `refresh_profile_token` reads the `(plugin_id, profile_id)` client row
        // for the token URL + client id, so it can renew an expiring token
        // without the manifest in hand. Best-effort: a failure here only means
        // a future refresh can't run (the token itself is already stored).
        let _ = self
            .ctx
            .store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: self.ctx.plugin_id.clone(),
                profile_id: profile.id.clone(),
                authorize_url: profile.authorize_url.clone(),
                token_url: Some(token_url.to_string()),
                client_id: Some(client_id.clone()),
            })
            .await;

        Ok(DevicePollOutcome::Ready)
    }
}

fn now_ms() -> i64 {
    crate::paths::now_ms()
}

/// reqwest's `Display` shows only the top-level "error sending request for url
/// (...)"; the actual cause (connection reset, dns error, timed out, …) lives
/// in the `source` chain. Flatten the chain so a transport failure is
/// diagnosable rather than opaque.
fn describe_reqwest_error(error: &reqwest::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut src = std::error::Error::source(error);
    while let Some(s) = src {
        parts.push(s.to_string());
        src = s.source();
    }
    parts.join(": ")
}

/// A plain `reqwest::Client` bounded on BOTH the connect phase and the whole
/// request by [`DEFAULT_HTTP_TIMEOUT`], used by the device-flow endpoints that
/// talk to an OAuth provider directly (not through [`AllowedHttpClient`]).
/// Without the bound a stalled provider would hang the device-authorization /
/// token poll forever. Building this client only fails on a malformed
/// TLS/proxy config — which this never sets — but the error is surfaced as
/// [`OauthErr::Failed`] rather than panicking.
fn bounded_http_client() -> Result<reqwest::Client, OauthErr> {
    reqwest::Client::builder()
        .timeout(DEFAULT_HTTP_TIMEOUT)
        .connect_timeout(DEFAULT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| OauthErr::Failed(error.to_string()))
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
            network_allowlist: vec!["127.0.0.1".to_string(), "example.test".to_string()],
            oauth_profile_ids: vec!["default".to_string(), "p1".to_string()],
            provider_ids: vec![],
        }
    }

    fn test_profile(id: &str) -> OAuthProfile {
        OAuthProfile {
            id: id.to_string(),
            authorize_url: Some("https://example.test/authorize".to_string()),
            token_url: Some("https://example.test/token".to_string()),
            device_authorization_url: None,
            scopes: vec!["repo".to_string(), "issues:read".to_string()],
            client_id: None,
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

    // The `gh` CLI model: with no user-set setting and no stored per-install
    // client, the manifest's baked-in public client id resolves — an end-user
    // connects with zero configuration.
    #[tokio::test]
    async fn resolve_client_id_falls_back_to_the_manifest_baked_client_id() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let mut profile = test_profile("default");
        profile.client_id = Some("Iv1.baked-public-id".to_string());

        let start = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await
            .expect("baked manifest client id should resolve");
        assert!(start
            .authorize_url
            .contains("client_id=Iv1.baked-public-id"));
    }

    // A stored per-install client id overrides the manifest default.
    #[tokio::test]
    async fn stored_client_id_overrides_the_manifest_baked_default() {
        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("stored-override".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);
        let mut profile = test_profile("default");
        profile.client_id = Some("Iv1.baked-public-id".to_string());

        let start = oauth
            .begin_pkce(&profile, "https://cockpit.local/callback")
            .await
            .unwrap();
        assert!(start.authorize_url.contains("client_id=stored-override"));
        assert!(!start.authorize_url.contains("Iv1.baked-public-id"));
    }

    #[tokio::test]
    async fn authorized_request_rejects_an_undeclared_profile_id() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let result = oauth
            .authorized_request(
                "not-declared",
                "GET",
                "https://example.test/me",
                vec![],
                None,
            )
            .await;
        assert_eq!(result, Err(OauthErr::Denied));
    }

    #[tokio::test]
    async fn authorized_request_with_no_stored_token_is_denied() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let result = oauth
            .authorized_request("default", "GET", "https://example.test/me", vec![], None)
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
            .authorized_request("p1", "GET", "https://example.test/me", vec![], None)
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
            .authorized_request("default", "GET", "https://example.test/me", vec![], None)
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
            .authorized_request("default", "GET", "https://example.test/me", vec![], None)
            .await;
        assert_eq!(result, Err(OauthErr::Expired));
    }

    // Regression: a NON-EXPIRING token (`expires_at: None` — a GitHub OAuth App
    // user token, whose device-flow response has no `expires_in`) must be
    // usable. The old `needs_refresh(None) == true` gate rejected it as
    // `Expired`, so every tool call reported "GitHub is not connected" even
    // right after a successful connect.
    #[tokio::test]
    async fn authorized_request_with_a_non_expiring_token_is_usable() {
        async fn ok_body() -> impl IntoResponse {
            "authorized"
        }
        let app = Router::new().route("/me", get(ok_body));
        let port = spawn_server(app).await;

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
                    // No expiry — the GitHub OAuth App case.
                    expires_at: None,
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
        let ctx = ctx_for(store, "github");
        let oauth = ProfileOauth::new(&ctx);

        let response = oauth
            .authorized_request(
                "default",
                "GET",
                &format!("http://127.0.0.1:{port}/me"),
                vec![],
                None,
            )
            .await
            .expect("a non-expiring token must be usable, not rejected as Expired");
        assert_eq!(response.body, b"authorized");
    }

    // An expiring token with a refresh token is renewed on use: the stored
    // client row (persisted at connect) supplies the token endpoint + client
    // id, the refresh grant returns a fresh token, and the request goes out
    // with the NEW bearer. The store ends up holding the refreshed token.
    #[tokio::test]
    async fn authorized_request_refreshes_a_near_expiry_token_and_uses_the_new_one() {
        use axum::routing::post;
        let app = Router::new()
            .route(
                "/token",
                post(|| async {
                    r#"{"access_token":"new-access","refresh_token":"rt-new","token_type":"bearer","expires_in":3600}"#
                }),
            )
            .route(
                "/api",
                get(|headers: HeaderMap| async move {
                    headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                }),
            );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "default",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "old-access".into(),
                    refresh_token: Some("rt-old".into()),
                    token_type: "Bearer".into(),
                    expires_at: Some(crate::paths::now_ms() - 1_000), // just elapsed
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: Some(format!("http://127.0.0.1:{port}/token")),
                client_id: Some("cid".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store.clone(), "github");
        let oauth = ProfileOauth::new(&ctx);

        let response = oauth
            .authorized_request(
                "default",
                "GET",
                &format!("http://127.0.0.1:{port}/api"),
                vec![],
                None,
            )
            .await
            .expect("a near-expiry token with a refresh token must be renewed, not rejected");
        assert_eq!(response.body, b"Bearer new-access");

        let stored = store
            .get_plugin_oauth_profile_token("github", "default")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, "new-access");
        assert_eq!(stored.refresh_token.as_deref(), Some("rt-new"));
        assert!(!stored.reconnect_required);
    }

    // A dead refresh token (the provider answers with an `error`) on an already
    // elapsed access token is terminal: report `Expired` and flag the stored
    // token `reconnect_required` so the UI stops showing it connected.
    #[tokio::test]
    async fn authorized_request_refresh_failure_on_an_elapsed_token_marks_reconnect() {
        use axum::routing::post;
        let app = Router::new().route("/token", post(|| async { r#"{"error":"invalid_grant"}"# }));
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        store
            .upsert_plugin_oauth_profile_token(
                "github",
                "default",
                &PluginOauthToken {
                    plugin_id: "github".into(),
                    access_token: "old".into(),
                    refresh_token: Some("rt-dead".into()),
                    token_type: "Bearer".into(),
                    expires_at: Some(crate::paths::now_ms() - 1_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
        store
            .upsert_plugin_oauth_profile_client(&crate::store::PluginOauthProfileClient {
                plugin_id: "github".into(),
                profile_id: "default".into(),
                authorize_url: None,
                token_url: Some(format!("http://127.0.0.1:{port}/token")),
                client_id: Some("cid".into()),
            })
            .await
            .unwrap();
        let ctx = ctx_for(store.clone(), "github");
        let oauth = ProfileOauth::new(&ctx);

        let result = oauth
            .authorized_request("default", "GET", "https://example.test/me", vec![], None)
            .await;
        assert_eq!(result, Err(OauthErr::Expired));

        let stored = store
            .get_plugin_oauth_profile_token("github", "default")
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored.reconnect_required,
            "a dead refresh token must flag reconnect_required"
        );
    }

    #[tokio::test]
    async fn authorized_request_honours_the_per_call_timeout_on_a_stalled_upstream() {
        // Regression guard for the OAuth-egress timeout gap the provider
        // conformance harness exposed: `authorized_request` must be bounded by
        // the component's per-call budget (threaded in by the runtime's
        // `ryuzi:oauth` host), NOT the 30s `DEFAULT_HTTP_TIMEOUT`. A blocked host
        // call is never preempted by the epoch deadline, so without this the
        // whole engine would hang on a stalled Anthropic OAuth upstream.
        use axum::routing::get;

        let app = Router::new().route(
            "/me",
            get(|| async {
                // Far longer than the client budget below: the client's own
                // timeout must fire first, not this.
                tokio::time::sleep(Duration::from_secs(30)).await;
                "too late"
            }),
        );
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

        let oauth = ProfileOauth::with_timeout(&ctx, Duration::from_millis(300));
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            oauth.authorized_request(
                "default",
                "GET",
                &format!("http://127.0.0.1:{port}/me"),
                vec![],
                None,
            ),
        )
        .await
        .expect("the client's own 300ms timeout must fire long before this 5s guard");

        assert!(
            matches!(result, Err(OauthErr::Failed(_))),
            "a stalled upstream must surface as Failed, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the per-call budget must catch the stall promptly, took {:?}",
            started.elapsed()
        );
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

    // Regression: GitHub's device endpoints default to a form-urlencoded body,
    // which fails JSON parsing ("expected value at line 1 column 1") unless we
    // send `Accept: application/json`. This provider returns form-encoded UNLESS
    // that header is present, so the flow only parses if we asked for JSON.
    #[tokio::test]
    async fn begin_device_flow_asks_for_json_so_a_form_defaulting_provider_parses() {
        use axum::http::HeaderMap;
        use axum::routing::post;

        let app = Router::new().route(
            "/device_authorization",
            post(|headers: HeaderMap| async move {
                let wants_json = headers
                    .get("accept")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("application/json"));
                let body = if wants_json {
                    r#"{"device_code":"dc","user_code":"WXYZ-1234","verification_uri":"https://example.test/device","expires_in":900,"interval":5}"#
                } else {
                    "device_code=dc&user_code=WXYZ-1234&verification_uri=https%3A%2F%2Fexample.test%2Fdevice&expires_in=900&interval=5"
                };
                (axum::http::StatusCode::OK, body)
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

        let start = oauth
            .begin_device_flow(
                &test_profile("default"),
                &format!("http://127.0.0.1:{port}/device_authorization"),
            )
            .await
            .expect("Accept: application/json must be sent so the JSON body parses");
        assert_eq!(start.user_code, "WXYZ-1234");
        assert_eq!(start.device_code, "dc");
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

        // Ready also persists the token endpoint + client id into the client
        // row, so a later `authorized_request` can refresh without the manifest.
        let client = store
            .get_plugin_oauth_profile_client("github", "default")
            .await
            .unwrap()
            .expect("connect should persist the client row for refresh");
        assert_eq!(client.token_url.as_deref(), Some(token_url.as_str()));
        assert_eq!(client.client_id.as_deref(), Some("client-abc"));
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
