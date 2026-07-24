//! The `anthropic` (x-api-key) provider's Anthropic-Messages configuration.
//!
//! All wire behaviour — base-URL resolution, request shaping, response/model
//! parsing, upstream-status classification — lives in the shared
//! `ryuzi_anthropic_format` crate and is tested there. What is left here is this
//! provider's own configuration, transcribed from the `anthropic`
//! `ProviderDescriptor` in `crates/core/src/llm_router/registry.rs`, plus the
//! tests that pin each value to the descriptor fact that justifies it.
//!
//! The shared crate is re-exported so this crate's `guest` module keeps
//! referencing `crate::logic::{...}` unchanged — the extraction moved the wire
//! logic out without changing a single guest call site or its behaviour.

pub use ryuzi_anthropic_format::{
    status_is_success, AnthropicFormat, ChunkOut, ModelOut, ProviderFail, ANTHROPIC_VERSION,
    BASE_URL_STORAGE_KEY, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS,
};

/// The `anthropic` descriptor, as wire configuration.
///
/// Descriptor facts that drive it: `base_url = Some("https://api.anthropic.com/v1")`,
/// `format = ApiFormat::Anthropic` (so `/messages`, not `/chat/completions`),
/// `chat_path = None`, and `has_models_endpoint = true` — so `list-models`
/// really does fetch `/models` rather than replaying the descriptor's seeded
/// model list.
///
/// `auth = AuthScheme::XApiKey` is deliberately absent: the HOST injects
/// `x-api-key` per the descriptor, and this component has no `ryuzi:http` import
/// with which to set one itself.
///
/// The context-window table is EMPTY on purpose. Anthropic's `/models` response
/// carries no context length and the descriptor pins no per-model windows, so
/// every model is advertised at [`DEFAULT_CONTEXT_WINDOW`] — the same
/// conservative value the router itself falls back to. Claude's published
/// windows are larger, but fabricating them here would be a guess presented as a
/// fact, and an over-stated window silently overflows prompts.
pub const CONFIG: AnthropicFormat = AnthropicFormat {
    provider_label: "Anthropic",
    default_base_url: "https://api.anthropic.com/v1",
    models_path: "/models",
    messages_path: "/messages",
    default_max_tokens: DEFAULT_MAX_TOKENS,
    context_windows: &[],
    default_context_window: DEFAULT_CONTEXT_WINDOW,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn config_matches_the_anthropic_descriptor() {
        // base_url: Some("https://api.anthropic.com/v1")
        assert_eq!(
            CONFIG.resolve_base_url(None),
            "https://api.anthropic.com/v1"
        );
        // format: ApiFormat::Anthropic -> /messages (NOT /chat/completions);
        // has_models_endpoint: true -> a real /models fetch.
        assert_eq!(
            CONFIG.messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            CONFIG.models_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(CONFIG.provider_label, "Anthropic");
        assert_eq!(ANTHROPIC_VERSION, "2023-06-01");
    }

    #[test]
    fn config_context_window_table_is_empty_so_every_model_takes_the_default() {
        // The `anthropic` descriptor pins no per-model windows and Anthropic's
        // /models response carries none, so an empty table is the honest answer.
        assert_eq!(
            CONFIG.context_window_for("claude-opus-4-5"),
            DEFAULT_CONTEXT_WINDOW
        );
        assert_eq!(
            CONFIG.context_window_for("anything-at-all"),
            DEFAULT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn config_x_api_key_request_carries_no_system_turn() {
        // The API-key component passes `system: None`, so — unlike the OAuth
        // variant — its request body must never carry a `system` field.
        let body: Value =
            serde_json::from_slice(&CONFIG.build_messages_body("m", "hi", None, None, None))
                .unwrap();
        assert!(
            body.get("system").is_none(),
            "the x-api-key path sends no Claude-subscription system marker",
        );
    }
}
