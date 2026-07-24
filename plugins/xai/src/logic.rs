//! The `xai` provider's OpenAI-format configuration.
//!
//! All wire behaviour — base-URL resolution, request shaping, response/model
//! parsing, upstream-status classification — lives in the shared
//! `ryuzi_openai_format` crate and is tested there. What is left here is this
//! provider's own configuration, transcribed from the `xai`
//! `ProviderDescriptor` in `crates/core/src/llm_router/registry.rs`, plus the
//! tests that pin each value to the descriptor fact that justifies it.

use ryuzi_openai_format::{OpenAiFormat, DEFAULT_CONTEXT_WINDOW};

/// The `xai` descriptor, as wire configuration.
///
/// Descriptor facts that drive it: `base_url = Some("https://api.x.ai/v1")`,
/// `chat_path = None` (so the OpenAI-format default `/chat/completions`),
/// `has_models_endpoint = true` (so `list-models` really does fetch `/models`),
/// and `uses_max_completion_tokens = false` — `openai` is the ONLY descriptor
/// that sets it, so this provider sends the legacy `max_tokens`.
///
/// `auth = AuthScheme::Bearer` is deliberately absent: the HOST injects the
/// credential per the descriptor, and this component has no `ryuzi:http` import
/// with which to set one itself.
///
/// The context-window table is EMPTY on purpose. xAI's `/models` response
/// carries no context length, and the descriptor pins no per-model windows, so
/// every model is advertised at [`DEFAULT_CONTEXT_WINDOW`] — the same
/// conservative value the router itself falls back to. Fabricating per-model
/// numbers here would be a guess presented as a fact.
pub const CONFIG: OpenAiFormat = OpenAiFormat {
    provider_label: "xAI",
    default_base_url: "https://api.x.ai/v1",
    models_path: "/models",
    chat_path: "/chat/completions",
    max_tokens_field: "max_tokens",
    context_windows: &[],
    default_context_window: DEFAULT_CONTEXT_WINDOW,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn config_matches_the_xai_descriptor() {
        assert_eq!(CONFIG.resolve_base_url(None), "https://api.x.ai/v1");
        assert_eq!(
            CONFIG.chat_url("https://api.x.ai/v1"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            CONFIG.models_url("https://api.x.ai/v1"),
            "https://api.x.ai/v1/models"
        );
        assert_eq!(CONFIG.provider_label, "xAI");
    }

    #[test]
    fn chat_body_uses_the_legacy_max_tokens_field() {
        // The `xai` descriptor leaves `uses_max_completion_tokens` false, so
        // sending `max_completion_tokens` would be wrong for this provider.
        let body: Value =
            serde_json::from_slice(&CONFIG.build_chat_body("m", "hi", Some(64), None)).unwrap();
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn every_model_takes_the_conservative_default_window() {
        // No static table: the component must not invent a per-model window.
        for model in ["anything", "some-big-model", "gpt-4"] {
            assert_eq!(CONFIG.context_window_for(model), DEFAULT_CONTEXT_WINDOW);
        }
    }
}
