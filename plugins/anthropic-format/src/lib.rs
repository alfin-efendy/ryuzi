//! Shared, host-free Anthropic **Messages** wire logic for Ryuzi's first-party
//! Anthropic provider components.
//!
//! Two provider descriptors in `crates/core/src/llm_router/registry.rs` declare
//! `format: ApiFormat::Anthropic` — `anthropic` (an `x-api-key` API-key
//! provider) and `anthropic-oauth` (a host-managed OAuth provider). They speak
//! the IDENTICAL `/messages` + `/models` wire shape and differ only in which
//! egress capability the guest calls (`ryuzi:provider-auth` vs `ryuzi:oauth`).
//! This crate owns everything they have in COMMON — base-URL override
//! resolution, request-body shaping from the flat provider ABI, `/models` and
//! message-response parsing, and upstream-status -> `provider-error`
//! classification — so that logic is written, reviewed and tested ONCE instead
//! of once per component.
//!
//! What stays per-provider is the data that provider's `ProviderDescriptor`
//! already carries (see [`AnthropicFormat`], a config transcription of the
//! descriptor), the egress capability its guest calls, and — for the OAuth
//! variant — the Claude-subscription auth markers (`anthropic-beta` flag and the
//! Claude-Code system prompt), which live in that component, not here.
//!
//! # Why this is not built on `ryuzi-openai-format`
//! That crate is the OpenAI-chat format: `messages[].content` as a string,
//! `choices[0].message.content` back, `usage.prompt_tokens`/`completion_tokens`,
//! an `error.code` vocabulary. Anthropic differs in every one of those — a
//! required `max_tokens`, `content[]` blocks out, `usage.input_tokens`/
//! `output_tokens`, `error.type` — so sharing would mean a format flag on every
//! function rather than shared behaviour. Anthropic is its own shape, so it gets
//! its own shared crate, extracted exactly the way `ryuzi-openai-format` was.
//!
//! # Nothing here touches a credential
//! The Anthropic provider components authenticate host-side: the API-key variant
//! through `ryuzi:provider-auth` (the host injects `x-api-key`), the OAuth
//! variant through `ryuzi:oauth` (the host injects the bearer). No function in
//! this crate sees, stores, or renders one — and [`error_tag`] exists
//! specifically to keep upstream error PROSE (which can echo a submitted key)
//! out of the guest-visible error string.
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text: a `prompt` string in, text chunks
//! out. Every component built on this crate therefore supports plain text
//! completion only — no tool calling, no structured multi-turn messages, no
//! multimodal content, and no true token streaming (the single buffered upstream
//! response is returned as one terminal chunk). That is a deliberate, accepted
//! tradeoff of the WASM provider migration, not an oversight. The OAuth variant's
//! optional system prompt (its Claude-subscription auth marker) is the one
//! structured field this flat ABI carries, injected by the component — see
//! [`AnthropicFormat::build_messages_body`]'s `system` argument.

use serde_json::{Map, Value};

/// Key in a component's (host-scoped) `ryuzi:storage` slice holding an OPTIONAL
/// base-URL override — the same product-level affordance every provider
/// component exposes (pointing at a compatible gateway, and letting the provider
/// conformance harness aim the component at a loopback mock). A blank/whitespace
/// value is treated as "unset". The manifest network allowlist still governs
/// whatever the override resolves to, so an override can never widen where the
/// user's credential may travel.
pub const BASE_URL_STORAGE_KEY: &str = "base-url";

/// The Anthropic API version this format pins, sent as the `anthropic-version`
/// request header on every call.
///
/// Anthropic REQUIRES this header and treats it as the contract version for
/// request and response shapes, so it must be a value chosen deliberately rather
/// than tracked implicitly. `2023-06-01` is the current stable Messages version
/// and is the SAME value the native router path already pins
/// (`llm_router::client::oauth_upstream_request`,
/// `llm_router::models::models_request`), so the components and the native path
/// cannot interpret the same account differently. It is a protocol version, not
/// a credential: the guest sets it, the host forwards it.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `max_tokens` sent when the flat provider ABI's `max-tokens` is absent.
///
/// Unlike OpenAI-chat, Anthropic REQUIRES `max_tokens` on every `/messages`
/// request — omitting it is a hard 400, so "leave it out when the caller did not
/// ask" is not an option here and a default must be picked. This matches the
/// default the engine's own OpenAI->Anthropic request translation already
/// injects for exactly this reason (`llm_router::translate::
/// openai_to_anthropic_request`), so a caller that specifies nothing gets the
/// same cap through the component that it gets through the native path.
///
/// It is a cap, not a target: a shorter completion still stops on its own. The
/// cost of it being low is a truncated long answer; the cost of it being high is
/// nothing until a model actually generates that much. 4096 is the conservative
/// middle the rest of this codebase already settled on.
pub const DEFAULT_MAX_TOKENS: u32 = 4_096;

/// Context window advertised for a model no static table covers.
///
/// Anthropic's `/models` response carries no context length (`id`,
/// `display_name`, `created_at`, `type`), so a window is either a static hint or
/// a guess. This is the conservative hint, and it deliberately mirrors the value
/// the router itself already falls back to
/// (`llm_router::model_meta::FALLBACK.context_window`) rather than introducing a
/// second, differently-wrong default.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// Longest an `error.type` tag may be before it stops looking like a
/// machine-readable code and starts looking like prose that could carry
/// upstream-echoed request material. See [`error_tag`].
const MAX_ERROR_TAG_LEN: usize = 64;

/// The Anthropic `error.type` that means "that model does not exist". Anthropic
/// returns it for any unknown resource, but on these components' only two
/// endpoints (`/models` and `/messages`) the resource in question is the model.
const MODEL_NOT_FOUND_TYPE: &str = "not_found_error";

/// Everything that differs between two Anthropic-Messages providers.
///
/// Every field is DATA the provider's `ProviderDescriptor`
/// (`crates/core/src/llm_router/registry.rs`) already states, so the config is a
/// transcription of the descriptor rather than an independent guess. The struct
/// exists (rather than bare constants) so a second Anthropic-shaped provider —
/// the OAuth variant, a gateway — is a new config value, not a fork of this
/// module.
pub struct AnthropicFormat {
    /// Human-readable provider name used in guest-visible error strings.
    /// Never a credential.
    pub provider_label: &'static str,
    /// The descriptor's `base_url`. Used unless the component's storage slice
    /// carries an override at [`BASE_URL_STORAGE_KEY`].
    pub default_base_url: &'static str,
    /// Model-discovery path appended to the resolved base. Only meaningful for
    /// a descriptor with `has_models_endpoint: true`.
    pub models_path: &'static str,
    /// Message-generation path appended to the resolved base.
    pub messages_path: &'static str,
    /// `max_tokens` when the ABI carries none — see [`DEFAULT_MAX_TOKENS`].
    pub default_max_tokens: u32,
    /// Static context-window hints by model-id PREFIX, scanned IN ORDER so the
    /// most specific prefix must be listed first. Empty for a provider with no
    /// published per-family values worth pinning.
    pub context_windows: &'static [(&'static str, u32)],
    /// Window for a model [`Self::context_windows`] does not cover.
    pub default_context_window: u32,
}

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

impl AnthropicFormat {
    /// The upstream base for this call: a non-blank stored override, else
    /// [`Self::default_base_url`]. Any trailing `/` is trimmed so path joins
    /// never produce a doubled separator.
    pub fn resolve_base_url(&self, stored: Option<&str>) -> String {
        let base = stored
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.default_base_url);
        base.trim_end_matches('/').to_string()
    }

    /// `<base><models_path>` — the model-discovery endpoint.
    pub fn models_url(&self, base: &str) -> String {
        format!("{base}{}", self.models_path)
    }

    /// `<base><messages_path>` — the message-generation endpoint.
    pub fn messages_url(&self, base: &str) -> String {
        format!("{base}{}", self.messages_path)
    }

    /// The static context window for `model_id` — the first matching prefix in
    /// [`Self::context_windows`], else [`Self::default_context_window`].
    pub fn context_window_for(&self, model_id: &str) -> u32 {
        self.context_windows
            .iter()
            .find(|(prefix, _)| model_id.starts_with(prefix))
            .map(|(_, window)| *window)
            .unwrap_or(self.default_context_window)
    }

    /// Build the NON-STREAMING `/messages` body for a flat prompt.
    ///
    /// The `ryuzi:provider/provider` ABI carries a single `prompt` string, so
    /// the request is one `user` turn with STRING content — no tools, no content
    /// blocks. `stream` is false because the host capability is a buffered
    /// request/response: the component asks for the whole message and returns it
    /// as one terminal chunk.
    ///
    /// `system` is the one structured field the flat ABI carries: when `Some`,
    /// a single leading `text` block is emitted as the request's `system`
    /// (`[{"type":"text","text":<system>}]`, the array form Anthropic accepts).
    /// The `x-api-key` component passes `None`; the OAuth component passes its
    /// Claude-subscription auth marker here, so the acceptance-required system
    /// prompt is part of the request BODY the component builds rather than a
    /// header the host would strip. When `None`, no `system` key is emitted at
    /// all — the API-key path is byte-for-byte unchanged.
    ///
    /// `max_tokens` is ALWAYS present, falling back to
    /// [`Self::default_max_tokens`] — Anthropic rejects a request without it.
    ///
    /// `temperature` is OMITTED when it is not finite (NaN/±inf): JSON has no
    /// representation for those values, so there is nothing to send. The request
    /// still goes out and the upstream applies its own default — failing an
    /// entire completion over an unrepresentable optional tuning knob would be
    /// the worse trade.
    pub fn build_messages_body(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        system: Option<&str>,
    ) -> Vec<u8> {
        let mut message = Map::new();
        message.insert("role".to_string(), Value::String("user".to_string()));
        message.insert("content".to_string(), Value::String(prompt.to_string()));

        let mut obj = Map::new();
        obj.insert("model".to_string(), Value::String(model.to_string()));
        obj.insert(
            "max_tokens".to_string(),
            Value::from(max_tokens.unwrap_or(self.default_max_tokens)),
        );
        obj.insert(
            "messages".to_string(),
            Value::Array(vec![Value::Object(message)]),
        );
        obj.insert("stream".to_string(), Value::Bool(false));
        if let Some(system) = system {
            let block = serde_json::json!({"type": "text", "text": system});
            obj.insert("system".to_string(), Value::Array(vec![block]));
        }
        if let Some(temp) = temperature {
            if let Some(number) = serde_json::Number::from_f64(temp as f64) {
                obj.insert("temperature".to_string(), Value::Number(number));
            }
        }
        serde_json::to_vec(&Value::Object(obj)).expect("messages body always serializes")
    }

    /// Parse an Anthropic `/models` response
    /// (`{"data":[{"type":"model","id":...,"display_name":...}]}`) into the
    /// advertised model list, preserving the served order.
    ///
    /// Unlike the OpenAI-format listing, Anthropic DOES carry a human display
    /// name, so it is used when present and the id stands in when it is not.
    /// The response still carries no context length, so the window comes from
    /// [`Self::context_window_for`]. Entries without a string `id` are skipped
    /// rather than failing the whole listing.
    pub fn parse_models(&self, body: &[u8]) -> Result<Vec<ModelOut>, ProviderFail> {
        let label = self.provider_label;
        let value: Value = serde_json::from_slice(body).map_err(|e| {
            ProviderFail::Failed(format!("{label} /models response is not JSON: {e}"))
        })?;
        let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
            ProviderFail::Failed(format!("{label} /models response has no data array"))
        })?;
        Ok(data
            .iter()
            .filter_map(|entry| {
                let id = entry.get("id").and_then(Value::as_str)?.to_string();
                let display_name = entry
                    .get("display_name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&id)
                    .to_string();
                Some(ModelOut {
                    display_name,
                    context_window: self.context_window_for(&id),
                    id,
                })
            })
            .collect())
    }

    /// Convert a buffered (non-stream) `/messages` response into ordered
    /// completion chunks: the FIRST `text` block of `content[]` becomes a single
    /// terminal chunk carrying the response's token usage when present.
    ///
    /// Anthropic returns `content` as an array of typed blocks. Only `text`
    /// blocks carry prose; a `thinking` or `tool_use` block that precedes one is
    /// skipped rather than rendered, so a reasoning model's private thinking is
    /// never surfaced as the completion. The flat ABI has no representation for
    /// anything past the first text block, which is a known consequence of that
    /// ABI (a plain, tool-free request returns exactly one text block).
    pub fn parse_message_response(&self, body: &[u8]) -> Result<Vec<ChunkOut>, ProviderFail> {
        let label = self.provider_label;
        let value: Value = serde_json::from_slice(body).map_err(|e| {
            ProviderFail::Failed(format!("{label} message response is not JSON: {e}"))
        })?;
        let text = value
            .get("content")
            .and_then(Value::as_array)
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            })
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderFail::Failed(format!("{label} message response carried no text block"))
            })?;
        Ok(vec![ChunkOut {
            text: text.to_string(),
            finished: true,
            usage: parse_usage(&value),
        }])
    }

    /// Map a non-2xx upstream response onto a [`ProviderFail`].
    ///
    /// - `429` -> rate-limited
    /// - `5xx` -> unavailable (transient/environmental, never a "bad model" verdict)
    /// - a `not_found_error` type -> model-not-found
    /// - any other `4xx` (and anything else non-2xx) -> invalid-request
    ///
    /// The rendered message carries only the provider label, the status and the
    /// short [`error_tag`] — never the upstream `message`, which can echo the
    /// submitted credential.
    pub fn classify_error(&self, status: u16, body: &[u8]) -> ProviderFail {
        let label = self.provider_label;
        let tag = error_tag(body);
        if status == 429 {
            return ProviderFail::RateLimited;
        }
        if status >= 500 {
            return ProviderFail::Unavailable;
        }
        if tag.as_deref() == Some(MODEL_NOT_FOUND_TYPE) {
            return ProviderFail::ModelNotFound;
        }
        ProviderFail::InvalidRequest(match tag {
            Some(tag) => format!("{label} rejected the request: HTTP {status} ({tag})"),
            None => format!("{label} rejected the request: HTTP {status}"),
        })
    }
}

/// Whether an upstream status is a success (and so parsed rather than classified
/// as an error).
pub fn status_is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// The short, machine-readable `error.type` from an Anthropic error body
/// (`{"type":"error","error":{"type":"...","message":"..."}}`), if it really
/// looks like a code.
///
/// Deliberately NOT `error.message`: Anthropic's authentication failures quote
/// the submitted key back in that prose, and this value crosses into a
/// guest-visible `provider-error`. A tag that is blank, over
/// [`MAX_ERROR_TAG_LEN`], or contains whitespace is prose rather than a code and
/// is dropped.
///
/// This filter is this crate's one non-obvious security-relevant behaviour: it
/// is what makes [`AnthropicFormat::classify_error`] safe to surface. It is the
/// same rule `ryuzi_openai_format::error_tag` applies, restated for THIS wire
/// shape rather than shared, because the field it reads (`error.type`, no
/// `error.code`) and the code vocabulary it recognizes are Anthropic's.
pub fn error_tag(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let tag = value.get("error")?.get("type")?.as_str()?;
    (!tag.is_empty() && tag.len() <= MAX_ERROR_TAG_LEN && !tag.chars().any(char::is_whitespace))
        .then(|| tag.to_string())
}

/// Anthropic reports usage as `input_tokens`/`output_tokens` (the OpenAI shape
/// says `prompt_tokens`/`completion_tokens`).
fn parse_usage(value: &Value) -> Option<UsageOut> {
    let usage = value.get("usage")?;
    let input = usage.get("input_tokens").and_then(Value::as_u64)?;
    let output = usage.get("output_tokens").and_then(Value::as_u64)?;
    Some(UsageOut {
        input: saturating_u32(input),
        output: saturating_u32(output),
    })
}

/// Narrow a JSON-wide `u64` token count to the WIT `token-usage`'s `u32`,
/// SATURATING rather than wrapping. A wrapping cast would turn an absurd (or
/// hostile) upstream count into a small plausible one and silently under-report
/// spend to the router; clamping at least stays monotonic and obviously extreme.
fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `anthropic` descriptor's config, transcribed — an EMPTY
    /// context-window table (Anthropic's `/models` carries no context length and
    /// the descriptor pins no per-model windows). The `anthropic-oauth`
    /// descriptor shares these exact wire values, differing only in egress.
    const ANTHROPIC: AnthropicFormat = AnthropicFormat {
        provider_label: "Anthropic",
        default_base_url: "https://api.anthropic.com/v1",
        models_path: "/models",
        messages_path: "/messages",
        default_max_tokens: DEFAULT_MAX_TOKENS,
        context_windows: &[],
        default_context_window: DEFAULT_CONTEXT_WINDOW,
    };

    /// A deliberately DIFFERENT config in every dimension the struct exposes —
    /// label, base, both paths, token default, non-default window. Its purpose
    /// is anti-tautology: assertions run against both configs, so a function that
    /// ignored `self` and hardcoded Anthropic's values would fail here even
    /// though it passed against [`ANTHROPIC`].
    const OTHER: AnthropicFormat = AnthropicFormat {
        provider_label: "Contoso",
        default_base_url: "https://api.contoso.test/anthropic/v2",
        models_path: "/model-list",
        messages_path: "/chat",
        default_max_tokens: 77,
        context_windows: &[("claude-opus", 200_000)],
        default_context_window: 32_768,
    };

    #[test]
    fn format_produces_the_expected_anthropic_endpoints() {
        assert_eq!(
            ANTHROPIC.resolve_base_url(None),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            ANTHROPIC.messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages",
            "ApiFormat::Anthropic generates at /messages, not /chat/completions",
        );
        assert_eq!(
            ANTHROPIC.models_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(ANTHROPIC.provider_label, "Anthropic");
        assert_eq!(ANTHROPIC_VERSION, "2023-06-01");
    }

    #[test]
    fn base_url_defaults_to_the_configured_api_and_honours_a_non_empty_override() {
        assert_eq!(
            ANTHROPIC.resolve_base_url(Some("")),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            ANTHROPIC.resolve_base_url(Some("   ")),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            OTHER.resolve_base_url(None),
            "https://api.contoso.test/anthropic/v2",
            "the default must come from the config, not a hardcoded vendor",
        );
        assert_eq!(
            ANTHROPIC.resolve_base_url(Some("http://127.0.0.1:8080")),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            ANTHROPIC.resolve_base_url(Some("https://gateway.test/v1/")),
            "https://gateway.test/v1",
            "a trailing slash is trimmed so path joins never double up",
        );
        assert_eq!(
            OTHER.messages_url(&OTHER.resolve_base_url(None)),
            "https://api.contoso.test/anthropic/v2/chat"
        );
        assert_eq!(
            OTHER.models_url(&OTHER.resolve_base_url(None)),
            "https://api.contoso.test/anthropic/v2/model-list"
        );
    }

    #[test]
    fn messages_body_maps_the_flat_prompt_to_a_single_user_turn() {
        let body: Value = serde_json::from_slice(&ANTHROPIC.build_messages_body(
            "claude-sonnet-4-5",
            "ping",
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "the flat ABI carries exactly one turn");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(
            messages[0]["content"], "ping",
            "content is a plain string, not a block array",
        );
        assert!(body.get("temperature").is_none());
        assert!(
            body.get("system").is_none(),
            "with no system argument the API-key request carries no system turn",
        );
    }

    #[test]
    fn messages_body_emits_a_leading_system_text_block_only_when_asked() {
        // The OAuth variant's Claude-subscription auth marker travels in the
        // request BODY, as the array form Anthropic accepts. The API-key variant
        // (system = None) must still emit no `system` key at all.
        let with_system: Value = serde_json::from_slice(&ANTHROPIC.build_messages_body(
            "claude-opus-4-5",
            "ping",
            None,
            None,
            Some("You are Claude Code, Anthropic's official CLI for Claude."),
        ))
        .unwrap();
        assert_eq!(
            with_system["system"],
            serde_json::json!([
                {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}
            ]),
            "the system marker is a single leading text block, not a bare string",
        );
        // The user turn is untouched by the system marker.
        assert_eq!(with_system["messages"][0]["role"], "user");
        assert_eq!(with_system["messages"][0]["content"], "ping");
        assert_eq!(with_system["max_tokens"], DEFAULT_MAX_TOKENS);

        // An empty string is still a system block (it is a caller's explicit
        // choice); only `None` omits the field.
        let empty: Value =
            serde_json::from_slice(&ANTHROPIC.build_messages_body("m", "hi", None, None, Some("")))
                .unwrap();
        assert_eq!(
            empty["system"],
            serde_json::json!([{"type": "text", "text": ""}])
        );
    }

    #[test]
    fn messages_body_always_carries_max_tokens_defaulting_when_the_abi_omits_it() {
        // Anthropic REJECTS a request without max_tokens, so unlike the
        // OpenAI-format components this field can never be omitted.
        let defaulted: Value =
            serde_json::from_slice(&ANTHROPIC.build_messages_body("m", "hi", None, None, None))
                .unwrap();
        assert_eq!(defaulted["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(DEFAULT_MAX_TOKENS, 4_096);

        let requested: Value = serde_json::from_slice(&ANTHROPIC.build_messages_body(
            "m",
            "hi",
            Some(64),
            Some(0.2),
            None,
        ))
        .unwrap();
        assert_eq!(
            requested["max_tokens"], 64,
            "a caller-supplied cap must win over the default",
        );
        // The WIT temperature is an f32, so the JSON number is its widened
        // value — compare within f32 precision rather than bit-exactly.
        assert!((requested["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);

        // ...and the default is the config's, not a module-level constant baked
        // into the builder.
        let other: Value =
            serde_json::from_slice(&OTHER.build_messages_body("m", "hi", None, None, None))
                .unwrap();
        assert_eq!(other["max_tokens"], 77);
    }

    #[test]
    fn messages_body_drops_a_non_finite_temperature_rather_than_failing() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let body: Value = serde_json::from_slice(&ANTHROPIC.build_messages_body(
                "m",
                "hi",
                None,
                Some(bad),
                None,
            ))
            .unwrap();
            assert!(
                body.get("temperature").is_none(),
                "a non-finite temperature ({bad}) must be omitted, not serialized",
            );
            assert_eq!(
                body["messages"][0]["content"], "hi",
                "the request still goes"
            );
            assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        }
    }

    #[test]
    fn parse_models_maps_data_entries_preferring_anthropics_display_name() {
        let body = br#"{"data":[
            {"type":"model","id":"claude-opus-4-5","display_name":"Claude Opus 4.5","created_at":"2025-01-01T00:00:00Z"},
            {"type":"model","id":"claude-haiku-4-5"},
            {"type":"model","display_name":"nameless"},
            {"type":"model","id":"claude-sonnet-4-5","display_name":""}
        ],"has_more":false}"#;
        let models = ANTHROPIC.parse_models(body).unwrap();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude-opus-4-5", "claude-haiku-4-5", "claude-sonnet-4-5"],
            "entries without a string id are skipped, order is preserved",
        );
        assert_eq!(
            models[0].display_name, "Claude Opus 4.5",
            "Anthropic's /models DOES carry a display name — use it",
        );
        assert_eq!(
            models[1].display_name, "claude-haiku-4-5",
            "the id stands in when no display name is served",
        );
        assert_eq!(
            models[2].display_name, "claude-sonnet-4-5",
            "an EMPTY display name is not a name",
        );
        for model in &models {
            assert_eq!(model.context_window, DEFAULT_CONTEXT_WINDOW);
        }
    }

    #[test]
    fn parse_models_uses_the_configs_own_window_table() {
        // Same body, different config: the window must come from the config's
        // table/default, never from a value baked into the parser.
        let body = br#"{"data":[{"id":"claude-opus-4-5"},{"id":"claude-haiku-4-5"}]}"#;
        let models = OTHER.parse_models(body).unwrap();
        assert_eq!(
            models.iter().map(|m| m.context_window).collect::<Vec<_>>(),
            vec![200_000, 32_768],
            "a prefix hit takes the table value, a miss the config's default",
        );
    }

    #[test]
    fn parse_models_rejects_a_body_without_a_data_array() {
        assert!(matches!(
            ANTHROPIC.parse_models(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            ANTHROPIC.parse_models(br#"{"has_more":false}"#),
            Err(ProviderFail::Failed(_))
        ));
        match OTHER.parse_models(b"{}") {
            Err(ProviderFail::Failed(message)) => assert!(message.contains("Contoso"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn parse_message_response_yields_one_terminal_chunk_with_anthropic_usage() {
        let body = br#"{
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type":"text","text":"Hello, world!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 7, "output_tokens": 3}
        }"#;
        let chunks = ANTHROPIC.parse_message_response(body).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello, world!");
        assert!(chunks[0].finished);
        assert_eq!(
            chunks[0].usage,
            Some(UsageOut {
                input: 7,
                output: 3
            }),
            "usage is input_tokens/output_tokens, NOT prompt_/completion_tokens",
        );
    }

    #[test]
    fn parse_message_response_skips_non_text_blocks_and_never_surfaces_thinking() {
        let body = br#"{
            "content": [
                {"type":"thinking","thinking":"secret chain of thought"},
                {"type":"text","text":"the answer"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }"#;
        let chunks = ANTHROPIC.parse_message_response(body).unwrap();
        assert_eq!(chunks[0].text, "the answer");
        assert!(
            !chunks[0].text.contains("secret chain of thought"),
            "a thinking block must never be rendered as the completion",
        );
    }

    #[test]
    fn parse_message_response_ignores_an_openai_shaped_body() {
        // Guards against the two formats being crossed: an OpenAI chat completion
        // has `choices`, no `content[]`, and must NOT parse here.
        let body = br#"{"choices":[{"message":{"role":"assistant","content":"hi"}}],
                        "usage":{"prompt_tokens":1,"completion_tokens":2}}"#;
        assert!(matches!(
            ANTHROPIC.parse_message_response(body),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn parse_message_response_saturates_a_usage_count_that_exceeds_u32() {
        // The WIT `token-usage` fields are u32 but JSON numbers are u64-wide. An
        // absurd/hostile count must SATURATE, never wrap: a wrapping cast would
        // turn 5_000_000_000 into 705_032_704 and silently under-report spend.
        let body = br#"{
            "content":[{"type":"text","text":"hi"}],
            "usage":{"input_tokens":5000000000,"output_tokens":4294967296}
        }"#;
        let chunks = ANTHROPIC.parse_message_response(body).unwrap();
        assert_eq!(
            chunks[0].usage,
            Some(UsageOut {
                input: u32::MAX,
                output: u32::MAX
            })
        );
    }

    #[test]
    fn parse_message_response_without_usage_still_succeeds() {
        let chunks = ANTHROPIC
            .parse_message_response(br#"{"content":[{"type":"text","text":"hi"}]}"#)
            .unwrap();
        assert_eq!(chunks[0].text, "hi");
        assert!(chunks[0].finished);
        assert_eq!(chunks[0].usage, None);
    }

    #[test]
    fn parse_message_response_rejects_a_body_with_no_text_block() {
        assert!(matches!(
            ANTHROPIC.parse_message_response(br#"{"content":[]}"#),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            ANTHROPIC.parse_message_response(br#"{"content":[{"type":"tool_use","id":"t1"}]}"#),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            ANTHROPIC.parse_message_response(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn classify_error_maps_429_to_rate_limited_and_5xx_to_unavailable() {
        assert_eq!(
            ANTHROPIC.classify_error(429, b""),
            ProviderFail::RateLimited
        );
        assert_eq!(
            ANTHROPIC.classify_error(
                429,
                br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#
            ),
            ProviderFail::RateLimited
        );
        for status in [500u16, 502, 503, 529] {
            assert_eq!(
                ANTHROPIC.classify_error(
                    status,
                    br#"{"type":"error","error":{"type":"overloaded_error"}}"#
                ),
                ProviderFail::Unavailable,
                "status {status}",
            );
        }
    }

    #[test]
    fn classify_error_maps_anthropics_not_found_type_to_model_not_found() {
        let body =
            br#"{"type":"error","error":{"type":"not_found_error","message":"model: nope"}}"#;
        assert_eq!(
            ANTHROPIC.classify_error(404, body),
            ProviderFail::ModelNotFound
        );
        // A 404 with some other type stays a plain invalid-request: the router
        // must not persist a bogus "bad model" verdict.
        assert!(matches!(
            ANTHROPIC.classify_error(404, br#"{"type":"error","error":{"type":"unknown_route"}}"#),
            ProviderFail::InvalidRequest(_)
        ));
    }

    #[test]
    fn classify_error_maps_other_4xx_to_invalid_request_naming_the_provider() {
        match ANTHROPIC.classify_error(
            400,
            br#"{"type":"error","error":{"type":"invalid_request_error"}}"#,
        ) {
            ProviderFail::InvalidRequest(message) => {
                assert!(
                    message.contains("400"),
                    "the status must be reported: {message}"
                );
                assert!(message.contains("invalid_request_error"));
                assert!(message.contains("Anthropic"));
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        // The same status through a different config names THAT provider.
        match OTHER.classify_error(403, b"") {
            ProviderFail::InvalidRequest(message) => {
                assert!(message.contains("Contoso"), "{message}");
                assert!(!message.contains("Anthropic"), "{message}");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn a_classified_error_never_echoes_the_upstream_message_or_a_credential() {
        // Anthropic's 401 body puts prose in `error.message`, and that prose can
        // quote the submitted key. Nothing from it may reach a guest-visible
        // error string.
        let body = br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key: sk-ant-api03-LIVEKEY"}}"#;
        let rendered = format!("{:?}", ANTHROPIC.classify_error(401, body));
        assert!(
            !rendered.contains("sk-ant-api03-LIVEKEY"),
            "leaked a credential: {rendered}",
        );
        assert!(
            !rendered.contains("invalid x-api-key"),
            "the upstream message must not be echoed verbatim: {rendered}",
        );
        assert!(
            rendered.contains("authentication_error"),
            "the short type is safe to surface",
        );
    }

    #[test]
    fn error_type_is_extracted_only_from_a_short_safe_field() {
        assert_eq!(
            error_tag(br#"{"type":"error","error":{"type":"overloaded_error"}}"#).as_deref(),
            Some("overloaded_error")
        );
        assert_eq!(
            error_tag(br#"{"type":"error","error":{"message":"boom"}}"#),
            None,
            "the prose message is never a tag",
        );
        assert_eq!(error_tag(b"not json"), None);
        assert_eq!(error_tag(br#"{"error":{}}"#), None);
        assert_eq!(
            error_tag(br#"{"error":{"type":"a b c"}}"#),
            None,
            "a whitespace-bearing tag is prose, not a machine code",
        );
        let long = "x".repeat(MAX_ERROR_TAG_LEN + 1);
        assert_eq!(
            error_tag(format!(r#"{{"error":{{"type":"{long}"}}}}"#).as_bytes()),
            None
        );
        let at_limit = "y".repeat(MAX_ERROR_TAG_LEN);
        assert_eq!(
            error_tag(format!(r#"{{"error":{{"type":"{at_limit}"}}}}"#).as_bytes()).as_deref(),
            Some(at_limit.as_str()),
            "a tag exactly at the limit is still accepted",
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
