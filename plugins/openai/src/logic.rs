//! Pure, host-free OpenAI wire logic: base-URL resolution, request-body
//! shaping, response/model parsing, and upstream-status classification. Every
//! function is deterministic over its inputs (no network, no clock, no
//! storage), so the whole module is covered by native `cargo test`. The wasm
//! `guest` glue supplies the live effects (the host-mediated
//! `ryuzi:provider-auth` egress and `ryuzi:storage`) and maps these plain types
//! to WIT.
//!
//! Nothing here touches a credential: the host resolves and injects the user's
//! API key (see `crate`'s module doc), so this module never sees, stores, or
//! renders one.

use serde_json::{Map, Value};

/// OpenAI's public API base (the `openai` descriptor's `base_url` in
/// `crates/core/src/llm_router/registry.rs`). Used unless the component's own
/// storage slice carries an override — see [`BASE_URL_STORAGE_KEY`].
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Key in this component's (host-scoped) `ryuzi:storage` slice holding an
/// OPTIONAL base-URL override. Real uses: an OpenAI-compatible proxy/gateway,
/// and the provider conformance harness pointing the component at its loopback
/// mock. A blank/whitespace value is treated as "unset". The manifest network
/// allowlist still governs whatever the override resolves to.
pub const BASE_URL_STORAGE_KEY: &str = "base-url";

/// Model-discovery path, appended to the resolved base. The `openai` descriptor
/// declares `has_models_endpoint: true`, so `list-models` really does call it.
pub const MODELS_PATH: &str = "/models";

/// Chat-completions path, appended to the resolved base (the descriptor's
/// `chat_path` is `None`, i.e. the OpenAI-format default).
pub const CHAT_PATH: &str = "/chat/completions";

/// The token-cap request field. The `openai` descriptor declares
/// `uses_max_completion_tokens: true`, so the newer name is what goes on the
/// wire; an OpenAI-format sibling whose descriptor says otherwise flips this
/// one constant to `"max_tokens"`.
pub const MAX_TOKENS_FIELD: &str = "max_completion_tokens";

/// Context window advertised for a model the static table below does not cover.
/// OpenAI's `/models` response reports no context length at all (it carries
/// only `id`/`object`/`created`/`owned_by`), so a window is either a static
/// hint or a guess — this is the conservative hint, deliberately not an
/// invented per-model API field.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// Static, conservative context-window hints by model-id PREFIX, scanned in
/// order so the most specific prefix wins (`gpt-4o` before `gpt-4`). These are
/// long-standing published values for well-known families; anything else — a
/// newer or unknown model — takes [`DEFAULT_CONTEXT_WINDOW`] rather than a
/// fabricated number.
const CONTEXT_WINDOWS: &[(&str, u32)] = &[
    ("gpt-3.5-turbo", 16_385),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4", 8_192),
    ("o1", 200_000),
    ("o3", 200_000),
];

/// Longest an `error.code`/`error.type` tag may be before it stops looking like
/// a machine-readable code and starts looking like prose that could carry
/// upstream-echoed request material. See [`error_tag`].
const MAX_ERROR_TAG_LEN: usize = 64;

/// One model the provider advertises (host-free mirror of the WIT `model-info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOut {
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
}

/// Token usage a chunk may report (host-free mirror of WIT `token-usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageOut {
    pub input: u32,
    pub output: u32,
}

/// One completion chunk (host-free mirror of WIT `completion-chunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkOut {
    pub text: String,
    pub finished: bool,
    pub usage: Option<UsageOut>,
}

/// A provider failure (host-free mirror of WIT `provider-error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFail {
    InvalidRequest(String),
    ModelNotFound,
    RateLimited,
    Unavailable,
    Failed(String),
}

/// The upstream base for this call: a non-blank stored override, else
/// [`DEFAULT_BASE_URL`]. Any trailing `/` is trimmed so path joins never
/// produce a doubled separator.
pub fn resolve_base_url(stored: Option<&str>) -> String {
    let base = stored
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    base.trim_end_matches('/').to_string()
}

/// `<base>/models` — the model-discovery endpoint.
pub fn models_url(base: &str) -> String {
    format!("{base}{MODELS_PATH}")
}

/// `<base>/chat/completions` — the chat endpoint.
pub fn chat_url(base: &str) -> String {
    format!("{base}{CHAT_PATH}")
}

/// Whether an upstream status is a success (and so parsed rather than
/// classified as an error).
pub fn status_is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// The conservative static context window for `model_id` — see
/// [`CONTEXT_WINDOWS`] and [`DEFAULT_CONTEXT_WINDOW`].
pub fn context_window_for(model_id: &str) -> u32 {
    CONTEXT_WINDOWS
        .iter()
        .find(|(prefix, _)| model_id.starts_with(prefix))
        .map(|(_, window)| *window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// Build the NON-STREAMING chat-completions body for a flat prompt.
///
/// The `ryuzi:provider/provider` ABI carries a single `prompt` string, so the
/// request is exactly one `user` message — no system turn, no tools, no
/// multimodal parts. `stream` is false because the host capability is a
/// buffered request/response: the component asks for the whole completion and
/// returns it as one terminal chunk.
pub fn build_chat_body(
    model: &str,
    prompt: &str,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> Vec<u8> {
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert("content".to_string(), Value::String(prompt.to_string()));

    let mut obj = Map::new();
    obj.insert("model".to_string(), Value::String(model.to_string()));
    obj.insert(
        "messages".to_string(),
        Value::Array(vec![Value::Object(message)]),
    );
    obj.insert("stream".to_string(), Value::Bool(false));
    if let Some(max) = max_tokens {
        obj.insert(MAX_TOKENS_FIELD.to_string(), Value::from(max));
    }
    if let Some(temp) = temperature {
        if let Some(number) = serde_json::Number::from_f64(temp as f64) {
            obj.insert("temperature".to_string(), Value::Number(number));
        }
    }
    serde_json::to_vec(&Value::Object(obj)).expect("chat body always serializes")
}

/// Parse an OpenAI `/models` response (`{"data":[{"id":...}]}`) into the
/// advertised model list, preserving the served order. The response carries no
/// display name or context length, so the id doubles as the display name and
/// the window comes from [`context_window_for`]. Entries without a string `id`
/// are skipped rather than failing the whole listing.
pub fn parse_models(body: &[u8]) -> Result<Vec<ModelOut>, ProviderFail> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| ProviderFail::Failed(format!("OpenAI /models response is not JSON: {e}")))?;
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderFail::Failed("OpenAI /models response has no data array".to_string())
    })?;
    Ok(data
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?.to_string();
            Some(ModelOut {
                display_name: id.clone(),
                context_window: context_window_for(&id),
                id,
            })
        })
        .collect())
}

/// Convert a buffered (non-stream) chat completion into ordered completion
/// chunks: the assistant message content becomes a single terminal chunk
/// carrying the response's token usage when present.
pub fn parse_chat_response(body: &[u8]) -> Result<Vec<ChunkOut>, ProviderFail> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| ProviderFail::Failed(format!("OpenAI chat response is not JSON: {e}")))?;
    let content = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderFail::Failed("OpenAI chat response carried no content".to_string())
        })?;
    Ok(vec![ChunkOut {
        text: content.to_string(),
        finished: true,
        usage: parse_usage(&value),
    }])
}

/// The short, machine-readable `error.code` (preferred) or `error.type` from an
/// OpenAI error body, if it really looks like a code.
///
/// Deliberately NOT `error.message`: OpenAI's auth failures echo the submitted
/// key back ("Incorrect API key provided: sk-…"), and this value crosses into a
/// guest-visible `provider-error`. A tag that is blank, over
/// [`MAX_ERROR_TAG_LEN`], or contains whitespace is prose rather than a code
/// and is dropped.
pub fn error_tag(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    ["code", "type"]
        .iter()
        .filter_map(|field| error.get(*field).and_then(Value::as_str))
        .find(|tag| {
            !tag.is_empty()
                && tag.len() <= MAX_ERROR_TAG_LEN
                && !tag.chars().any(char::is_whitespace)
        })
        .map(str::to_string)
}

/// Map a non-2xx upstream response onto a [`ProviderFail`].
///
/// - `429` -> rate-limited
/// - `5xx` -> unavailable (transient/environmental, never a "bad model" verdict)
/// - a `model_not_found` code -> model-not-found
/// - any other `4xx` (and anything else non-2xx) -> invalid-request
///
/// The rendered message carries only the status and the short
/// [`error_tag`] — never the upstream `message`, which can echo the submitted
/// credential.
pub fn classify_error(status: u16, body: &[u8]) -> ProviderFail {
    let tag = error_tag(body);
    if status == 429 {
        return ProviderFail::RateLimited;
    }
    if status >= 500 {
        return ProviderFail::Unavailable;
    }
    if tag.as_deref() == Some("model_not_found") {
        return ProviderFail::ModelNotFound;
    }
    ProviderFail::InvalidRequest(match tag {
        Some(tag) => format!("OpenAI rejected the request: HTTP {status} ({tag})"),
        None => format!("OpenAI rejected the request: HTTP {status}"),
    })
}

fn parse_usage(value: &Value) -> Option<UsageOut> {
    let usage = value.get("usage")?;
    let input = usage.get("prompt_tokens").and_then(Value::as_u64)? as u32;
    let output = usage.get("completion_tokens").and_then(Value::as_u64)? as u32;
    Some(UsageOut { input, output })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn base_url_defaults_to_the_openai_api_and_honours_a_non_empty_override() {
        assert_eq!(resolve_base_url(None), DEFAULT_BASE_URL);
        assert_eq!(resolve_base_url(Some("")), DEFAULT_BASE_URL);
        assert_eq!(resolve_base_url(Some("   ")), DEFAULT_BASE_URL);
        assert_eq!(
            resolve_base_url(Some("http://127.0.0.1:8080")),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            resolve_base_url(Some("https://proxy.test/v1/")),
            "https://proxy.test/v1",
            "a trailing slash is trimmed so path joins never double up"
        );
    }

    #[test]
    fn endpoint_urls_are_joined_onto_the_resolved_base() {
        assert_eq!(
            models_url(DEFAULT_BASE_URL),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            chat_url(DEFAULT_BASE_URL),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            models_url("http://127.0.0.1:9"),
            "http://127.0.0.1:9/models"
        );
    }

    #[test]
    fn chat_body_maps_the_flat_prompt_to_a_single_user_message() {
        let body: Value =
            serde_json::from_slice(&build_chat_body("gpt-5.2", "ping", None, None)).unwrap();
        assert_eq!(body["model"], "gpt-5.2");
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "the flat ABI carries exactly one turn");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "ping");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn chat_body_uses_max_completion_tokens_not_max_tokens() {
        // The `openai` descriptor declares `uses_max_completion_tokens: true`
        // (crates/core/src/llm_router/registry.rs), so the newer field name is
        // the one that goes on the wire.
        let body: Value =
            serde_json::from_slice(&build_chat_body("gpt-5.2", "hi", Some(64), Some(0.2))).unwrap();
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(
            body.get("max_tokens").is_none(),
            "the legacy field must not be sent alongside it"
        );
        // The WIT temperature is an f32, so the JSON number is its widened
        // value — compare within f32 precision rather than bit-exactly.
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn parse_models_maps_data_ids_with_a_context_window() {
        let body = br#"{"object":"list","data":[
            {"id":"gpt-4o","object":"model"},
            {"id":"gpt-3.5-turbo","object":"model"},
            {"object":"model"},
            {"id":"some-future-model","object":"model"}
        ]}"#;
        let models = parse_models(body).unwrap();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["gpt-4o", "gpt-3.5-turbo", "some-future-model"],
            "entries without a string id are skipped, order is preserved"
        );
        assert_eq!(models[0].display_name, "gpt-4o");
        assert_eq!(models[0].context_window, 128_000);
        assert_eq!(models[1].context_window, 16_385);
        assert_eq!(
            models[2].context_window, DEFAULT_CONTEXT_WINDOW,
            "an unknown model falls back to the conservative default"
        );
    }

    #[test]
    fn parse_models_rejects_a_body_without_a_data_array() {
        assert!(matches!(
            parse_models(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            parse_models(br#"{"object":"list"}"#),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn context_window_prefers_the_most_specific_prefix() {
        assert_eq!(context_window_for("gpt-4o-mini"), 128_000);
        assert_eq!(context_window_for("gpt-4-turbo-2024-04-09"), 128_000);
        assert_eq!(
            context_window_for("gpt-4-0613"),
            8_192,
            "plain gpt-4 must not inherit the gpt-4o window"
        );
        assert_eq!(context_window_for("o1-preview"), 200_000);
        assert_eq!(context_window_for("gpt-5.2"), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn parse_chat_response_yields_one_terminal_chunk_with_usage() {
        let body = br#"{
            "id": "chatcmpl-1",
            "choices": [{"index":0,"message":{"role":"assistant","content":"Hello, world!"},"finish_reason":"stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        }"#;
        let chunks = parse_chat_response(body).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello, world!");
        assert!(chunks[0].finished);
        assert_eq!(
            chunks[0].usage,
            Some(UsageOut {
                input: 7,
                output: 3
            })
        );
    }

    #[test]
    fn parse_chat_response_without_usage_still_succeeds() {
        let chunks = parse_chat_response(br#"{"choices":[{"message":{"content":"hi"}}]}"#).unwrap();
        assert_eq!(chunks[0].text, "hi");
        assert!(chunks[0].finished);
        assert_eq!(chunks[0].usage, None);
    }

    #[test]
    fn parse_chat_response_rejects_a_body_with_no_content() {
        assert!(matches!(
            parse_chat_response(br#"{"choices":[]}"#),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            parse_chat_response(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn classify_error_maps_429_to_rate_limited_and_5xx_to_unavailable() {
        assert_eq!(classify_error(429, b""), ProviderFail::RateLimited);
        for status in [500u16, 502, 503, 504] {
            assert_eq!(classify_error(status, b"boom"), ProviderFail::Unavailable);
        }
    }

    #[test]
    fn classify_error_maps_a_model_not_found_code_to_model_not_found() {
        let body = br#"{"error":{"message":"The model `nope` does not exist","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert_eq!(classify_error(404, body), ProviderFail::ModelNotFound);
    }

    #[test]
    fn classify_error_maps_other_4xx_to_invalid_request() {
        match classify_error(400, br#"{"error":{"type":"invalid_request_error"}}"#) {
            ProviderFail::InvalidRequest(message) => {
                assert!(
                    message.contains("400"),
                    "the status must be reported: {message}"
                );
                assert!(message.contains("invalid_request_error"));
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        // A 404 with no `model_not_found` code stays a plain invalid-request:
        // the router must not persist a bogus "bad model" verdict.
        assert!(matches!(
            classify_error(404, br#"{"error":{"code":"unknown_route"}}"#),
            ProviderFail::InvalidRequest(_)
        ));
    }

    #[test]
    fn a_classified_error_never_echoes_the_upstream_message_or_a_credential() {
        // OpenAI's 401 body echoes a (partially redacted) key. Nothing from the
        // upstream `message` may reach a guest-visible error string.
        let body = br#"{"error":{"message":"Incorrect API key provided: sk-live-ABCDEF. You can find your API key at ...","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        let rendered = format!("{:?}", classify_error(401, body));
        assert!(
            !rendered.contains("sk-live-ABCDEF"),
            "leaked a credential: {rendered}"
        );
        assert!(
            !rendered.contains("Incorrect API key provided"),
            "the upstream message must not be echoed verbatim: {rendered}"
        );
        assert!(
            rendered.contains("invalid_api_key"),
            "the short code is safe to surface"
        );
    }

    #[test]
    fn error_code_and_type_are_extracted_only_from_short_safe_fields() {
        assert_eq!(
            error_tag(br#"{"error":{"code":"rate_limit_exceeded","type":"requests"}}"#).as_deref(),
            Some("rate_limit_exceeded")
        );
        assert_eq!(
            error_tag(br#"{"error":{"type":"server_error"}}"#).as_deref(),
            Some("server_error")
        );
        assert_eq!(error_tag(b"not json"), None);
        assert_eq!(error_tag(br#"{"error":{}}"#), None);
        assert_eq!(
            error_tag(br#"{"error":{"code":"a b c d e f g h i j k l m n o p q r s t u v w x y z 0 1 2 3"}}"#),
            None,
            "an over-long or whitespace-bearing tag is not a machine code and is dropped"
        );
        // Length alone is disqualifying, independent of the whitespace rule.
        let long = "x".repeat(MAX_ERROR_TAG_LEN + 1);
        assert_eq!(
            error_tag(format!(r#"{{"error":{{"code":"{long}"}}}}"#).as_bytes()),
            None
        );
        // ...and a tag exactly at the limit is still accepted.
        let at_limit = "y".repeat(MAX_ERROR_TAG_LEN);
        assert_eq!(
            error_tag(format!(r#"{{"error":{{"code":"{at_limit}"}}}}"#).as_bytes()).as_deref(),
            Some(at_limit.as_str())
        );
    }

    #[test]
    fn success_statuses_are_not_classified_as_errors() {
        assert!(status_is_success(200));
        assert!(status_is_success(299));
        assert!(!status_is_success(300));
        assert!(!status_is_success(199));
    }
}
