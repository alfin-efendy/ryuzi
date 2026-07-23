//! The `openai` provider's OpenAI-format configuration.
//!
//! All wire behaviour — base-URL resolution, request shaping, response/model
//! parsing, upstream-status classification — lives in the shared
//! `ryuzi_openai_format` crate and is tested there. What is left here is this
//! provider's own configuration, transcribed from the `openai`
//! `ProviderDescriptor` in `crates/core/src/llm_router/registry.rs`, plus the
//! tests that pin each value to the descriptor fact that justifies it.

use ryuzi_openai_format::{OpenAiFormat, DEFAULT_CONTEXT_WINDOW};

/// Static, conservative context-window hints by model-id PREFIX, scanned in
/// order so the most specific prefix wins (`gpt-4o` before `gpt-4`). These are
/// long-standing published values for well-known OpenAI families; anything
/// else — a newer or unknown model — takes [`DEFAULT_CONTEXT_WINDOW`] rather
/// than a fabricated number. OpenAI's `/models` response carries no context
/// length at all, so a static hint is the only honest source.
const CONTEXT_WINDOWS: &[(&str, u32)] = &[
    ("gpt-3.5-turbo", 16_385),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4", 8_192),
    ("o1", 200_000),
    ("o3", 200_000),
];

/// The `openai` descriptor, as wire configuration.
///
/// Descriptor facts that drive it: `base_url =
/// Some("https://api.openai.com/v1")`, `chat_path = None` (so the OpenAI-format
/// default `/chat/completions`), `has_models_endpoint = true` (so `list-models`
/// really does fetch `/models`), and `uses_max_completion_tokens = true` — the
/// one field where OpenAI differs from every other OpenAI-compatible provider,
/// because its current generation rejects `max_tokens` with HTTP 400.
///
/// `auth = AuthScheme::Bearer` is deliberately absent: the HOST injects the
/// credential per the descriptor, and this component has no `ryuzi:http` import
/// with which to set one itself.
pub const CONFIG: OpenAiFormat = OpenAiFormat {
    provider_label: "OpenAI",
    default_base_url: "https://api.openai.com/v1",
    models_path: "/models",
    chat_path: "/chat/completions",
    max_tokens_field: "max_completion_tokens",
    context_windows: CONTEXT_WINDOWS,
    default_context_window: DEFAULT_CONTEXT_WINDOW,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn config_matches_the_openai_descriptor() {
        // base_url: Some("https://api.openai.com/v1")
        assert_eq!(CONFIG.resolve_base_url(None), "https://api.openai.com/v1");
        // chat_path: None -> the OpenAI-format default; has_models_endpoint: true
        assert_eq!(
            CONFIG.chat_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            CONFIG.models_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(CONFIG.provider_label, "OpenAI");
    }

    #[test]
    fn chat_body_uses_max_completion_tokens_not_max_tokens() {
        // The `openai` descriptor is the ONLY one declaring
        // `uses_max_completion_tokens: true`, so this component is the only one
        // that must put the newer field on the wire.
        let body: Value =
            serde_json::from_slice(&CONFIG.build_chat_body("gpt-5.2", "hi", Some(64), None))
                .unwrap();
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(
            body.get("max_tokens").is_none(),
            "the legacy field must not be sent alongside it"
        );
    }

    #[test]
    fn context_window_table_prefers_the_most_specific_prefix() {
        assert_eq!(CONFIG.context_window_for("gpt-4o-mini"), 128_000);
        assert_eq!(CONFIG.context_window_for("gpt-3.5-turbo-0125"), 16_385);
        assert_eq!(
            CONFIG.context_window_for("gpt-4-0613"),
            8_192,
            "plain gpt-4 must not inherit the gpt-4o window"
        );
        assert_eq!(CONFIG.context_window_for("o1-preview"), 200_000);
        assert_eq!(
            CONFIG.context_window_for("gpt-5.2"),
            DEFAULT_CONTEXT_WINDOW,
            "an unknown model takes the conservative default, never a guess"
        );
    }
}
