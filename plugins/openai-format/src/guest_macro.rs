//! The wasm32 guest glue every OpenAI-format provider component needs, as one
//! reviewed macro instead of one hand-copied module per provider.
//!
//! The glue is effect orchestration and WIT type mapping ONLY — no
//! wire-protocol decision lives here, they all live in the pure
//! [`crate::OpenAiFormat`] logic, which is why the whole wire mapping is
//! covered by native `cargo test` in this crate. What is left is genuinely
//! identical across providers modulo one config constant, so duplicating it per
//! component would mean re-reviewing (and re-fixing) the same twelve lines of
//! capability handling ten times.
//!
//! # No `Authorization` is ever set here
//! There is no `ryuzi:http` import to set one on: every request goes through
//! `ryuzi:provider-auth`, where the HOST resolves the user's stored key and
//! injects it per the provider descriptor's `AuthScheme`. The host also
//! discards any credential header a guest supplies. This glue therefore has
//! nothing to contribute to authentication — which is the point, and it is
//! structural (a missing capability) rather than conventional (a rule someone
//! remembered to follow).

/// Emit a complete `ryuzi:provider/provider` guest for an OpenAI-chat provider.
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

        use exports::ryuzi::provider::provider::{
            CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
        };
        use ryuzi::provider_auth::provider_auth::{
            self, Header, ProviderAuthError, ProviderRequest, ProviderResponse,
        };
        use ryuzi::storage::storage;

        /// The router provider id this component serves — see the macro's
        /// `provider_id` argument.
        const PROVIDER_ID: &str = $provider_id;

        /// This provider's OpenAI-format wire configuration.
        const CONFIG: &$crate::OpenAiFormat = &$config;

        struct ProviderComponent;

        impl Guest for ProviderComponent {
            fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
                let url = CONFIG.models_url(&base_url());
                let response = authorized_request("GET", &url, None).map_err(map_fail)?;
                let models = if $crate::status_is_success(response.status) {
                    CONFIG.parse_models(&response.body)
                } else {
                    Err(CONFIG.classify_error(response.status, &response.body))
                };
                models
                    .map(|list| list.into_iter().map(map_model).collect())
                    .map_err(map_fail)
            }

            fn complete(
                request: CompletionRequest,
            ) -> Result<Vec<CompletionChunk>, ProviderError> {
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
                let response = authorized_request("POST", &url, Some(body)).map_err(map_fail)?;
                let chunks = if $crate::status_is_success(response.status) {
                    CONFIG.parse_chat_response(&response.body)
                } else {
                    Err(CONFIG.classify_error(response.status, &response.body))
                };
                chunks
                    .map(|list| list.into_iter().map(map_chunk).collect())
                    .map_err(map_fail)
            }
        }

        /// Send one request through the host-mediated provider-auth capability.
        /// The only headers the guest supplies are content negotiation — the
        /// credential is the host's business.
        fn authorized_request(
            method: &str,
            url: &str,
            body: Option<Vec<u8>>,
        ) -> Result<ProviderResponse, $crate::ProviderFail> {
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
            provider_auth::authorized_request(
                PROVIDER_ID,
                &ProviderRequest {
                    method: method.to_string(),
                    url: url.to_string(),
                    headers,
                    body,
                },
            )
            .map_err(map_auth_error)
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

        /// Map a host provider-auth failure onto a provider-error. No variant
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
