//! Host-side provider API-key injection (Task 16c1) — the missing piece that
//! lets a user-API-key LLM provider run as a sandboxed WASM component.
//!
//! # Why this exists
//! Every other route to a provider credential is closed by design:
//! `capabilities::settings` returns an EMPTY value for any secret field, and
//! `capabilities::http` strips a component-supplied `Authorization` header.
//! `capabilities::oauth` opens a host-mediated path for OAuth *profile* tokens
//! only. A provider component therefore has no way to authenticate with the
//! user's stored API key — so this module adds the equivalent host-mediated
//! path for static credentials, modeled directly on
//! [`super::oauth::ProfileOauth::authorized_request`].
//!
//! # A component never sees the key
//! [`ProviderAuth::authorized_request`] resolves the credential host-side, and
//! injects it host-side through [`AllowedHttpClient`] — the same client the
//! plain `ryuzi:http` capability uses, so the bundle's manifest network
//! allowlist and the per-hop redirect re-check still apply unchanged. The
//! component receives only the upstream [`SafeHttpResponse`]. The credential is
//! never placed in a response, a header, an error, a log line, or telemetry.
//!
//! # Order of operations
//! 1. **Authorize the caller.** The requested provider id must be one the
//!    installed bundle declared in its manifest `provider-ids`
//!    (`PluginCapabilityContext::provider_ids`, from
//!    `PluginBundleManifest::resolved_provider_ids`); anything else is
//!    [`ProviderAuthErr::Denied`], exactly like `ensure_declared_profile` in
//!    `oauth`.
//! 2. **Resolve the provider descriptor** from the router's catalog
//!    (`llm_router::registry::descriptor`).
//! 3. **Look up the user's credential** in the EXISTING secret storage — the
//!    `provider_connections` rows `llm_router::connections` owns, decrypted on
//!    read by `llm_router::secrets`. There is no second secret store.
//! 4. **Inject per the descriptor's [`AuthScheme`]** — bearer vs `x-api-key`
//!    is DATA on the descriptor, so no provider id is ever named in host code
//!    here (Global Constraint: no plugin-ID-specific host branch).
//! 5. **Send through [`AllowedHttpClient`]**, having first discarded every
//!    credential-shaped header the component supplied, so a forged credential
//!    can neither reach the wire nor sit alongside the host's.

use super::http::{AllowedHttpClient, HttpErr, SafeHttpResponse, DEFAULT_HTTP_TIMEOUT};
use super::PluginCapabilityContext;
use crate::llm_router::connections::{self, ConnectionRow};
use crate::llm_router::registry::{self, AuthScheme, ProviderDescriptor};
use std::time::Duration;

/// Credential-carrying request header names the host ALWAYS removes from a
/// component-supplied header list before injecting its own. `authorization` is
/// already stripped inside [`AllowedHttpClient`] for every non-first-party
/// bundle, but it is repeated here so this capability's guarantee does not
/// depend on that flag, and `x-api-key` is added because
/// [`AuthScheme::XApiKey`] injects it: `reqwest`'s builder APPENDS headers, so
/// a component-supplied copy left in place would travel to the upstream
/// alongside the host's value.
const HOST_MANAGED_CREDENTIAL_HEADERS: &[&str] = &["authorization", "x-api-key"];

/// A capability-adapter-local error, mapped to the generated WIT
/// `ProviderAuthError` variants by the runtime's `Host` trait impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAuthErr {
    /// Malformed request, or a provider id with no descriptor in the catalog.
    InvalidRequest(String),
    /// The calling bundle did not declare this provider id.
    Denied,
    /// The provider requires a credential and the user has stored none.
    NotConfigured,
    /// The target host (or a redirect hop) is outside the manifest allowlist.
    Rejected,
    Unavailable,
    Failed(String),
}

/// How the host authenticates one outbound request. Derived purely from the
/// provider descriptor's [`AuthScheme`], never from its id.
enum CredentialInjection {
    /// `Authorization: Bearer <credential>`, applied through
    /// [`AllowedHttpClient::request_with_bearer`] so the host's value is added
    /// last and unconditionally.
    Bearer(String),
    /// Literal credential headers appended after the component's (filtered)
    /// headers — e.g. `x-api-key`.
    Headers(Vec<(String, String)>),
    /// The descriptor declares no credential at all (local endpoints such as
    /// Ollama, and free-tier passthrough providers).
    None,
}

/// One plugin's view of the provider credentials its bundle declared.
pub struct ProviderAuth<'a> {
    pub ctx: &'a PluginCapabilityContext,
    /// Wall-clock bound on the outbound request (connect + whole request).
    /// Defaults to [`DEFAULT_HTTP_TIMEOUT`]; the runtime threads the calling
    /// component's own epoch budget in via [`Self::with_timeout`]. Epoch
    /// interruption only preempts GUEST wasm, never a host function, so
    /// without this bound a stalled allowlisted server would hang the host
    /// call past a deadline nothing can enforce — see
    /// `capabilities::http::DEFAULT_HTTP_TIMEOUT`.
    timeout: Duration,
}

impl<'a> ProviderAuth<'a> {
    pub fn new(ctx: &'a PluginCapabilityContext) -> Self {
        Self {
            ctx,
            timeout: DEFAULT_HTTP_TIMEOUT,
        }
    }

    /// Like [`Self::new`], but bounds the outbound request by `timeout` — the
    /// calling component's per-call epoch budget.
    pub fn with_timeout(ctx: &'a PluginCapabilityContext, timeout: Duration) -> Self {
        Self { ctx, timeout }
    }

    /// Rejects a provider id the installed bundle did not declare in its
    /// manifest `provider-ids`. Mirrors
    /// `ProfileOauth::ensure_declared_profile`: the check is a pure set
    /// membership test against immutable, manifest-derived context state, so a
    /// component cannot widen it.
    fn ensure_declared_provider(&self, provider_id: &str) -> Result<(), ProviderAuthErr> {
        if self
            .ctx
            .provider_ids
            .iter()
            .any(|declared| declared == provider_id)
        {
            Ok(())
        } else {
            Err(ProviderAuthErr::Denied)
        }
    }

    /// The user's stored API key for `provider_id`, read through the existing
    /// `provider_connections` storage (decrypted on read by
    /// `llm_router::secrets`). Rows are already ordered by routing priority, so
    /// the first usable one is the same credential the native router would
    /// pick. OAuth-backed rows are skipped: their credential is an access token
    /// with different injection semantics, already served by `ryuzi:oauth`.
    async fn stored_credential(
        &self,
        provider_id: &str,
    ) -> Result<Option<String>, ProviderAuthErr> {
        let rows = connections::list_connections(&self.ctx.store)
            .await
            .map_err(|error| ProviderAuthErr::Failed(error.to_string()))?;
        Ok(rows
            .into_iter()
            .find_map(|row| usable_api_key(row, provider_id)))
    }

    /// Issues one HTTP request against `url` authenticated with the user's
    /// stored credential for `provider_id`, without ever exposing that
    /// credential to the caller. The client is constrained to this context's
    /// immutable manifest-derived network allowlist; callers cannot widen it.
    /// See the module doc for the exact ordering guarantee.
    pub async fn authorized_request(
        &self,
        provider_id: &str,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<SafeHttpResponse, ProviderAuthErr> {
        self.ensure_declared_provider(provider_id)?;
        let descriptor = registry::descriptor(provider_id).ok_or_else(|| {
            ProviderAuthErr::InvalidRequest(format!("unknown provider `{provider_id}`"))
        })?;
        let injection = self.resolve_injection(descriptor, provider_id).await?;

        // Whatever the scheme, the component's own credential headers are
        // dropped first, so the host's value is the only one that can reach the
        // wire (and cannot be duplicated alongside a forged one).
        let headers: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(name, _)| !is_host_managed_credential_header(name))
            .collect();

        // `allow_self_auth = false` unconditionally: a host-injected provider
        // credential must never coexist with a component-set `Authorization`,
        // not even for a verified first-party bundle.
        let client = AllowedHttpClient::with_self_auth(
            self.ctx.network_allowlist.clone(),
            false,
            self.timeout,
        );
        let result = match injection {
            CredentialInjection::Bearer(credential) => {
                client
                    .request_with_bearer(method, url, headers, body, &credential)
                    .await
            }
            CredentialInjection::Headers(injected) => {
                let mut headers = headers;
                headers.extend(injected);
                client.request(method, url, headers, body).await
            }
            CredentialInjection::None => client.request(method, url, headers, body).await,
        };
        result.map_err(map_http_err)
    }

    /// Turns descriptor DATA plus the stored credential into the concrete
    /// injection for this request. The `match` is over the descriptor's
    /// [`AuthScheme`] — the same data the native router path keys off — so
    /// adding a provider never touches this module.
    async fn resolve_injection(
        &self,
        descriptor: &ProviderDescriptor,
        provider_id: &str,
    ) -> Result<CredentialInjection, ProviderAuthErr> {
        if descriptor.auth == AuthScheme::None {
            return Ok(CredentialInjection::None);
        }
        let credential = self
            .stored_credential(provider_id)
            .await?
            .ok_or(ProviderAuthErr::NotConfigured)?;
        Ok(match descriptor.auth {
            AuthScheme::Bearer => CredentialInjection::Bearer(credential),
            AuthScheme::XApiKey => {
                CredentialInjection::Headers(vec![("x-api-key".to_string(), credential)])
            }
            // Handled above; repeated so a new scheme is a compile error here
            // rather than a silently unauthenticated request.
            AuthScheme::None => CredentialInjection::None,
        })
    }
}

/// `true` when `name` is a header the host itself manages for credential
/// injection (case-insensitive) — see [`HOST_MANAGED_CREDENTIAL_HEADERS`].
fn is_host_managed_credential_header(name: &str) -> bool {
    let lower = name.to_lowercase();
    HOST_MANAGED_CREDENTIAL_HEADERS.contains(&lower.as_str())
}

/// The API key `row` contributes for `provider_id`, if any: the row must be for
/// that provider, enabled, not OAuth-backed, and carry a non-blank key.
fn usable_api_key(row: ConnectionRow, provider_id: &str) -> Option<String> {
    if row.provider != provider_id || !row.enabled || connections::is_oauth(&row) {
        return None;
    }
    // Returned verbatim (only the emptiness test trims): the native router
    // path sends `conn.data.api_key` as stored, and a capability that silently
    // reshaped the user's credential would diverge from it.
    row.data.api_key.filter(|key| !key.trim().is_empty())
}

/// Maps the shared HTTP adapter's error onto this capability's typed error.
/// Deliberately variant-preserving (rather than the `format!("{error:?}")` the
/// OAuth adapter uses) so a guest can tell an allowlist refusal from a
/// transport failure without parsing a string. No variant carries credential
/// material: `HttpErr::Failed` wraps a `reqwest` error, which reports the URL
/// and the transport cause but never request headers.
fn map_http_err(error: HttpErr) -> ProviderAuthErr {
    match error {
        HttpErr::InvalidRequest(message) => ProviderAuthErr::InvalidRequest(message),
        HttpErr::Rejected => ProviderAuthErr::Rejected,
        HttpErr::Unavailable => ProviderAuthErr::Unavailable,
        HttpErr::Failed(message) => ProviderAuthErr::Failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use crate::llm_router::registry::{
        self, ApiFormat, AuthScheme, ProviderCategory, ProviderDescriptor, ProviderToolTransport,
    };
    use crate::llm_router::secrets;
    use crate::plugins::capabilities::PluginCapabilityContext;
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use crate::telemetry::NoopTelemetry;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Redirect};
    use axum::routing::get;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    /// What the upstream mock server actually saw on the wire. The whole point
    /// of the capability is that the host — not the guest — decides these.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct SeenAuth {
        authorization: Option<String>,
        x_api_key: Option<String>,
    }

    async fn open_test_store() -> (Arc<Store>, tempfile::NamedTempFile) {
        secrets::use_test_key_file();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        (Arc::new(store), tmp)
    }

    /// A capability context for `plugin_id` whose bundle declares
    /// `provider_ids` (the manifest `provider-ids` list) and can reach only
    /// loopback / `example.test`.
    fn ctx_for(
        store: Arc<Store>,
        plugin_id: &str,
        provider_ids: &[&str],
    ) -> PluginCapabilityContext {
        PluginCapabilityContext {
            plugin_id: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            settings: SettingsStore::new(store.clone()),
            store,
            telemetry: Arc::new(NoopTelemetry),
            network_allowlist: vec!["127.0.0.1".to_string(), "example.test".to_string()],
            oauth_profile_ids: vec![],
            provider_ids: provider_ids.iter().map(|id| id.to_string()).collect(),
        }
    }

    /// Stores a user API key for `provider` through the SAME path Cockpit uses
    /// (`provider_connections`, encrypted at rest by `llm_router::secrets`).
    async fn seed_api_key(store: &Store, provider: &str, key: &str) {
        seed_connection(store, provider, Some(key), true, "api_key").await;
    }

    async fn seed_connection(
        store: &Store,
        provider: &str,
        key: Option<&str>,
        enabled: bool,
        auth_type: &str,
    ) {
        let now = crate::paths::now_ms();
        connections::add_connection(
            store,
            ConnectionRow {
                id: format!("conn-{provider}-{auth_type}"),
                provider: provider.to_string(),
                auth_type: auth_type.to_string(),
                label: provider.to_string(),
                priority: 0,
                enabled,
                data: ConnectionData {
                    api_key: key.map(str::to_string),
                    ..Default::default()
                },
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .expect("seeding a provider connection should succeed");
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

    /// Records the credential headers the upstream saw and answers with a body
    /// that contains NO credential material, so a response-side leak can only
    /// come from the host itself.
    async fn record_auth(
        State(seen): State<Arc<Mutex<SeenAuth>>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        *seen.lock().unwrap() = SeenAuth {
            authorization: header("authorization"),
            x_api_key: header("x-api-key"),
        };
        ([("x-upstream-note", "no-credential-here")], "upstream-ok")
    }

    /// A mock upstream plus the slot recording what it saw.
    async fn spawn_recording_server() -> (u16, Arc<Mutex<SeenAuth>>) {
        let seen = Arc::new(Mutex::new(SeenAuth::default()));
        let app = Router::new()
            .route("/v1/chat", get(record_auth))
            .with_state(seen.clone());
        (spawn_server(app).await, seen)
    }

    /// Test-only catalog entry with an explicit [`AuthScheme`] and an id no
    /// host code could possibly know about — the proof that injection is
    /// driven by descriptor DATA rather than a per-provider-id branch.
    fn register_test_descriptor(id: &'static str, auth: AuthScheme) {
        let descriptor: &'static ProviderDescriptor = Box::leak(Box::new(ProviderDescriptor {
            id,
            name: "Task 16c1 Test Provider",
            family: id,
            color: "#000000",
            initial: "T",
            category: ProviderCategory::ApiKey,
            format: ApiFormat::OpenAi,
            tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
            base_url: None,
            auth,
            models: &[],
            requires_base_url: true,
            oauth: None,
            no_auth: false,
            device_flow: None,
            free_tier: false,
            risk_notice: false,
            chat_path: None,
            has_models_endpoint: false,
            uses_max_completion_tokens: false,
            device_grant: None,
        }));
        registry::register_custom_descriptor(descriptor);
    }

    #[tokio::test]
    async fn authorized_request_injects_the_stored_key_and_overrides_a_forged_authorization() {
        let (port, seen) = spawn_recording_server().await;
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-real-user-key").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let response = ProviderAuth::new(&ctx)
            .authorized_request(
                "openai",
                "GET",
                &format!("http://127.0.0.1:{port}/v1/chat"),
                vec![
                    ("Authorization".to_string(), "Bearer sneaky".to_string()),
                    ("x-api-key".to_string(), "sneaky-too".to_string()),
                ],
                None,
            )
            .await
            .expect("a declared provider with a stored key must be authorized");

        assert_eq!(response.status, 200);
        assert_eq!(
            seen.lock().unwrap().authorization,
            Some("Bearer sk-real-user-key".to_string()),
            "the host-injected credential must be the only Authorization on the wire"
        );
        assert_eq!(
            seen.lock().unwrap().x_api_key,
            None,
            "a guest-forged x-api-key must never reach the upstream"
        );
    }

    #[tokio::test]
    async fn authorized_request_rejects_a_provider_id_the_bundle_did_not_declare() {
        let (store, _tmp) = open_test_store().await;
        // A credential DOES exist for anthropic — the denial must come from
        // the manifest authorization check, not from a missing credential.
        seed_api_key(&store, "anthropic", "sk-ant-user-key").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let result = ProviderAuth::new(&ctx)
            .authorized_request("anthropic", "GET", "https://example.test/v1", vec![], None)
            .await;

        assert_eq!(result, Err(ProviderAuthErr::Denied));
    }

    #[tokio::test]
    async fn authorized_request_is_isolated_by_the_declaring_bundle() {
        let (port, _seen) = spawn_recording_server().await;
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-real-user-key").await;
        let url = format!("http://127.0.0.1:{port}/v1/chat");

        // Bundle A declares `openai` and may use the key.
        let bundle_a = ctx_for(store.clone(), "openai-provider", &["openai"]);
        ProviderAuth::new(&bundle_a)
            .authorized_request("openai", "GET", &url, vec![], None)
            .await
            .expect("the declaring bundle must reach its own provider credential");

        // Bundle B declares only `anthropic`; the very same stored key is
        // unreachable for it.
        let bundle_b = ctx_for(store, "anthropic-provider", &["anthropic"]);
        let result = ProviderAuth::new(&bundle_b)
            .authorized_request("openai", "GET", &url, vec![], None)
            .await;

        assert_eq!(
            result,
            Err(ProviderAuthErr::Denied),
            "a bundle must not reach a provider credential it did not declare"
        );
    }

    #[tokio::test]
    async fn authorized_request_without_a_stored_credential_is_not_configured() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let result = ProviderAuth::new(&ctx)
            .authorized_request("openai", "GET", "https://example.test/v1", vec![], None)
            .await;

        assert_eq!(result, Err(ProviderAuthErr::NotConfigured));
    }

    #[tokio::test]
    async fn a_disabled_or_empty_connection_is_not_a_usable_credential() {
        let (store, _tmp) = open_test_store().await;
        seed_connection(&store, "openai", Some("sk-disabled"), false, "api_key").await;
        seed_connection(&store, "openai", Some("   "), true, "blank").await;
        // An OAuth-backed row is served by `ryuzi:oauth`, not this capability.
        seed_connection(&store, "openai", Some("sk-oauth-row"), true, "oauth").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let result = ProviderAuth::new(&ctx)
            .authorized_request("openai", "GET", "https://example.test/v1", vec![], None)
            .await;

        assert_eq!(result, Err(ProviderAuthErr::NotConfigured));
    }

    #[tokio::test]
    async fn the_stored_credential_never_reaches_the_guest_visible_response() {
        let (port, seen) = spawn_recording_server().await;
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-never-show-me").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let response = ProviderAuth::new(&ctx)
            .authorized_request(
                "openai",
                "GET",
                &format!("http://127.0.0.1:{port}/v1/chat"),
                vec![],
                None,
            )
            .await
            .expect("the request itself must succeed");

        // Sanity: the upstream really did receive the credential...
        assert_eq!(
            seen.lock().unwrap().authorization,
            Some("Bearer sk-never-show-me".to_string())
        );
        // ...and none of it comes back through the guest-visible surface.
        assert!(!String::from_utf8_lossy(&response.body).contains("sk-never-show-me"));
        for (name, value) in &response.headers {
            assert!(
                !value.contains("sk-never-show-me"),
                "response header `{name}` leaked the credential"
            );
        }
    }

    #[tokio::test]
    async fn the_stored_credential_never_reaches_a_guest_visible_error() {
        // A closed loopback port: the request fails at the transport layer,
        // which is where a naive `format!("{err:?}")` of a request builder
        // would be most likely to spill headers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = listener.local_addr().unwrap().port();
        drop(listener);

        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-never-show-me").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let error = ProviderAuth::new(&ctx)
            .authorized_request(
                "openai",
                "GET",
                &format!("http://127.0.0.1:{dead_port}/v1/chat"),
                vec![],
                None,
            )
            .await
            .expect_err("a closed port must surface an error");

        assert!(
            !format!("{error:?}").contains("sk-never-show-me"),
            "an error must never carry the user's API key: {error:?}"
        );
    }

    #[tokio::test]
    async fn the_manifest_network_allowlist_still_applies() {
        let (port, _seen) = spawn_recording_server().await;
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-real-user-key").await;
        // A context whose bundle declared a DIFFERENT host: loopback is not
        // reachable even though the provider id is declared and keyed.
        let ctx = PluginCapabilityContext {
            network_allowlist: vec!["api.openai.com".to_string()],
            ..ctx_for(store, "openai-provider", &["openai"])
        };

        let result = ProviderAuth::new(&ctx)
            .authorized_request(
                "openai",
                "GET",
                &format!("http://127.0.0.1:{port}/v1/chat"),
                vec![],
                None,
            )
            .await;

        assert_eq!(result, Err(ProviderAuthErr::Rejected));
    }

    #[tokio::test]
    async fn a_redirect_to_an_unlisted_host_is_refused() {
        let app = Router::new().route(
            "/v1/chat",
            get(|| async { Redirect::temporary("http://exfil.invalid/collect") }),
        );
        let port = spawn_server(app).await;

        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-real-user-key").await;
        let ctx = ctx_for(store, "openai-provider", &["openai"]);

        let result = ProviderAuth::new(&ctx)
            .authorized_request(
                "openai",
                "GET",
                &format!("http://127.0.0.1:{port}/v1/chat"),
                vec![],
                None,
            )
            .await;

        assert_eq!(
            result,
            Err(ProviderAuthErr::Rejected),
            "a redirect off the manifest allowlist must not carry the credential"
        );
    }

    /// Two REAL catalog providers whose descriptors declare different
    /// [`AuthScheme`]s must be authenticated differently, with no host-side
    /// knowledge of either id.
    #[tokio::test]
    async fn injection_follows_the_descriptor_auth_scheme() {
        let (port, seen) = spawn_recording_server().await;
        let url = format!("http://127.0.0.1:{port}/v1/chat");
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "openai", "sk-bearer-key").await;
        seed_api_key(&store, "anthropic", "sk-ant-key").await;
        let ctx = ctx_for(store, "multi-provider", &["openai", "anthropic"]);

        // `openai` declares AuthScheme::Bearer.
        ProviderAuth::new(&ctx)
            .authorized_request("openai", "GET", &url, vec![], None)
            .await
            .expect("bearer provider must be authorized");
        assert_eq!(
            *seen.lock().unwrap(),
            SeenAuth {
                authorization: Some("Bearer sk-bearer-key".to_string()),
                x_api_key: None,
            }
        );

        // `anthropic` declares AuthScheme::XApiKey — same code path, different
        // descriptor data, different header on the wire.
        ProviderAuth::new(&ctx)
            .authorized_request(
                "anthropic",
                "GET",
                &url,
                vec![("x-api-key".to_string(), "forged".to_string())],
                None,
            )
            .await
            .expect("x-api-key provider must be authorized");
        assert_eq!(
            *seen.lock().unwrap(),
            SeenAuth {
                authorization: None,
                x_api_key: Some("sk-ant-key".to_string()),
            }
        );
    }

    /// The decisive no-hardcoded-id proof: an id invented by this test, whose
    /// scheme is chosen at runtime, still authenticates correctly.
    #[tokio::test]
    async fn injection_is_descriptor_driven_for_an_id_no_host_code_knows() {
        register_test_descriptor("task16c1-xapikey", AuthScheme::XApiKey);
        register_test_descriptor("task16c1-bearer", AuthScheme::Bearer);

        let (port, seen) = spawn_recording_server().await;
        let url = format!("http://127.0.0.1:{port}/v1/chat");
        let (store, _tmp) = open_test_store().await;
        seed_api_key(&store, "task16c1-xapikey", "key-x").await;
        seed_api_key(&store, "task16c1-bearer", "key-b").await;
        let ctx = ctx_for(
            store,
            "invented-provider",
            &["task16c1-xapikey", "task16c1-bearer"],
        );

        ProviderAuth::new(&ctx)
            .authorized_request("task16c1-xapikey", "GET", &url, vec![], None)
            .await
            .expect("an invented x-api-key provider must be authorized");
        assert_eq!(
            *seen.lock().unwrap(),
            SeenAuth {
                authorization: None,
                x_api_key: Some("key-x".to_string()),
            }
        );

        ProviderAuth::new(&ctx)
            .authorized_request("task16c1-bearer", "GET", &url, vec![], None)
            .await
            .expect("an invented bearer provider must be authorized");
        assert_eq!(
            *seen.lock().unwrap(),
            SeenAuth {
                authorization: Some("Bearer key-b".to_string()),
                x_api_key: None,
            }
        );
    }

    #[tokio::test]
    async fn a_provider_id_with_no_descriptor_is_an_invalid_request() {
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "ghost-provider", &["not-in-any-catalog"]);

        let result = ProviderAuth::new(&ctx)
            .authorized_request(
                "not-in-any-catalog",
                "GET",
                "https://example.test/v1",
                vec![],
                None,
            )
            .await;

        assert!(
            matches!(result, Err(ProviderAuthErr::InvalidRequest(_))),
            "an unknown provider id must be a typed invalid-request, got {result:?}"
        );
    }

    /// `AuthScheme::None` providers (local endpoints like Ollama) need no
    /// credential at all: the request goes out unauthenticated rather than
    /// failing as unconfigured — and a guest-forged credential header is still
    /// stripped.
    #[tokio::test]
    async fn a_none_scheme_provider_needs_no_stored_credential() {
        let (port, seen) = spawn_recording_server().await;
        let (store, _tmp) = open_test_store().await;
        let ctx = ctx_for(store, "ollama-provider", &["ollama"]);

        ProviderAuth::new(&ctx)
            .authorized_request(
                "ollama",
                "GET",
                &format!("http://127.0.0.1:{port}/v1/chat"),
                vec![
                    ("Authorization".to_string(), "Bearer sneaky".to_string()),
                    ("x-api-key".to_string(), "sneaky".to_string()),
                ],
                None,
            )
            .await
            .expect("a no-credential provider must still be able to call out");

        assert_eq!(*seen.lock().unwrap(), SeenAuth::default());
    }
}
