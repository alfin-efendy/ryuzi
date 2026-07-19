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
}
