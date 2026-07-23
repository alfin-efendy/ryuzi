//! wasm32-only guest glue: wires [`crate::logic`] to the
//! `ryuzi:provider-auth`/`ryuzi:storage` host imports and exports
//! `ryuzi:provider/provider`.
//!
//! Kept deliberately thin — no wire-protocol decisions live here, only effect
//! orchestration and WIT type mapping.
//!
//! # No `Authorization` is ever set here
//! There is no `ryuzi:http` import to set one on: every request goes through
//! [`authorized_request`], where the HOST resolves the user's stored OpenAI key
//! and injects it per the descriptor's auth scheme. The host also discards any
//! credential header a guest supplies, so this module simply has nothing to
//! contribute to authentication — which is the point.

use crate::logic::{self, ChunkOut, ProviderFail};

wit_bindgen::generate!({
    path: "wit",
    world: "openai",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};
use ryuzi::provider_auth::provider_auth::{
    self, Header, ProviderAuthError, ProviderRequest, ProviderResponse,
};
use ryuzi::storage::storage;

/// The router provider id this component serves. Must match the manifest's
/// `provider-ids` — the host authorizes `authorized_request` against exactly
/// that declaration, so a mismatch is a hard `denied`.
const PROVIDER_ID: &str = "openai";

struct OpenAi;

impl Guest for OpenAi {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        let url = logic::models_url(&base_url());
        let response = authorized_request("GET", &url, None).map_err(map_fail)?;
        let models = if logic::status_is_success(response.status) {
            logic::parse_models(&response.body)
        } else {
            Err(logic::classify_error(response.status, &response.body))
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
        let url = logic::chat_url(&base_url());
        let body = logic::build_chat_body(
            &request.model,
            &request.prompt,
            request.max_tokens,
            request.temperature,
        );
        let response = authorized_request("POST", &url, Some(body)).map_err(map_fail)?;
        let chunks = if logic::status_is_success(response.status) {
            logic::parse_chat_response(&response.body)
        } else {
            Err(logic::classify_error(response.status, &response.body))
        };
        chunks
            .map(|list| list.into_iter().map(map_chunk).collect())
            .map_err(map_fail)
    }
}

/// Send one request through the host-mediated provider-auth capability. The
/// only headers the guest supplies are content negotiation — the credential is
/// the host's business.
fn authorized_request(
    method: &str,
    url: &str,
    body: Option<Vec<u8>>,
) -> Result<ProviderResponse, ProviderFail> {
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

/// The upstream base: the override in this component's storage slice if the
/// host granted storage and a value is set, else [`logic::DEFAULT_BASE_URL`].
/// A storage error degrades to the default rather than failing the call —
/// storage is an optional affordance, never a correctness dependency.
fn base_url() -> String {
    let stored = match storage::get(logic::BASE_URL_STORAGE_KEY) {
        Ok(value) => String::from_utf8(value.value).ok(),
        Err(_) => None,
    };
    logic::resolve_base_url(stored.as_deref())
}

/// Map a host provider-auth failure onto a provider-error. No variant can carry
/// credential material: the host's own contract keeps the key out of every
/// error it returns, and nothing here adds request headers to a message.
fn map_auth_error(error: ProviderAuthError) -> ProviderFail {
    match error {
        ProviderAuthError::InvalidRequest(message) => ProviderFail::InvalidRequest(message),
        ProviderAuthError::Denied => ProviderFail::Failed(
            "this bundle is not authorized to use the OpenAI provider credential".to_string(),
        ),
        ProviderAuthError::NotConfigured => ProviderFail::Failed(
            "no OpenAI API key is configured — add one in Settings > Providers".to_string(),
        ),
        ProviderAuthError::Rejected => ProviderFail::Failed(
            "the OpenAI endpoint is not in this bundle's network allowlist".to_string(),
        ),
        ProviderAuthError::Unavailable => ProviderFail::Unavailable,
        ProviderAuthError::Failed(message) => {
            ProviderFail::Failed(format!("OpenAI request failed: {message}"))
        }
    }
}

fn map_model(model: logic::ModelOut) -> ModelInfo {
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

export!(OpenAi);
