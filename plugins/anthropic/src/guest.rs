//! wasm32-only guest glue for the `anthropic` provider component.
//!
//! Unlike the OpenAI-format components, this glue is NOT emitted by the shared
//! `ryuzi_openai_format::provider_component!` macro: Anthropic's `/messages`
//! wire format is a different protocol (see [`crate::logic`]), so the glue
//! calls this crate's own [`crate::logic::AnthropicFormat`] and adds the
//! `anthropic-version` header every Anthropic request requires. What remains
//! shared in SHAPE with the macro — effect orchestration, storage-backed base
//! URL, provider-auth mediation, WIT type mapping — is kept structurally
//! identical so the two stay easy to compare, and every wire DECISION still
//! lives in the natively-tested pure logic rather than here.
//!
//! # No credential is ever set here
//! There is no `ryuzi:http` import to set one on: every request goes through
//! `ryuzi:provider-auth`, where the HOST resolves the user's stored Anthropic
//! key and injects it per the descriptor's `AuthScheme::XApiKey` (as
//! `x-api-key`). The host also discards any credential-shaped header a guest
//! supplies. The only headers this glue sets are content negotiation and the
//! `anthropic-version` protocol version — none of them a credential.

use crate::logic::{self, ChunkOut, ModelOut, ProviderFail, CONFIG};

wit_bindgen::generate!({
    path: "wit",
    world: "anthropic",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};
use ryuzi::provider_auth::provider_auth::{
    self, Header, ProviderAuthError, ProviderRequest, ProviderResponse,
};
use ryuzi::storage::storage;

/// The router provider id this component serves. It must equal the manifest's
/// `provider-ids` entry: the host authorizes each credentialed request against
/// exactly that declaration, so a mismatch is a hard `denied`.
const PROVIDER_ID: &str = "anthropic";

struct ProviderComponent;

impl Guest for ProviderComponent {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        let url = CONFIG.models_url(&base_url());
        let response = authorized_request("GET", &url, None).map_err(map_fail)?;
        let models = if logic::status_is_success(response.status) {
            CONFIG.parse_models(&response.body)
        } else {
            Err(CONFIG.classify_error(response.status, &response.body))
        };
        models
            .map(|list| list.into_iter().map(map_model).collect())
            .map_err(map_fail)
    }

    fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
        if request.model.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "a completion request must name a model".to_string(),
            ));
        }
        let url = CONFIG.messages_url(&base_url());
        let body = CONFIG.build_messages_body(
            &request.model,
            &request.prompt,
            request.max_tokens,
            request.temperature,
        );
        let response = authorized_request("POST", &url, Some(body)).map_err(map_fail)?;
        let chunks = if logic::status_is_success(response.status) {
            CONFIG.parse_message_response(&response.body)
        } else {
            Err(CONFIG.classify_error(response.status, &response.body))
        };
        chunks
            .map(|list| list.into_iter().map(map_chunk).collect())
            .map_err(map_fail)
    }
}

/// Send one request through the host-mediated provider-auth capability. The
/// headers the guest supplies are content negotiation plus the required
/// `anthropic-version` protocol version — the credential is the host's
/// business.
fn authorized_request(
    method: &str,
    url: &str,
    body: Option<Vec<u8>>,
) -> Result<ProviderResponse, ProviderFail> {
    let mut headers = vec![
        Header {
            name: "accept".to_string(),
            value: "application/json".to_string(),
        },
        // Required on every Anthropic request; a protocol version, not a
        // credential (see `logic::ANTHROPIC_VERSION`).
        Header {
            name: "anthropic-version".to_string(),
            value: logic::ANTHROPIC_VERSION.to_string(),
        },
    ];
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

/// The upstream base: the override in this component's storage slice when one
/// is set, else the config's default.
///
/// `ryuzi:storage` is a WORLD IMPORT, so it is always linked — a host that
/// withheld it could not instantiate this component at all, and "storage was
/// not granted" is therefore not a state this function can observe. The
/// `Err(_) => None` arm covers the reachable cases instead: no value stored at
/// this key yet, or a failed read. Either degrades to the default rather than
/// failing the call — the override is an optional affordance, never a
/// correctness dependency.
fn base_url() -> String {
    let stored = match storage::get(logic::BASE_URL_STORAGE_KEY) {
        Ok(value) => String::from_utf8(value.value).ok(),
        Err(_) => None,
    };
    CONFIG.resolve_base_url(stored.as_deref())
}

/// Map a host provider-auth failure onto a provider-error. No variant can carry
/// credential material: the host's own contract keeps the key out of every
/// error it returns, and nothing here adds request headers to a message.
fn map_auth_error(error: ProviderAuthError) -> ProviderFail {
    let label = CONFIG.provider_label;
    match error {
        ProviderAuthError::InvalidRequest(message) => ProviderFail::InvalidRequest(message),
        ProviderAuthError::Denied => ProviderFail::Failed(format!(
            "this bundle is not authorized to use the {label} provider credential"
        )),
        ProviderAuthError::NotConfigured => ProviderFail::Failed(format!(
            "no {label} API key is configured — add one in Settings > Providers"
        )),
        ProviderAuthError::Rejected => ProviderFail::Failed(format!(
            "the {label} endpoint is not in this bundle's network allowlist"
        )),
        ProviderAuthError::Unavailable => ProviderFail::Unavailable,
        ProviderAuthError::Failed(message) => {
            ProviderFail::Failed(format!("{label} request failed: {message}"))
        }
    }
}

fn map_model(model: ModelOut) -> ModelInfo {
    ModelInfo {
        id: model.id,
        display_name: model.display_name,
        context_window: model.context_window,
    }
}

fn map_chunk(chunk: ChunkOut) -> CompletionChunk {
    CompletionChunk {
        text: chunk.text,
        finished: chunk.finished,
        usage: chunk.usage.map(|u| TokenUsage {
            input: u.input,
            output: u.output,
        }),
    }
}

fn map_fail(fail: ProviderFail) -> ProviderError {
    match fail {
        ProviderFail::InvalidRequest(message) => ProviderError::InvalidRequest(message),
        ProviderFail::ModelNotFound => ProviderError::ModelNotFound,
        ProviderFail::RateLimited => ProviderError::RateLimited,
        ProviderFail::Unavailable => ProviderError::Unavailable,
        ProviderFail::Failed(message) => ProviderError::Failed(message),
    }
}

export!(ProviderComponent);
