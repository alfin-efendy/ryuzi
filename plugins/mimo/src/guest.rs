//! wasm32-only guest glue: wires [`crate::logic`] to the `ryuzi:http`/
//! `ryuzi:storage` host imports and exports `ryuzi:provider/provider`.
//!
//! Kept deliberately thin — no wire-protocol decisions live here, only effect
//! orchestration (bootstrap/cache/retry) and WIT type mapping. The JWT, device
//! fingerprint, and session-affinity id are cached in host storage so they
//! survive the per-call re-instantiation the Task 10 adapter performs.

use crate::logic::{self, ChunkOut, ProviderFail};

wit_bindgen::generate!({
    path: "wit",
    world: "mimo",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};
use ryuzi::storage::storage;

// Per-plugin storage keys (namespaced to `mimo` by the host).
const KEY_JWT: &str = "jwt";
const KEY_JWT_EXP: &str = "jwt_exp";
const KEY_FINGERPRINT: &str = "device_fingerprint";
const KEY_SESSION_AFFINITY: &str = "session_affinity";

struct Mimo;

impl Guest for Mimo {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        // No network: MiMo's free host has no /models endpoint (see logic).
        Ok(logic::models().into_iter().map(map_model).collect())
    }

    fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
        let model = if request.model.is_empty() {
            logic::MODEL_ID.to_string()
        } else {
            request.model.clone()
        };
        let session_affinity = ensure_session_affinity();

        let jwt = ensure_jwt().map_err(map_fail)?;
        let response = send_chat(&jwt, &session_affinity, &model, &request).map_err(map_fail)?;

        // The upstream rejected the bootstrap JWT — invalidate, re-mint once,
        // and retry the same request (mirrors llm_router::client::send_upstream).
        let response = if matches!(response.status, 401 | 403) {
            let _ = storage::delete(KEY_JWT);
            let _ = storage::delete(KEY_JWT_EXP);
            let fresh = mint_jwt().map_err(map_fail)?;
            send_chat(&fresh, &session_affinity, &model, &request).map_err(map_fail)?
        } else {
            response
        };

        let chunks = if (200..300).contains(&response.status) {
            logic::parse_chat_response(&response.body)
        } else {
            Err(logic::classify_chat_error(response.status, &response.body))
        };
        chunks.map(|c| c.into_iter().map(map_chunk).collect()).map_err(map_fail)
    }
}

/// Build + send one chat request through the host HTTP capability.
fn send_chat(
    jwt: &str,
    session_affinity: &str,
    model: &str,
    request: &CompletionRequest,
) -> Result<ryuzi::http::http::HttpResponse, ProviderFail> {
    let body = logic::build_chat_body(model, &request.prompt, request.max_tokens, request.temperature);
    let headers = logic::chat_headers(jwt, session_affinity);
    http_post(logic::CHAT_URL, headers, body)
}

/// The cached JWT if still fresh, otherwise a freshly minted one.
fn ensure_jwt() -> Result<String, ProviderFail> {
    if let (Some(jwt), Some(exp)) = (
        storage_get_string(KEY_JWT),
        storage_get_string(KEY_JWT_EXP),
    ) {
        if let Ok(exp_ms) = exp.parse::<i64>() {
            if logic::jwt_is_fresh(exp_ms, now_ms()) {
                return Ok(jwt);
            }
        }
    }
    mint_jwt()
}

/// Mint a fresh bootstrap JWT and cache it (with its expiry) in storage.
fn mint_jwt() -> Result<String, ProviderFail> {
    let fingerprint = ensure_fingerprint();
    let body = logic::bootstrap_body(&fingerprint);
    let headers = vec![
        ("user-agent".to_string(), logic::CHROME_UA.to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    let response = http_post(logic::BOOTSTRAP_URL, headers, body)?;
    if !(200..300).contains(&response.status) {
        return Err(ProviderFail::Failed(format!(
            "MiMo bootstrap failed: HTTP {}",
            response.status
        )));
    }
    let jwt = logic::parse_bootstrap_jwt(&response.body).map_err(ProviderFail::Failed)?;
    let exp = logic::jwt_exp_ms(&jwt, now_ms());
    storage_put_string(KEY_JWT, &jwt);
    storage_put_string(KEY_JWT_EXP, &exp.to_string());
    Ok(jwt)
}

/// The stable per-install device fingerprint, generated once and persisted.
fn ensure_fingerprint() -> String {
    if let Some(existing) = storage_get_string(KEY_FINGERPRINT) {
        return existing;
    }
    let seed = format!("mimo-fp-{}", now_nanos());
    let fingerprint = logic::fingerprint_from_seed(seed.as_bytes());
    storage_put_string(KEY_FINGERPRINT, &fingerprint);
    fingerprint
}

/// The stable per-install session-affinity id, generated once and persisted.
fn ensure_session_affinity() -> String {
    if let Some(existing) = storage_get_string(KEY_SESSION_AFFINITY) {
        return existing;
    }
    let seed = format!("mimo-sa-{}", now_nanos());
    let affinity = logic::session_affinity_from_seed(seed.as_bytes());
    storage_put_string(KEY_SESSION_AFFINITY, &affinity);
    affinity
}

fn http_post(
    url: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<ryuzi::http::http::HttpResponse, ProviderFail> {
    let request = ryuzi::http::http::HttpRequest {
        method: "POST".to_string(),
        url: url.to_string(),
        headers: headers
            .into_iter()
            .map(|(name, value)| ryuzi::http::http::Header { name, value })
            .collect(),
        body: Some(body),
    };
    ryuzi::http::http::request(&request).map_err(|error| ProviderFail::Failed(describe_http_error(error)))
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

/// Read a UTF-8 string from storage. A missing key or any storage error
/// degrades to `None` (the guest then bootstraps/re-generates rather than
/// failing) — storage is best-effort caching, never a correctness dependency.
fn storage_get_string(key: &str) -> Option<String> {
    match storage::get(key) {
        Ok(value) => String::from_utf8(value.value).ok(),
        Err(_) => None,
    }
}

/// Best-effort persist; a storage error is swallowed (see [`storage_get_string`]).
fn storage_put_string(key: &str, value: &str) {
    let _ = storage::put(&storage::StoredValue {
        key: key.to_string(),
        value: value.as_bytes().to_vec(),
    });
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
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

export!(Mimo);
