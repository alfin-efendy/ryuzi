//! The wasm32 guest glue every OpenAI-format provider component needs, as
//! reviewed macros instead of one hand-copied module per provider.
//!
//! The glue is effect orchestration and WIT type mapping ONLY — no
//! wire-protocol decision lives here, they all live in the pure
//! [`crate::OpenAiFormat`] logic, which is why the whole wire mapping is
//! covered by native `cargo test` in this crate. What is left is genuinely
//! identical across providers modulo one config constant and ONE seam: how the
//! built request reaches the network.
//!
//! # The egress seam
//! [`__openai_provider_guest_core!`] emits the egress-AGNOSTIC half — the
//! `ryuzi:provider/provider` `Guest` impl, the base-URL override read, and the
//! WIT type mappers — expressed against two functions the invoking macro
//! defines: `__send_request` (issue one request, return `(status, body)`) and
//! `__list_models_source` (produce this provider's model list). The two public
//! macros supply those:
//!
//! - [`provider_component!`] — an API-KEY component. `__send_request` goes
//!   through host-mediated `ryuzi:provider-auth` (the host injects the user's
//!   stored key), and `__list_models_source` FETCHES `/models`.
//! - [`oauth_provider_component!`] — an OAUTH component. `__send_request` goes
//!   through `ryuzi:oauth`'s `authorized-request` (the host injects the profile
//!   bearer; the component never sees a token), and `__list_models_source`
//!   returns a SEEDED list (its descriptor's `/models` route 404s).
//!
//! Both build the SAME request from the SAME [`crate::OpenAiFormat`] logic and
//! parse the SAME response with it — the wire is never forked, only the seam.
//!
//! # No `Authorization` is ever set here
//! Neither macro imports `ryuzi:http`, so there is no plain-HTTP capability on
//! which to set a credential: an API-key component has only `ryuzi:provider-auth`
//! and an OAuth component has only `ryuzi:oauth`, and in both the HOST injects
//! the credential (per the descriptor's `AuthScheme`, resp. the profile bearer)
//! and discards any credential header the guest supplies. This glue therefore
//! has nothing to contribute to authentication — which is the point, and it is
//! structural (a missing capability) rather than conventional (a rule someone
//! remembered to follow). The only headers it sets are content negotiation.

/// The egress-agnostic body of an OpenAI-format provider guest.
///
/// Invoked at the END of one of the public macros below, in the component's own
/// `wasm32` guest module. It requires these items to be ALREADY defined in that
/// module (order does not matter — they are all module-level items):
///
/// - the `ryuzi:provider/provider` export bindings and `ryuzi:storage`, which
///   this macro `use`s (every OpenAI-format world exports the former and imports
///   the latter identically);
/// - `fn __send_request(method: &str, url: &str, body: Option<Vec<u8>>) ->
///   Result<(u16, Vec<u8>), $crate::ProviderFail>` — the egress seam;
/// - `fn __list_models_source() -> Result<Vec<$crate::ModelOut>,
///   $crate::ProviderFail>` — the model-list source (fetch or seed).
///
/// It defines `CONFIG`, `base_url`, the `Guest` impl, the WIT mappers, and the
/// component export. NOT a public entry point — call one of the two macros below.
#[macro_export]
#[doc(hidden)]
macro_rules! __openai_provider_guest_core {
    (config: $config:expr $(,)?) => {
        use exports::ryuzi::provider::provider::{
            CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
        };
        use ryuzi::storage::storage;

        /// This provider's OpenAI-format wire configuration.
        const CONFIG: &$crate::OpenAiFormat = &$config;

        struct ProviderComponent;

        impl Guest for ProviderComponent {
            fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
                __list_models_source()
                    .map(|list| list.into_iter().map(map_model).collect())
                    .map_err(map_fail)
            }

            fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
                if request.model.is_empty() {
                    return Err(ProviderError::InvalidRequest(
                        "a completion request must name a model".to_string(),
                    ));
                }
                let url = CONFIG.chat_url(&base_url());
                let body = CONFIG.build_chat_body(
                    &request.model,
                    &request.prompt,
                    request.max_tokens,
                    request.temperature,
                );
                let (status, bytes) = __send_request("POST", &url, Some(body)).map_err(map_fail)?;
                let chunks = if $crate::status_is_success(status) {
                    CONFIG.parse_chat_response(&bytes)
                } else {
                    Err(CONFIG.classify_error(status, &bytes))
                };
                chunks
                    .map(|list| list.into_iter().map(map_chunk).collect())
                    .map_err(map_fail)
            }
        }

        /// The upstream base: the override in this component's storage slice
        /// when one is set, else the config's default.
        ///
        /// `ryuzi:storage` is a WORLD IMPORT, so it is always linked — a host
        /// that withheld it could not instantiate this component at all, and
        /// "storage was not granted" is therefore not a state this function can
        /// observe. The `Err(_) => None` arm covers the reachable cases
        /// instead: no value stored at this key yet, or a failed read. Either
        /// degrades to the default rather than failing the call — the override
        /// is an optional affordance, never a correctness dependency.
        fn base_url() -> String {
            let stored = match storage::get($crate::BASE_URL_STORAGE_KEY) {
                Ok(value) => String::from_utf8(value.value).ok(),
                Err(_) => None,
            };
            CONFIG.resolve_base_url(stored.as_deref())
        }

        fn map_model(model: $crate::ModelOut) -> ModelInfo {
            ModelInfo {
                id: model.id,
                display_name: model.display_name,
                context_window: model.context_window,
            }
        }

        fn map_chunk(chunk: $crate::ChunkOut) -> CompletionChunk {
            CompletionChunk {
                text: chunk.text,
                finished: chunk.finished,
                usage: chunk.usage.map(|u| TokenUsage {
                    input: u.input,
                    output: u.output,
                }),
            }
        }

        fn map_fail(fail: $crate::ProviderFail) -> ProviderError {
            match fail {
                $crate::ProviderFail::InvalidRequest(message) => {
                    ProviderError::InvalidRequest(message)
                }
                $crate::ProviderFail::ModelNotFound => ProviderError::ModelNotFound,
                $crate::ProviderFail::RateLimited => ProviderError::RateLimited,
                $crate::ProviderFail::Unavailable => ProviderError::Unavailable,
                $crate::ProviderFail::Failed(message) => ProviderError::Failed(message),
            }
        }

        export!(ProviderComponent);
    };
}

/// Emit a complete `ryuzi:provider/provider` guest for an OpenAI-chat API-KEY
/// provider — egress through host-mediated `ryuzi:provider-auth`.
///
/// Invoke ONCE at the top level of a component's `wasm32`-only `guest` module:
///
/// ```ignore
/// ryuzi_openai_format::provider_component!(
///     world: "groq",
///     provider_id: "groq",
///     config: crate::logic::CONFIG,
/// );
/// ```
///
/// - `world` — the world name in the component's own `wit/world.wit`. It must
///   import `ryuzi:provider-auth` and `ryuzi:storage`, export
///   `ryuzi:provider/provider`, and must NOT import `ryuzi:http`.
/// - `provider_id` — the router provider id. It must equal the manifest's
///   `provider-ids` entry: the host authorizes each credentialed request
///   against exactly that declaration, so a mismatch is a hard `denied`.
/// - `config` — a `&'static`-usable [`crate::OpenAiFormat`] constant.
///
/// The component crate must depend on `wit-bindgen`, since the expansion
/// invokes `wit_bindgen::generate!` in the component's own crate (so that
/// `path: "wit"` resolves against the COMPONENT's manifest directory, not this
/// crate's).
#[macro_export]
macro_rules! provider_component {
    (
        world: $world:literal,
        provider_id: $provider_id:literal,
        config: $config:expr $(,)?
    ) => {
        ::wit_bindgen::generate!({
            path: "wit",
            world: $world,
            generate_all,
        });

        use ryuzi::provider_auth::provider_auth::{
            self, Header, ProviderAuthError, ProviderRequest, ProviderResponse,
        };

        /// The router provider id this component serves — see the macro's
        /// `provider_id` argument.
        const PROVIDER_ID: &str = $provider_id;

        /// Send one request through the host-mediated provider-auth capability,
        /// returning the upstream `(status, body)`. The only headers the guest
        /// supplies are content negotiation — the credential is the host's
        /// business, injected per the descriptor's `AuthScheme`.
        fn __send_request(
            method: &str,
            url: &str,
            body: Option<Vec<u8>>,
        ) -> Result<(u16, Vec<u8>), $crate::ProviderFail> {
            let mut headers = vec![Header {
                name: "accept".to_string(),
                value: "application/json".to_string(),
            }];
            if body.is_some() {
                headers.push(Header {
                    name: "content-type".to_string(),
                    value: "application/json".to_string(),
                });
            }
            let response: ProviderResponse = provider_auth::authorized_request(
                PROVIDER_ID,
                &ProviderRequest {
                    method: method.to_string(),
                    url: url.to_string(),
                    headers,
                    body,
                },
            )
            .map_err(map_auth_error)?;
            Ok((response.status, response.body))
        }

        /// An API-key provider's descriptor sets `has_models_endpoint: true`, so
        /// the model list is FETCHED from `/models` and parsed.
        fn __list_models_source() -> Result<Vec<$crate::ModelOut>, $crate::ProviderFail> {
            let url = CONFIG.models_url(&base_url());
            let (status, bytes) = __send_request("GET", &url, None)?;
            if $crate::status_is_success(status) {
                CONFIG.parse_models(&bytes)
            } else {
                Err(CONFIG.classify_error(status, &bytes))
            }
        }

        /// Map a host provider-auth failure onto a provider failure. No variant
        /// can carry credential material: the host's own contract keeps the key
        /// out of every error it returns, and nothing here adds request headers
        /// to a message.
        fn map_auth_error(error: ProviderAuthError) -> $crate::ProviderFail {
            let label = CONFIG.provider_label;
            match error {
                ProviderAuthError::InvalidRequest(message) => {
                    $crate::ProviderFail::InvalidRequest(message)
                }
                ProviderAuthError::Denied => $crate::ProviderFail::Failed(format!(
                    "this bundle is not authorized to use the {label} provider credential"
                )),
                ProviderAuthError::NotConfigured => $crate::ProviderFail::Failed(format!(
                    "no {label} API key is configured — add one in Settings > Providers"
                )),
                ProviderAuthError::Rejected => $crate::ProviderFail::Failed(format!(
                    "the {label} endpoint is not in this bundle's network allowlist"
                )),
                ProviderAuthError::Unavailable => $crate::ProviderFail::Unavailable,
                ProviderAuthError::Failed(message) => {
                    $crate::ProviderFail::Failed(format!("{label} request failed: {message}"))
                }
            }
        }

        $crate::__openai_provider_guest_core!(config: $config);
    };
}

/// Emit a complete `ryuzi:provider/provider` guest for an OpenAI-chat OAUTH
/// provider — egress through `ryuzi:oauth`'s `authorized-request`.
///
/// Invoke ONCE at the top level of a component's `wasm32`-only `guest` module:
///
/// ```ignore
/// ryuzi_openai_format::oauth_provider_component!(
///     world: "qwen",
///     oauth_profile: crate::logic::OAUTH_PROFILE,
///     config: crate::logic::CONFIG,
///     seeded_models: crate::logic::SEEDED_MODELS,
/// );
/// ```
///
/// - `world` — the world name in the component's own `wit/world.wit`. It must
///   import `ryuzi:oauth` and `ryuzi:storage`, export `ryuzi:provider/provider`,
///   and must NOT import `ryuzi:http` or `ryuzi:provider-auth`.
/// - `oauth_profile` — the `[[oauth]]` profile id the guest passes to
///   `authorized-request`. It MUST equal the manifest's `[[oauth]]` id AND the
///   router provider id, or the host rejects the request with `denied`.
/// - `config` — a `&'static`-usable [`crate::OpenAiFormat`] constant.
/// - `seeded_models` — the `&[&str]` this provider advertises. OAuth providers
///   here declare `has_models_endpoint: false` (their `/models` 404s), so
///   `list-models` returns this seed rather than fetching. Transcribe it from
///   the descriptor's `models`.
///
/// The credential never crosses into the guest: the host resolves the stored
/// access token for the profile, injects it as the `Authorization: Bearer`
/// header (stripping any the component set — it sets none), and hands back only
/// the upstream response.
#[macro_export]
macro_rules! oauth_provider_component {
    (
        world: $world:literal,
        oauth_profile: $profile:expr,
        config: $config:expr,
        seeded_models: $seeded:expr $(,)?
    ) => {
        ::wit_bindgen::generate!({
            path: "wit",
            world: $world,
            generate_all,
        });

        use ryuzi::oauth::oauth::{self, Header, OauthError, OauthRequest};

        /// The OAuth profile id this component authenticates through — see the
        /// macro's `oauth_profile` argument.
        const OAUTH_PROFILE: &str = $profile;

        /// Send one request through the host-managed OAuth egress for
        /// [`OAUTH_PROFILE`]. The host injects the bearer and strips any
        /// component-set `Authorization` (never present here); the component
        /// never sees the token. The only headers the guest supplies are content
        /// negotiation.
        fn __send_request(
            method: &str,
            url: &str,
            body: Option<Vec<u8>>,
        ) -> Result<(u16, Vec<u8>), $crate::ProviderFail> {
            let mut headers = vec![Header {
                name: "accept".to_string(),
                value: "application/json".to_string(),
            }];
            if body.is_some() {
                headers.push(Header {
                    name: "content-type".to_string(),
                    value: "application/json".to_string(),
                });
            }
            let response = oauth::authorized_request(
                OAUTH_PROFILE,
                &OauthRequest {
                    method: method.to_string(),
                    url: url.to_string(),
                    headers,
                    body,
                },
            )
            .map_err(map_oauth_error)?;
            Ok((response.status, response.body))
        }

        /// An OAuth provider here sets `has_models_endpoint: false` (its
        /// `/models` route 404s), so the model list is the descriptor's SEEDED
        /// set, never a fetch — the same fallback the native router uses.
        fn __list_models_source() -> Result<Vec<$crate::ModelOut>, $crate::ProviderFail> {
            Ok(CONFIG.seeded_models($seeded))
        }

        /// Convert the generated WIT `oauth-error` into the host-free
        /// [`$crate::OAuthFail`] and map it via the natively-tested
        /// [`$crate::oauth_error_to_provider_error`]. No variant carries a token:
        /// the host's `oauth-error` contract keeps it out, and the mapping
        /// originates its own credential-free `denied`/`expired` text.
        fn map_oauth_error(error: OauthError) -> $crate::ProviderFail {
            let fail = match error {
                OauthError::InvalidRequest(message) => $crate::OAuthFail::InvalidRequest(message),
                OauthError::Denied => $crate::OAuthFail::Denied,
                OauthError::Expired => $crate::OAuthFail::Expired,
                OauthError::Failed(message) => $crate::OAuthFail::Failed(message),
            };
            $crate::oauth_error_to_provider_error(CONFIG.provider_label, fail)
        }

        $crate::__openai_provider_guest_core!(config: $config);
    };
}
