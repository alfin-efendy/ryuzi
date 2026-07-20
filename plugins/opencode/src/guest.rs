//! wasm32-only guest glue: wires [`crate::logic`] to the `ryuzi:http` host
//! import and exports `ryuzi:provider/provider`. No storage, no bootstrap —
//! the bearer is a constant, so both `list-models` (a `/models` GET) and
//! `complete` (a `/chat/completions` POST) are single stateless requests.

use crate::logic::{self, ChunkOut, ProviderFail};

wit_bindgen::generate!({
    path: "wit",
    world: "opencode",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};

struct OpenCode;

impl Guest for OpenCode {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        let response = http_send("GET", logic::MODELS_URL, None).map_err(map_fail)?;
        if !(200..300).contains(&response.status) {
            return Err(map_fail(logic::classify_chat_error(
                response.status,
                &response.body,
            )));
        }
        logic::parse_models(&response.body)
            .map(|models| models.into_iter().map(map_model).collect())
            .map_err(map_fail)
    }

    fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
        let body = logic::build_chat_body(
            &request.model,
            &request.prompt,
            request.max_tokens,
            request.temperature,
        );
        let response = http_send("POST", logic::CHAT_URL, Some(body)).map_err(map_fail)?;
        let chunks = if (200..300).contains(&response.status) {
            logic::parse_chat_response(&response.body)
        } else {
            Err(logic::classify_chat_error(response.status, &response.body))
        };
        chunks
            .map(|c| c.into_iter().map(map_chunk).collect())
            .map_err(map_fail)
    }
}

/// One request through the host HTTP capability, carrying the shared OpenCode
/// headers (static bearer + client tag).
fn http_send(
    method: &str,
    url: &str,
    body: Option<Vec<u8>>,
) -> Result<ryuzi::http::http::HttpResponse, ProviderFail> {
    let request = ryuzi::http::http::HttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers: logic::request_headers()
            .into_iter()
            .map(|(name, value)| ryuzi::http::http::Header { name, value })
            .collect(),
        body,
    };
    ryuzi::http::http::request(&request)
        .map_err(|error| ProviderFail::Failed(describe_http_error(error)))
}

fn describe_http_error(error: ryuzi::http::http::HttpError) -> String {
    use ryuzi::http::http::HttpError as E;
    match error {
        E::InvalidRequest(message) => format!("invalid HTTP request: {message}"),
        E::Rejected => "HTTP request rejected by the host allowlist".to_string(),
        E::Unavailable => "HTTP capability unavailable".to_string(),
        E::Failed(message) => format!("HTTP request failed: {message}"),
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

export!(OpenCode);
