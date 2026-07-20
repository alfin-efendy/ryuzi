//! Pure, host-free OpenCode Zen wire logic — ported from `ryuzi-core`'s
//! `llm_router` `opencode-free` descriptor + request-header assembly. Every
//! function is deterministic over its inputs, so the module is fully covered by
//! native `cargo test`; the wasm `guest` glue supplies HTTP and maps these
//! plain types to WIT.

use serde_json::{Map, Value};

/// OpenCode Zen free-tier base (from the `opencode-free` descriptor).
pub const BASE_URL: &str = "https://opencode.ai/zen/v1";

/// Chat endpoint (OpenAI-compatible `/chat/completions`).
pub const CHAT_URL: &str = "https://opencode.ai/zen/v1/chat/completions";

/// Model-discovery endpoint (`has_models_endpoint: true`).
pub const MODELS_URL: &str = "https://opencode.ai/zen/v1/models";

/// The static free-tier bearer (`llm_router::client`'s `opencode-free` header).
pub const BEARER: &str = "public";

/// The client tag OpenCode's free tier expects.
pub const X_OPENCODE_CLIENT: &str = "desktop";

/// Fallback context window used when the `/models` entry does not report one
/// (OpenAI `/models` entries usually omit it). A usable default keeps the
/// router's routing/budgeting sane rather than advertising a zero window.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// One model the provider advertises (host-free mirror of WIT `model-info`).
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

/// The shared request headers for both `/models` and `/chat/completions`: the
/// static bearer plus the OpenCode client tag. The host forwards this
/// `authorization` header only because this is a VERIFIED first-party bundle.
pub fn request_headers() -> Vec<(String, String)> {
    vec![
        ("authorization".to_string(), format!("Bearer {BEARER}")),
        (
            "x-opencode-client".to_string(),
            X_OPENCODE_CLIENT.to_string(),
        ),
        ("accept".to_string(), "application/json".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ]
}

/// Build the OpenAI-format chat request body for a flat prompt. OpenCode needs
/// no gate/system marker, so the prompt is sent as a single user message.
/// `stream` is false: the host HTTP capability is buffered, so the guest asks
/// for the whole completion and emits it as chunks.
pub fn build_chat_body(
    model: &str,
    prompt: &str,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> Vec<u8> {
    let mut user = Map::new();
    user.insert("role".to_string(), Value::String("user".to_string()));
    user.insert("content".to_string(), Value::String(prompt.to_string()));

    let mut obj = Map::new();
    obj.insert("model".to_string(), Value::String(model.to_string()));
    obj.insert(
        "messages".to_string(),
        Value::Array(vec![Value::Object(user)]),
    );
    obj.insert("stream".to_string(), Value::Bool(false));
    if let Some(max) = max_tokens {
        obj.insert("max_tokens".to_string(), Value::from(max));
    }
    if let Some(temp) = temperature {
        if let Some(number) = serde_json::Number::from_f64(temp as f64) {
            obj.insert("temperature".to_string(), Value::Number(number));
        }
    }
    serde_json::to_vec(&Value::Object(obj)).expect("chat body always serializes")
}

/// Parse an OpenAI-style `/models` response (`{"data":[{"id":...}]}`) into the
/// advertised model list. A per-entry `context_length`/`context_window` is used
/// when present, else [`DEFAULT_CONTEXT_WINDOW`]. Entries without a string `id`
/// are skipped.
pub fn parse_models(body: &[u8]) -> Result<Vec<ModelOut>, ProviderFail> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| ProviderFail::Failed(format!("OpenCode /models response is not JSON: {e}")))?;
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderFail::Failed("OpenCode /models response has no data array".to_string())
    })?;
    let models = data
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?.to_string();
            let context_window = entry
                .get("context_length")
                .or_else(|| entry.get("context_window"))
                .and_then(Value::as_u64)
                .map(|w| w as u32)
                .unwrap_or(DEFAULT_CONTEXT_WINDOW);
            Some(ModelOut {
                display_name: id.clone(),
                id,
                context_window,
            })
        })
        .collect();
    Ok(models)
}

/// Convert a buffered (non-stream) OpenAI chat completion into ordered
/// completion chunks — the assistant content as a single finished chunk with
/// usage when present.
pub fn parse_chat_response(body: &[u8]) -> Result<Vec<ChunkOut>, ProviderFail> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| ProviderFail::Failed(format!("OpenCode chat response is not JSON: {e}")))?;
    let content = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderFail::Failed("OpenCode chat response carried no content".to_string())
        })?;
    Ok(vec![ChunkOut {
        text: content.to_string(),
        finished: true,
        usage: parse_usage(&value),
    }])
}

/// Map a non-2xx chat response to a [`ProviderFail`]. OpenCode's free tier has
/// no bespoke transient-block protocol, so a 429 is a rate limit and everything
/// else is a generic failure carrying the status (never `model-not-found`, so a
/// transient hiccup is not persisted as a bad model).
pub fn classify_chat_error(status: u16, _body: &[u8]) -> ProviderFail {
    if status == 429 {
        ProviderFail::RateLimited
    } else {
        ProviderFail::Failed(format!("OpenCode chat failed: HTTP {status}"))
    }
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
    fn request_headers_carry_the_static_bearer_and_client_tag() {
        let headers = request_headers();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("authorization"), Some("Bearer public"));
        assert_eq!(get("x-opencode-client"), Some("desktop"));
        assert_eq!(get("content-type"), Some("application/json"));
    }

    #[test]
    fn chat_body_sends_the_prompt_as_a_single_user_message() {
        let body: Value =
            serde_json::from_slice(&build_chat_body("some-model", "ping", Some(32), Some(0.5)))
                .unwrap();
        assert_eq!(body["model"], "some-model");
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], 32);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "no system/gate message for OpenCode");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "ping");
    }

    #[test]
    fn chat_body_omits_optional_fields_when_absent() {
        let body: Value = serde_json::from_slice(&build_chat_body("m", "hi", None, None)).unwrap();
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn parse_models_reads_ids_and_context_windows() {
        let body = br#"{"data":[
            {"id":"claude-3-5-sonnet","context_length":200000},
            {"id":"grok-code","context_window":256000},
            {"id":"no-window"}
        ]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "claude-3-5-sonnet");
        assert_eq!(models[0].display_name, "claude-3-5-sonnet");
        assert_eq!(models[0].context_window, 200000);
        assert_eq!(models[1].context_window, 256000);
        assert_eq!(
            models[2].context_window, DEFAULT_CONTEXT_WINDOW,
            "a model without a reported window gets the default"
        );
    }

    #[test]
    fn parse_models_skips_entries_without_a_string_id_and_rejects_bad_shapes() {
        let body = br#"{"data":[{"id":"ok"},{"noid":1},{"id":123}]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ok");

        assert!(matches!(
            parse_models(br#"{"nope":1}"#),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            parse_models(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn parse_chat_response_yields_one_finished_chunk_with_usage() {
        let body = br#"{
            "choices":[{"message":{"role":"assistant","content":"Hi there"}}],
            "usage":{"prompt_tokens":5,"completion_tokens":2}
        }"#;
        let chunks = parse_chat_response(body).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hi there");
        assert!(chunks[0].finished);
        assert_eq!(
            chunks[0].usage,
            Some(UsageOut {
                input: 5,
                output: 2
            })
        );
    }

    #[test]
    fn parse_chat_response_without_usage_still_succeeds() {
        let chunks = parse_chat_response(br#"{"choices":[{"message":{"content":"ok"}}]}"#).unwrap();
        assert_eq!(chunks[0].text, "ok");
        assert_eq!(chunks[0].usage, None);
    }

    #[test]
    fn parse_chat_response_rejects_a_body_without_content() {
        assert!(matches!(
            parse_chat_response(br#"{"choices":[]}"#),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn classify_chat_error_maps_429_to_rate_limited_else_failed() {
        assert_eq!(classify_chat_error(429, b""), ProviderFail::RateLimited);
        match classify_chat_error(503, b"down") {
            ProviderFail::Failed(msg) => assert!(msg.contains("503")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
