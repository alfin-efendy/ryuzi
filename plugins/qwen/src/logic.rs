//! The `qwen` (Qwen Code) provider's OpenAI-format configuration plus its
//! OAuth/device-grant markers.
//!
//! All wire behaviour — base-URL resolution, request shaping, chat-response
//! parsing, upstream-status classification, and the seeded model list — lives in
//! the shared `ryuzi_openai_format` crate and is tested there. What is left here
//! is this provider's own configuration, transcribed from the `qwen`
//! `ProviderDescriptor` in `crates/core/src/llm_router/registry.rs`, plus the
//! tests that pin each value to the descriptor fact that justifies it.
//!
//! # Why this is an OAuth component, not an API-key one
//! Qwen Code is an OAUTH provider (`category: OAuth`, `device_grant:
//! QWEN_DEVICE_GRANT`): the credential is the user's Qwen subscription obtained
//! through an RFC 8628 device-authorization grant, not an API key. The component
//! therefore imports `ryuzi:oauth` (NOT `ryuzi:http`/`ryuzi:provider-auth`) and
//! sends every request through `authorized-request("qwen", ..)`, where the host
//! injects the stored access token as `Authorization: Bearer …`. The access and
//! refresh tokens never cross into the guest.
//!
//! # No live `/models` endpoint — the list is SEEDED
//! The `qwen` descriptor sets `has_models_endpoint: false`: portal.qwen.ai's
//! `/models` route 404s, so the native router advertises the descriptor's seeded
//! model list rather than fetching. [`SEEDED_MODELS`] transcribes that list, and
//! the OAuth guest returns it from `list-models` (see
//! `ryuzi_openai_format::oauth_provider_component!`) instead of a fetch.
//!
//! # Known production gap the flat ABI cannot express (shard resource_url)
//! Qwen tokens are bound to the shard `resource_url` returned at grant time; the
//! native path rewrites the chat base to `https://<resource_url>/v1` when one is
//! present, falling back to portal.qwen.ai otherwise
//! (`llm_router::client::qwen_base_url`). A component only ever sees its own
//! `ryuzi:storage` slice and the descriptor base — it cannot read the
//! connection's `provider_specific.resource_url` — so it always targets the
//! descriptor base host (portal.qwen.ai). A token issued on a non-default shard
//! would 401 there. This is a real, documented limitation of porting qwen to the
//! sandboxed component model, NOT verified against the live service here.

use ryuzi_openai_format::{OpenAiFormat, DEFAULT_CONTEXT_WINDOW};

/// The OAuth profile id this component passes to `authorized-request`. It MUST
/// equal the manifest's `[[oauth]]` id AND the router provider id, or the host
/// rejects the request with `denied`
/// (`ProfileOauth::ensure_declared_profile`).
pub const OAUTH_PROFILE: &str = "qwen";

/// The seeded model ids Qwen Code advertises, transcribed from the `qwen`
/// descriptor's `models`. Used because `has_models_endpoint: false` — the live
/// `/models` route 404s, so `list-models` returns this seed rather than fetching.
pub const SEEDED_MODELS: &[&str] = &[
    "qwen3-coder-plus",
    "qwen3-coder-flash",
    "vision-model",
    "coder-model",
];

/// The `qwen` descriptor, as wire configuration.
///
/// Descriptor facts that drive it: `base_url =
/// Some("https://portal.qwen.ai/v1")`, `format = ApiFormat::OpenAi`, `chat_path
/// = None` (so the OpenAI-format default `/chat/completions`),
/// `uses_max_completion_tokens = false` (so the legacy `max_tokens` field), and
/// an EMPTY context-window table because the descriptor pins no per-model
/// windows and Qwen's responses carry no context length.
///
/// `auth = AuthScheme::Bearer` is deliberately absent as a field: the HOST
/// injects the OAuth bearer, and this component has no `ryuzi:http` import with
/// which to set one itself.
pub const CONFIG: OpenAiFormat = OpenAiFormat {
    provider_label: "Qwen Code",
    default_base_url: "https://portal.qwen.ai/v1",
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
    fn config_matches_the_qwen_descriptor() {
        // base_url: Some("https://portal.qwen.ai/v1")
        assert_eq!(CONFIG.resolve_base_url(None), "https://portal.qwen.ai/v1");
        // chat_path: None -> the OpenAI-format default `/chat/completions`, the
        // same path the native `oauth_upstream_request` qwen arm POSTs to.
        assert_eq!(
            CONFIG.chat_url("https://portal.qwen.ai/v1"),
            "https://portal.qwen.ai/v1/chat/completions"
        );
        assert_eq!(CONFIG.provider_label, "Qwen Code");
    }

    #[test]
    fn chat_body_uses_the_legacy_max_tokens_field() {
        // `uses_max_completion_tokens: false` -> qwen speaks the legacy
        // `max_tokens`, never OpenAI's `max_completion_tokens`.
        let body: Value = serde_json::from_slice(&CONFIG.build_chat_body(
            "qwen3-coder-plus",
            "hi",
            Some(64),
            None,
        ))
        .unwrap();
        assert_eq!(body["max_tokens"], 64);
        assert!(
            body.get("max_completion_tokens").is_none(),
            "qwen must not send the OpenAI-only token field"
        );
        // The flat ABI is exactly one user turn.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn seeded_models_are_the_descriptor_models_with_the_default_window() {
        // `has_models_endpoint: false` -> the list is the descriptor's seed, not
        // a fetch. The table is empty, so every model takes the conservative
        // default window, and the id doubles as the display name.
        let models = CONFIG.seeded_models(SEEDED_MODELS);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "qwen3-coder-plus",
                "qwen3-coder-flash",
                "vision-model",
                "coder-model"
            ],
            "the seed and its order come from the descriptor's models"
        );
        for model in &models {
            assert_eq!(model.display_name, model.id);
            assert_eq!(model.context_window, DEFAULT_CONTEXT_WINDOW);
        }
    }

    #[test]
    fn oauth_profile_is_the_provider_id() {
        // The guest passes this to `authorized-request`; it must equal the
        // router provider id and the manifest `[[oauth]]` id, or the host denies.
        assert_eq!(OAUTH_PROFILE, "qwen");
    }
}
