// A provider component that fetches its model list and completions from an HTTP
// upstream — the provider conformance harness's mock server
// (`plugins::wasm_provider_conformance`). It combines the two fixture patterns
// (`component-provider` + `component-http-import`): it exports
// `ryuzi:provider/provider@0.1.0` and imports `ryuzi:http/http` +
// `ryuzi:storage/storage`.
//
// The upstream base URL is read from the component's OWN storage slice
// (key `conformance-base-url`), which the host seeds before each call — the
// generic endpoint-override channel a real provider component would use in a
// conformance run (`list-models` takes no arguments, so the URL cannot come
// through the request). It exercises the generic provider seam end-to-end
// against MOCKED, allowlisted HTTP:
//   - `list-models` GETs `{base}/models` and parses the served model table
//     (`id\tdisplay\tcontext` per line).
//   - `complete` POSTs to `{base}/complete` and maps the served completion
//     table (`text\tfinished[\tinput\toutput]` per line) into an ORDERED
//     `list<completion-chunk>`.
//   - every request forges an `Authorization` header the host MUST strip before
//     it reaches the upstream (auth absence).
//   - a non-2xx upstream status maps to the matching `provider-error` variant
//     (429 -> rate-limited, 5xx -> unavailable, else failed).
//   - an HTTP failure (e.g. the host's timeout budget catching a stalled
//     upstream) maps to a `provider-error`, never a hang.

wit_bindgen::generate!({
    path: "wit",
    world: "provider-http-fixture",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};
use ryuzi::http::http::{self, Header, HttpRequest};
use ryuzi::storage::storage;

/// Storage key the host seeds with the upstream base URL before each call.
const BASE_URL_KEY: &str = "conformance-base-url";

/// A secret the guest forges onto every request; the host must strip it so it
/// never reaches the upstream. Kept in sync with the same literal in the
/// harness's `FORBIDDEN_AUTHORIZATION`.
const FORBIDDEN_AUTHORIZATION: &str = "Bearer guest-forbidden-secret";

struct Fixture;

/// Read the upstream base URL the host seeded into this component's storage.
fn base_url() -> Result<String, ProviderError> {
    let stored = storage::get(BASE_URL_KEY)
        .map_err(|_| ProviderError::Failed("conformance base url not configured".to_string()))?;
    String::from_utf8(stored.value)
        .map_err(|error| ProviderError::Failed(format!("invalid base url: {error}")))
}

/// Every outbound request forges an `Authorization` header the host must strip.
fn forged_headers() -> Vec<Header> {
    vec![Header {
        name: "Authorization".to_string(),
        value: FORBIDDEN_AUTHORIZATION.to_string(),
    }]
}

/// Map a host HTTP-capability error onto a provider-error.
fn map_http_error(error: http::HttpError) -> ProviderError {
    match error {
        http::HttpError::InvalidRequest(message) => ProviderError::InvalidRequest(message),
        http::HttpError::Rejected => {
            ProviderError::Failed("upstream host not allowlisted".to_string())
        }
        http::HttpError::Unavailable => ProviderError::Unavailable,
        http::HttpError::Failed(message) => ProviderError::Failed(message),
    }
}

/// Map a non-2xx upstream status onto the matching provider-error, or `None`
/// for a 2xx success.
fn status_error(status: u16) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        429 => Some(ProviderError::RateLimited),
        500 | 502 | 503 | 504 => Some(ProviderError::Unavailable),
        other => Some(ProviderError::Failed(format!("upstream status {other}"))),
    }
}

impl Guest for Fixture {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        let base = base_url()?;
        let response = http::request(&HttpRequest {
            method: "GET".to_string(),
            url: format!("{base}/models"),
            headers: forged_headers(),
            body: None,
        })
        .map_err(map_http_error)?;
        if let Some(error) = status_error(response.status) {
            return Err(error);
        }
        let body = String::from_utf8(response.body)
            .map_err(|error| ProviderError::Failed(format!("invalid models body: {error}")))?;
        let mut models = Vec::new();
        for line in body.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let id = fields.next().unwrap_or_default().to_string();
            let display_name = fields.next().unwrap_or_default().to_string();
            let context_window = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
            models.push(ModelInfo {
                id,
                display_name,
                context_window,
            });
        }
        Ok(models)
    }

    fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
        let base = base_url()?;
        let response = http::request(&HttpRequest {
            method: "POST".to_string(),
            url: format!("{base}/complete"),
            headers: forged_headers(),
            body: Some(request.prompt.into_bytes()),
        })
        .map_err(map_http_error)?;
        if let Some(error) = status_error(response.status) {
            return Err(error);
        }
        let body = String::from_utf8(response.body)
            .map_err(|error| ProviderError::Failed(format!("invalid completion body: {error}")))?;
        let mut chunks = Vec::new();
        for line in body.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let text = fields.next().unwrap_or_default().to_string();
            let finished = fields.next() == Some("true");
            let input = fields.next().and_then(|f| f.parse().ok());
            let output = fields.next().and_then(|f| f.parse().ok());
            let usage = match (input, output) {
                (Some(input), Some(output)) => Some(TokenUsage { input, output }),
                _ => None,
            };
            chunks.push(CompletionChunk {
                text,
                finished,
                usage,
            });
        }
        Ok(chunks)
    }
}

export!(Fixture);
