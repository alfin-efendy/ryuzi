//! Shared, host-free OpenAI-chat wire logic for Ryuzi's first-party provider
//! components.
//!
//! A dozen providers in `crates/core/src/llm_router/registry.rs` declare
//! `format: ApiFormat::OpenAi` and differ only in a handful of constants. This
//! crate owns everything they have in COMMON — base-URL override resolution,
//! request-body shaping from the flat provider ABI, `/models` and chat-response
//! parsing, and upstream-status -> `provider-error` classification — so that
//! logic is written, reviewed and tested ONCE instead of once per component.
//!
//! What stays per-provider is exactly the data that provider's
//! `ProviderDescriptor` in the registry already carries: see [`OpenAiFormat`],
//! whose every field maps to a descriptor field. A component crate is then a
//! config constant, a thin wrapper, and the [`provider_component!`] guest glue.
//!
//! # Egress-agnostic: how a component reaches the network is the ONE seam
//! Request-BUILDING and response-PARSING (everything on [`OpenAiFormat`], plus
//! [`status_is_success`], [`error_tag`], [`seeded_models`](OpenAiFormat::seeded_models)
//! and [`oauth_error_to_provider_error`]) are wholly independent of HOW the
//! request is sent. The single egress seam lives in the guest glue: an API-key
//! component sends its built request through host-mediated `ryuzi:provider-auth`
//! ([`provider_component!`]); an OAuth component sends the SAME built request
//! through `ryuzi:oauth`'s `authorized-request` ([`oauth_provider_component!`]).
//! Both macros share one core ([`__openai_provider_guest_core!`]) and differ
//! only in that seam — the wire logic is never forked.
//!
//! # Nothing here touches a credential
//! Provider components authenticate host-side: the API-key variant through
//! `ryuzi:provider-auth` (the host resolves the user's stored key and injects
//! it), the OAuth variant through `ryuzi:oauth` (the host injects the bearer for
//! the profile). No function in this crate sees, stores, or renders one — and
//! [`error_tag`] exists specifically to keep upstream error PROSE (which can
//! echo a submitted key) out of the guest-visible error string.
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text: a `prompt` string in, text chunks
//! out. Every component built on this crate therefore supports plain text
//! completion only — no tool calling, no structured multi-turn messages, no
//! multimodal content, and no true token streaming (the single buffered
//! upstream response is returned as one terminal chunk). That is a deliberate,
//! accepted tradeoff of the WASM provider migration, not an oversight.

use serde_json::{Map, Value};

mod guest_macro;

/// Key in a component's (host-scoped) `ryuzi:storage` slice holding an OPTIONAL
/// base-URL override. Shared across every OpenAI-format component because it is
/// the same product-level affordance everywhere: pointing a component at an
/// OpenAI-compatible proxy/gateway, and letting the provider conformance
/// harness aim it at a loopback mock. A blank/whitespace value is treated as
/// "unset". The manifest network allowlist still governs whatever the override
/// resolves to, so an override can never widen where the user's key may travel.
pub const BASE_URL_STORAGE_KEY: &str = "base-url";

/// Context window advertised for a model no per-provider table covers.
///
/// An OpenAI-format `/models` response carries no context length (only
/// `id`/`object`/`created`/`owned_by`), so a window is either a static hint or a
/// guess. This is the conservative hint, and it deliberately mirrors the value
/// the router itself already falls back to
/// (`llm_router::model_meta::FALLBACK.context_window`) rather than introducing a
/// second, differently-wrong default.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// Longest an `error.code`/`error.type` tag may be before it stops looking like
/// a machine-readable code and starts looking like prose that could carry
/// upstream-echoed request material. See [`error_tag`].
const MAX_ERROR_TAG_LEN: usize = 64;

/// Everything that differs between two OpenAI-chat providers.
///
/// Every field is DATA a provider's `ProviderDescriptor`
/// (`crates/core/src/llm_router/registry.rs`) already states, so a component's
/// config is a transcription of its descriptor rather than an independent guess:
///
/// | field | descriptor source |
/// | --- | --- |
/// | [`Self::provider_label`] | `name` |
/// | [`Self::default_base_url`] | `base_url` |
/// | [`Self::models_path`] | the OpenAI-format `/models` default |
/// | [`Self::chat_path`] | `chat_path`, or the OpenAI-format default |
/// | [`Self::max_tokens_field`] | `uses_max_completion_tokens` |
/// | [`Self::context_windows`] / [`Self::default_context_window`] | static hints; see [`DEFAULT_CONTEXT_WINDOW`] |
///
/// Fields are listed exhaustively at every construction site ON PURPOSE: adding
/// one here must be a compile error at each component, forcing a deliberate
/// per-provider decision instead of a silently inherited default.
///
/// Two descriptor facts are deliberately NOT fields:
/// - `auth` — the HOST injects the credential per the descriptor's `AuthScheme`;
///   a component never sees or chooses it.
/// - `has_models_endpoint` — every provider built on this crate declares it
///   `true`. A provider that declares `false` needs a different `list-models`
///   shape (a seeded list, not a fetch), so it belongs in its own component
///   rather than behind a flag here.
pub struct OpenAiFormat {
    /// Human-readable provider name used in guest-visible error strings
    /// ("Groq rejected the request: HTTP 400"). Never a credential, never an id
    /// the host branches on.
    pub provider_label: &'static str,
    /// The descriptor's `base_url`. Used unless the component's storage slice
    /// carries an override at [`BASE_URL_STORAGE_KEY`].
    pub default_base_url: &'static str,
    /// Model-discovery path appended to the resolved base.
    pub models_path: &'static str,
    /// Chat-generation path appended to the resolved base. `/chat/completions`
    /// unless the descriptor names a nonstandard `chat_path`.
    pub chat_path: &'static str,
    /// The token-cap request field: `"max_completion_tokens"` when the
    /// descriptor sets `uses_max_completion_tokens`, else `"max_tokens"`.
    /// Current OpenAI models reject the legacy name with HTTP 400; every other
    /// OpenAI-compatible provider rejects (or ignores) the new one.
    pub max_tokens_field: &'static str,
    /// Static context-window hints by model-id PREFIX, scanned IN ORDER so the
    /// most specific prefix must be listed first (`gpt-4o` before `gpt-4`).
    /// Empty for a provider with no published per-family values worth pinning —
    /// an empty table plus [`Self::default_context_window`] is the honest
    /// answer, and strictly better than fabricated per-model numbers.
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

impl OpenAiFormat {
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

    /// `<base><chat_path>` — the chat-generation endpoint.
    pub fn chat_url(&self, base: &str) -> String {
        format!("{base}{}", self.chat_path)
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

    /// Build the NON-STREAMING chat-completions body for a flat prompt.
    ///
    /// The `ryuzi:provider/provider` ABI carries a single `prompt` string, so
    /// the request is exactly one `user` message — no system turn, no tools, no
    /// multimodal parts. `stream` is false because the host capability is a
    /// buffered request/response: the component asks for the whole completion
    /// and returns it as one terminal chunk.
    ///
    /// `temperature` is OMITTED when it is not finite (NaN/±inf): JSON has no
    /// representation for those values, so there is nothing to send. The request
    /// still goes out and the upstream applies its own default — failing an
    /// entire completion over an unrepresentable optional tuning knob would be
    /// the worse trade. Pinned by
    /// `tests::chat_body_drops_a_non_finite_temperature_rather_than_failing`.
    pub fn build_chat_body(
        &self,
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
            obj.insert(self.max_tokens_field.to_string(), Value::from(max));
        }
        if let Some(temp) = temperature {
            if let Some(number) = serde_json::Number::from_f64(temp as f64) {
                obj.insert("temperature".to_string(), Value::Number(number));
            }
        }
        serde_json::to_vec(&Value::Object(obj)).expect("chat body always serializes")
    }

    /// Parse an OpenAI-format `/models` response (`{"data":[{"id":...}]}`) into
    /// the advertised model list, preserving the served order. The response
    /// carries no display name or context length, so the id doubles as the
    /// display name and the window comes from [`Self::context_window_for`].
    /// Entries without a string `id` are skipped rather than failing the whole
    /// listing.
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
                Some(ModelOut {
                    display_name: id.clone(),
                    context_window: self.context_window_for(&id),
                    id,
                })
            })
            .collect())
    }

    /// The provider's SEEDED model list, for a descriptor whose
    /// `has_models_endpoint` is `false` — its `/models` route 404s, so the honest
    /// `list-models` is the static seed the router itself falls back to, NOT a
    /// fetch. Each id doubles as the display name (an OpenAI-format listing
    /// carries none anyway) and takes its window from [`Self::context_window_for`].
    /// Served order is preserved. This is the format-level counterpart to
    /// [`Self::parse_models`]: same [`ModelOut`] shape, no network. The mapping is
    /// egress-agnostic, so it is written and tested ONCE here rather than in each
    /// seeded provider's own component.
    pub fn seeded_models(&self, ids: &[&str]) -> Vec<ModelOut> {
        ids.iter()
            .map(|id| ModelOut {
                display_name: (*id).to_string(),
                context_window: self.context_window_for(id),
                id: (*id).to_string(),
            })
            .collect()
    }

    /// Convert a buffered (non-stream) chat completion into ordered completion
    /// chunks: the assistant message content becomes a single terminal chunk
    /// carrying the response's token usage when present.
    pub fn parse_chat_response(&self, body: &[u8]) -> Result<Vec<ChunkOut>, ProviderFail> {
        let label = self.provider_label;
        let value: Value = serde_json::from_slice(body)
            .map_err(|e| ProviderFail::Failed(format!("{label} chat response is not JSON: {e}")))?;
        let content = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderFail::Failed(format!("{label} chat response carried no content"))
            })?;
        Ok(vec![ChunkOut {
            text: content.to_string(),
            finished: true,
            usage: parse_usage(&value),
        }])
    }

    /// Map a non-2xx upstream response onto a [`ProviderFail`].
    ///
    /// - `429` -> rate-limited
    /// - `5xx` -> unavailable (transient/environmental, never a "bad model" verdict)
    /// - a `model_not_found` code -> model-not-found
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
        if tag.as_deref() == Some(MODEL_NOT_FOUND_CODE) {
            return ProviderFail::ModelNotFound;
        }
        ProviderFail::InvalidRequest(match tag {
            Some(tag) => format!("{label} rejected the request: HTTP {status} ({tag})"),
            None => format!("{label} rejected the request: HTTP {status}"),
        })
    }
}

/// The OpenAI-format `error.code` that means "that model does not exist". Shared
/// across the format: the compatible providers reuse OpenAI's own code
/// vocabulary, and a provider that does not simply never emits it (its 404 stays
/// a plain invalid-request, which is the safe verdict).
const MODEL_NOT_FOUND_CODE: &str = "model_not_found";

/// Whether an upstream status is a success (and so parsed rather than
/// classified as an error).
pub fn status_is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// A host OAuth-egress failure, mirrored host-free so mapping it to a
/// [`ProviderFail`] is natively testable — the WIT `oauth-error` variant only
/// exists in a component's wasm32 guest build. The OAuth egress glue (see
/// [`oauth_provider_component!`]) converts the generated `oauth-error` into this
/// and calls [`oauth_error_to_provider_error`]. Mirrors `ryuzi:oauth`'s
/// `oauth-error` variant set exactly (`invalid-request` / `denied` / `expired` /
/// `failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthFail {
    InvalidRequest(String),
    Denied,
    Expired,
    Failed(String),
}

/// Map a host `oauth-error` onto a [`ProviderFail`], credential-safe.
///
/// `denied`/`expired` become actionable `invalid-request`s (the request cannot
/// proceed until the user (re)connects — not a transient condition), each naming
/// the provider via `label`. The host's own `oauth-error` contract keeps the
/// access/refresh token out of every message it returns, and nothing here
/// fabricates or echoes a URL: the `denied`/`expired` messages this function
/// ORIGINATES name only the provider and the action to take, so even a hostile
/// host string cannot smuggle a token through them. This is the OpenAI-format
/// analogue of the mapping the `anthropic-oauth` component carries, shared here
/// because every OpenAI-format OAuth provider needs the identical behaviour.
pub fn oauth_error_to_provider_error(label: &str, fail: OAuthFail) -> ProviderFail {
    match fail {
        OAuthFail::Denied => ProviderFail::InvalidRequest(format!(
            "{label} is not connected — connect it in Settings > Providers."
        )),
        OAuthFail::Expired => ProviderFail::InvalidRequest(format!(
            "{label} authorization expired — reconnect it in Settings > Providers."
        )),
        // The host's invalid-request message is credential-free by contract;
        // surface it so the caller learns what was wrong with the request.
        OAuthFail::InvalidRequest(message) => ProviderFail::InvalidRequest(message),
        OAuthFail::Failed(message) => {
            ProviderFail::Failed(format!("{label} request failed: {message}"))
        }
    }
}

/// The short, machine-readable `error.code` (preferred) or `error.type` from an
/// OpenAI-format error body, if it really looks like a code.
///
/// Deliberately NOT `error.message`: OpenAI-format auth failures echo the
/// submitted key back ("Incorrect API key provided: sk-…"), and this value
/// crosses into a guest-visible `provider-error`. A tag that is blank, over
/// [`MAX_ERROR_TAG_LEN`], or contains whitespace is prose rather than a code and
/// is dropped.
///
/// This filter is the crate's one non-obvious security-relevant behaviour: it is
/// what makes [`OpenAiFormat::classify_error`] safe to surface, and every
/// component inherits it by construction.
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

fn parse_usage(value: &Value) -> Option<UsageOut> {
    let usage = value.get("usage")?;
    let input = usage.get("prompt_tokens").and_then(Value::as_u64)?;
    let output = usage.get("completion_tokens").and_then(Value::as_u64)?;
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

    /// The `openai` descriptor's config, transcribed — the shape with the
    /// NEWER token field and a populated context-window table.
    const OPENAI: OpenAiFormat = OpenAiFormat {
        provider_label: "OpenAI",
        default_base_url: "https://api.openai.com/v1",
        models_path: "/models",
        chat_path: "/chat/completions",
        max_tokens_field: "max_completion_tokens",
        context_windows: &[
            ("gpt-3.5-turbo", 16_385),
            ("gpt-4o", 128_000),
            ("gpt-4-turbo", 128_000),
            ("gpt-4", 8_192),
            ("o1", 200_000),
            ("o3", 200_000),
        ],
        default_context_window: DEFAULT_CONTEXT_WINDOW,
    };

    /// A deliberately DIFFERENT config in every dimension the struct exposes —
    /// label, base, both paths, token field, empty table, non-default window.
    /// Its purpose is anti-tautology: assertions run against both configs, so a
    /// function that ignored `self` and hardcoded OpenAI's values would fail
    /// here even though it passed above.
    const OTHER: OpenAiFormat = OpenAiFormat {
        provider_label: "Contoso",
        default_base_url: "https://api.contoso.test/openai/v1",
        models_path: "/model-list",
        chat_path: "/chat",
        max_tokens_field: "max_tokens",
        context_windows: &[],
        default_context_window: 32_768,
    };

    #[test]
    fn base_url_defaults_to_the_configured_api_and_honours_a_non_empty_override() {
        assert_eq!(OPENAI.resolve_base_url(None), "https://api.openai.com/v1");
        assert_eq!(
            OPENAI.resolve_base_url(Some("")),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            OPENAI.resolve_base_url(Some("   ")),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            OTHER.resolve_base_url(None),
            "https://api.contoso.test/openai/v1",
            "the default must come from the config, not a hardcoded vendor"
        );
        assert_eq!(
            OPENAI.resolve_base_url(Some("http://127.0.0.1:8080")),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            OPENAI.resolve_base_url(Some("https://proxy.test/v1/")),
            "https://proxy.test/v1",
            "a trailing slash is trimmed so path joins never double up"
        );
    }

    #[test]
    fn endpoint_urls_are_joined_onto_the_resolved_base_using_the_configured_paths() {
        assert_eq!(
            OPENAI.models_url(&OPENAI.resolve_base_url(None)),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            OPENAI.chat_url(&OPENAI.resolve_base_url(None)),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            OPENAI.models_url("http://127.0.0.1:9"),
            "http://127.0.0.1:9/models"
        );
        // A config with nonstandard paths must actually use them.
        assert_eq!(
            OTHER.models_url(&OTHER.resolve_base_url(None)),
            "https://api.contoso.test/openai/v1/model-list"
        );
        assert_eq!(
            OTHER.chat_url(&OTHER.resolve_base_url(None)),
            "https://api.contoso.test/openai/v1/chat"
        );
    }

    #[test]
    fn chat_body_maps_the_flat_prompt_to_a_single_user_message() {
        let body: Value =
            serde_json::from_slice(&OPENAI.build_chat_body("gpt-5.2", "ping", None, None)).unwrap();
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
    fn chat_body_uses_the_configured_token_cap_field_and_never_both() {
        let newer: Value =
            serde_json::from_slice(&OPENAI.build_chat_body("gpt-5.2", "hi", Some(64), Some(0.2)))
                .unwrap();
        assert_eq!(newer["max_completion_tokens"], 64);
        assert!(
            newer.get("max_tokens").is_none(),
            "the legacy field must not be sent alongside the new one"
        );
        // The WIT temperature is an f32, so the JSON number is its widened
        // value — compare within f32 precision rather than bit-exactly.
        assert!((newer["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);

        let legacy: Value =
            serde_json::from_slice(&OTHER.build_chat_body("m", "hi", Some(64), None)).unwrap();
        assert_eq!(legacy["max_tokens"], 64);
        assert!(
            legacy.get("max_completion_tokens").is_none(),
            "a provider whose descriptor does not set uses_max_completion_tokens \
             must send only the legacy field"
        );
    }

    #[test]
    fn chat_body_drops_a_non_finite_temperature_rather_than_failing() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let body: Value =
                serde_json::from_slice(&OPENAI.build_chat_body("gpt-5.2", "hi", None, Some(bad)))
                    .unwrap();
            assert!(
                body.get("temperature").is_none(),
                "a non-finite temperature ({bad}) must be omitted, not serialized"
            );
            assert_eq!(
                body["messages"][0]["content"], "hi",
                "the request still goes"
            );
        }
    }

    #[test]
    fn parse_models_maps_data_ids_with_a_context_window() {
        let body = br#"{"object":"list","data":[
            {"id":"gpt-4o","object":"model"},
            {"id":"gpt-3.5-turbo","object":"model"},
            {"object":"model"},
            {"id":"some-future-model","object":"model"}
        ]}"#;
        let models = OPENAI.parse_models(body).unwrap();
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
    fn parse_models_uses_the_configs_own_default_window_when_its_table_is_empty() {
        // Same body, different config: a provider with no static table must
        // advertise ITS default for every model, including ids OpenAI's table
        // would have matched.
        let body = br#"{"data":[{"id":"gpt-4o"},{"id":"llama-3.3-70b"}]}"#;
        let models = OTHER.parse_models(body).unwrap();
        assert_eq!(
            models.iter().map(|m| m.context_window).collect::<Vec<_>>(),
            vec![32_768, 32_768]
        );
    }

    #[test]
    fn parse_models_rejects_a_body_without_a_data_array() {
        assert!(matches!(
            OPENAI.parse_models(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            OPENAI.parse_models(br#"{"object":"list"}"#),
            Err(ProviderFail::Failed(_))
        ));
        // The failure names the provider it came from.
        match OTHER.parse_models(b"{}") {
            Err(ProviderFail::Failed(message)) => assert!(message.contains("Contoso"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn context_window_prefers_the_most_specific_prefix() {
        assert_eq!(OPENAI.context_window_for("gpt-4o-mini"), 128_000);
        assert_eq!(OPENAI.context_window_for("gpt-4-turbo-2024-04-09"), 128_000);
        assert_eq!(
            OPENAI.context_window_for("gpt-4-0613"),
            8_192,
            "plain gpt-4 must not inherit the gpt-4o window"
        );
        assert_eq!(OPENAI.context_window_for("o1-preview"), 200_000);
        assert_eq!(OPENAI.context_window_for("gpt-5.2"), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn parse_chat_response_yields_one_terminal_chunk_with_usage() {
        let body = br#"{
            "id": "chatcmpl-1",
            "choices": [{"index":0,"message":{"role":"assistant","content":"Hello, world!"},"finish_reason":"stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        }"#;
        let chunks = OPENAI.parse_chat_response(body).unwrap();
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
    fn parse_chat_response_saturates_a_usage_count_that_exceeds_u32() {
        // The WIT `token-usage` fields are u32 but JSON numbers are u64-wide.
        // An absurd/hostile count must SATURATE, never wrap: a wrapping cast
        // would turn 5_000_000_000 into 705_032_704 and silently under-report
        // spend to the router.
        let body = br#"{
            "choices":[{"message":{"content":"hi"}}],
            "usage":{"prompt_tokens":5000000000,"completion_tokens":4294967296}
        }"#;
        let chunks = OPENAI.parse_chat_response(body).unwrap();
        assert_eq!(
            chunks[0].usage,
            Some(UsageOut {
                input: u32::MAX,
                output: u32::MAX
            })
        );
    }

    #[test]
    fn parse_chat_response_without_usage_still_succeeds() {
        let chunks = OPENAI
            .parse_chat_response(br#"{"choices":[{"message":{"content":"hi"}}]}"#)
            .unwrap();
        assert_eq!(chunks[0].text, "hi");
        assert!(chunks[0].finished);
        assert_eq!(chunks[0].usage, None);
    }

    #[test]
    fn parse_chat_response_rejects_a_body_with_no_content() {
        assert!(matches!(
            OPENAI.parse_chat_response(br#"{"choices":[]}"#),
            Err(ProviderFail::Failed(_))
        ));
        assert!(matches!(
            OPENAI.parse_chat_response(b"not json"),
            Err(ProviderFail::Failed(_))
        ));
    }

    #[test]
    fn classify_error_maps_429_to_rate_limited_and_5xx_to_unavailable() {
        assert_eq!(OPENAI.classify_error(429, b""), ProviderFail::RateLimited);
        for status in [500u16, 502, 503, 504] {
            assert_eq!(
                OPENAI.classify_error(status, b"boom"),
                ProviderFail::Unavailable
            );
        }
    }

    #[test]
    fn classify_error_maps_a_model_not_found_code_to_model_not_found() {
        let body = br#"{"error":{"message":"The model `nope` does not exist","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert_eq!(
            OPENAI.classify_error(404, body),
            ProviderFail::ModelNotFound
        );
    }

    #[test]
    fn classify_error_maps_other_4xx_to_invalid_request_naming_the_provider() {
        match OPENAI.classify_error(400, br#"{"error":{"type":"invalid_request_error"}}"#) {
            ProviderFail::InvalidRequest(message) => {
                assert!(
                    message.contains("400"),
                    "the status must be reported: {message}"
                );
                assert!(message.contains("invalid_request_error"));
                assert!(message.contains("OpenAI"));
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        // The same status through a different config names THAT provider.
        match OTHER.classify_error(400, b"") {
            ProviderFail::InvalidRequest(message) => {
                assert!(message.contains("Contoso"), "{message}");
                assert!(!message.contains("OpenAI"), "{message}");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        // A 404 with no `model_not_found` code stays a plain invalid-request:
        // the router must not persist a bogus "bad model" verdict.
        assert!(matches!(
            OPENAI.classify_error(404, br#"{"error":{"code":"unknown_route"}}"#),
            ProviderFail::InvalidRequest(_)
        ));
    }

    #[test]
    fn a_classified_error_never_echoes_the_upstream_message_or_a_credential() {
        // OpenAI's 401 body echoes a (partially redacted) key. Nothing from the
        // upstream `message` may reach a guest-visible error string.
        let body = br#"{"error":{"message":"Incorrect API key provided: sk-live-ABCDEF. You can find your API key at ...","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        let rendered = format!("{:?}", OPENAI.classify_error(401, body));
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

    #[test]
    fn seeded_models_maps_ids_with_the_configs_window_table() {
        // A provider whose descriptor sets `has_models_endpoint: false` (its
        // `/models` route 404s) advertises its SEEDED model list instead of
        // fetching. The id doubles as the display name (an OpenAI-format listing
        // carries no display name), and the window comes from the config's table
        // / default — never a fetch.
        let seeded = OPENAI.seeded_models(&["gpt-4o", "gpt-5.2", "gpt-3.5-turbo"]);
        assert_eq!(
            seeded,
            vec![
                ModelOut {
                    id: "gpt-4o".to_string(),
                    display_name: "gpt-4o".to_string(),
                    context_window: 128_000,
                },
                ModelOut {
                    id: "gpt-5.2".to_string(),
                    display_name: "gpt-5.2".to_string(),
                    context_window: DEFAULT_CONTEXT_WINDOW,
                },
                ModelOut {
                    id: "gpt-3.5-turbo".to_string(),
                    display_name: "gpt-3.5-turbo".to_string(),
                    context_window: 16_385,
                },
            ],
            "served order is preserved, id is the display name, window comes from the config",
        );
        // A config with an EMPTY table gives every seeded id its own default.
        let other = OTHER.seeded_models(&["a", "b"]);
        assert_eq!(
            other.iter().map(|m| m.context_window).collect::<Vec<_>>(),
            vec![32_768, 32_768],
        );
        assert!(OPENAI.seeded_models(&[]).is_empty(), "no ids, no models");
    }

    #[test]
    fn oauth_errors_map_to_actionable_provider_errors_naming_the_provider() {
        // denied/expired become actionable invalid-requests (the request cannot
        // proceed until the user (re)connects — not a transient condition), each
        // naming the provider from the config label.
        assert_eq!(
            oauth_error_to_provider_error("Qwen Code", OAuthFail::Denied),
            ProviderFail::InvalidRequest(
                "Qwen Code is not connected — connect it in Settings > Providers.".to_string()
            ),
        );
        assert!(matches!(
            oauth_error_to_provider_error("Qwen Code", OAuthFail::Expired),
            ProviderFail::InvalidRequest(message)
                if message.contains("Qwen Code") && message.contains("expired")
        ));
        // A host invalid-request message is credential-free by contract; surface
        // it so the caller learns what was wrong with the request.
        assert_eq!(
            oauth_error_to_provider_error(
                "Qwen Code",
                OAuthFail::InvalidRequest("bad method".into())
            ),
            ProviderFail::InvalidRequest("bad method".to_string()),
        );
        // A host transport failure stays Failed carrying the provider label and
        // the host's (token-free) message — what the conformance timeout check
        // reads as "caught it, didn't hang".
        match oauth_error_to_provider_error(
            "Qwen Code",
            OAuthFail::Failed("connect timeout".into()),
        ) {
            ProviderFail::Failed(message) => {
                assert!(message.contains("Qwen Code"));
                assert!(message.contains("failed"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_mapped_oauth_error_never_contains_a_bearer_token() {
        // The denied/expired arms originate their OWN text and never interpolate
        // the host message, so even a hostile host string cannot smuggle a token
        // through them.
        let rendered = format!(
            "{:?}",
            oauth_error_to_provider_error("Qwen Code", OAuthFail::Denied)
        );
        assert!(!rendered.to_lowercase().contains("bearer"));
        assert!(!rendered.contains("access_token"));
    }
}
