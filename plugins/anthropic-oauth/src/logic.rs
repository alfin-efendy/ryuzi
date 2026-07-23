//! The `anthropic-oauth` (Claude subscription) provider's Anthropic-Messages
//! configuration plus the Claude-subscription OAuth auth markers.
//!
//! All wire behaviour — base-URL resolution, request shaping, response/model
//! parsing, upstream-status classification — lives in the shared
//! `ryuzi_anthropic_format` crate and is tested there. What is left here is this
//! provider's configuration (identical wire values to the `anthropic` bundle),
//! the OAuth auth markers this variant adds, and the host-`oauth-error` ->
//! `provider-error` mapping — each pinned to the native router path it is ported
//! from.
//!
//! # The auth markers, and where they come from
//! Anthropic's OAuth (Claude Pro/Max) endpoint rejects a subscription bearer
//! unless the request also looks like the official Claude Code client. The
//! native router path (`crates/core/src/llm_router`) sends, for every
//! `anthropic-oauth` call, three token-INDEPENDENT markers, each ported here:
//! the `anthropic-beta: <ANTHROPIC_OAUTH_BETA>` header (carrying the
//! `oauth-2025-04-20` flag; `llm_router::models::ANTHROPIC_OAUTH_BETA`), a
//! leading Claude-Code `system` block
//! (`llm_router::models::CLAUDE_CODE_SYSTEM_PROMPT`, injected by
//! `inject_claude_code_system_prompt`, documented there as REQUIRED for OAuth
//! tokens), and `?beta=true` on the `/messages` URL
//! (`llm_router::client::oauth_upstream_request`). All three are expressible in
//! the flat-text ABI: the header and query are set by [`crate::guest`], and the
//! system marker goes into the request BODY this module builds (the host owns
//! headers, so a body-borne marker is the only place a component can put it).
//!
//! # What is deliberately NOT ported (anti-abuse spoofing, not authentication)
//! The native path also runs `claude_cloak::apply_request_cloak` +
//! `spoof_headers`. Under the flat-text ABI (no tools) the tool-cloaking is a
//! no-op, and the rest is anti-abuse fingerprinting, not an acceptance
//! requirement: a body-hash billing block, a client-fingerprint header set, and
//! — critically — a `metadata.user_id` DERIVED FROM THE ACCESS TOKEN, which this
//! component fundamentally cannot compute because it never sees the token (that
//! is the whole point of the OAuth-from-component model). Omitting them makes the
//! request more detectable as non-official-client traffic but does not stop it
//! authenticating; see the task report for the full accounting.

use ryuzi_anthropic_format::{
    AnthropicFormat, ProviderFail, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS,
};

// Re-exported so `crate::guest` references these through `crate::logic` (kept
// structurally parallel to the `anthropic` bundle's guest).
pub use ryuzi_anthropic_format::{
    status_is_success, ChunkOut, ModelOut, ANTHROPIC_VERSION, BASE_URL_STORAGE_KEY,
};

/// The OAuth profile id this component passes to `authorized-request`. It MUST
/// equal the manifest's `[[oauth]]` id AND the router provider id, or the host
/// rejects the request with `denied` (`ProfileOauth::ensure_declared_profile`).
pub const OAUTH_PROFILE: &str = "anthropic-oauth";

/// The Claude-Code-branded leading system block Anthropic's OAuth endpoint
/// requires before it accepts a subscription bearer. Ported verbatim from
/// `llm_router::models::CLAUDE_CODE_SYSTEM_PROMPT`, whose doc states OAuth tokens
/// "require the Claude-Code-branded leading system block". It is the acceptance
/// "cloak" marker, and it travels in the request BODY (`system`), not a header.
pub const CLAUDE_CODE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// The `anthropic-beta` header value the OAuth endpoint requires — it carries
/// the `oauth-2025-04-20` flag that marks the request as an OAuth-bearer call.
/// Ported verbatim from `llm_router::models::ANTHROPIC_OAUTH_BETA` (sent on both
/// `/messages` and `/models` in the native path); `crate::guest` sets it as a
/// request header, exactly the way it sets `anthropic-version`.
pub const ANTHROPIC_OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";

/// The query the native `oauth_upstream_request` appends to the `/messages` URL
/// (`{base}/messages?beta=true`).
const MESSAGES_BETA_QUERY: &str = "beta=true";

/// The `anthropic-oauth` descriptor, as wire configuration. The wire values are
/// IDENTICAL to the `anthropic` bundle's (both descriptors declare
/// `base_url = Some("https://api.anthropic.com/v1")`, `ApiFormat::Anthropic`,
/// and `has_models_endpoint = true`); they differ only in category (OAuth vs
/// ApiKey) and egress, which is not a wire concern. The context-window table is
/// EMPTY for the same reason as the `anthropic` bundle: Anthropic's `/models`
/// carries no context length and the descriptor pins none.
///
/// `provider_label` uses the descriptor `name` so a guest-visible error names
/// the Claude-subscription provider distinctly from the API-key one.
pub const CONFIG: AnthropicFormat = AnthropicFormat {
    provider_label: "Anthropic (Claude subscription)",
    default_base_url: "https://api.anthropic.com/v1",
    models_path: "/models",
    messages_path: "/messages",
    default_max_tokens: DEFAULT_MAX_TOKENS,
    context_windows: &[],
    default_context_window: DEFAULT_CONTEXT_WINDOW,
};

/// The resolved upstream base for a call: a non-blank stored override, else the
/// descriptor default.
pub fn resolve_base_url(stored: Option<&str>) -> String {
    CONFIG.resolve_base_url(stored)
}

/// The model-discovery URL (`{base}/models`) — identical to the x-api-key path.
pub fn models_url(base: &str) -> String {
    CONFIG.models_url(base)
}

/// The message-generation URL WITH the OAuth `?beta=true` query the native path
/// sends (`{base}/messages?beta=true`).
pub fn messages_url(base: &str) -> String {
    format!("{}?{}", CONFIG.messages_url(base), MESSAGES_BETA_QUERY)
}

/// Build the `/messages` body for the OAuth path: the flat prompt plus the
/// REQUIRED Claude-subscription system marker
/// ([`CLAUDE_CODE_SYSTEM_PROMPT`]). Everything else (single user turn, mandatory
/// `max_tokens`, non-finite-temperature handling) is the shared crate's.
pub fn build_completion_body(
    model: &str,
    prompt: &str,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> Vec<u8> {
    CONFIG.build_messages_body(
        model,
        prompt,
        max_tokens,
        temperature,
        Some(CLAUDE_CODE_SYSTEM_PROMPT),
    )
}

/// A host OAuth failure, mirrored host-free so the mapping to [`ProviderFail`]
/// is natively testable — the WIT `oauth-error` variant only exists in the
/// wasm32 guest build. [`crate::guest`] converts the generated variant into this
/// and calls [`map_oauth_error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthFail {
    InvalidRequest(String),
    Denied,
    Expired,
    Failed(String),
}

/// Map a host `oauth-error` onto a provider failure, credential-safe.
///
/// `denied`/`expired` become actionable `invalid-request`s (the request cannot
/// proceed until the user (re)connects — not a transient condition). The host's
/// own `oauth-error` contract keeps the access/refresh token out of every
/// message it returns, and nothing here fabricates or echoes a URL: the messages
/// this function ORIGINATES name only the provider and the action to take.
pub fn map_oauth_error(error: OAuthFail) -> ProviderFail {
    let label = CONFIG.provider_label;
    match error {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn config_matches_the_anthropic_oauth_descriptor() {
        // base_url: Some("https://api.anthropic.com/v1"); ApiFormat::Anthropic ->
        // /messages; has_models_endpoint: true -> a real /models fetch.
        assert_eq!(resolve_base_url(None), "https://api.anthropic.com/v1");
        assert_eq!(
            models_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(CONFIG.provider_label, "Anthropic (Claude subscription)");
        // The wire values match the x-api-key sibling exactly.
        assert_eq!(CONFIG.default_base_url, "https://api.anthropic.com/v1");
        assert_eq!(CONFIG.messages_path, "/messages");
    }

    #[test]
    fn messages_url_carries_the_oauth_beta_query() {
        // The native `oauth_upstream_request` POSTs to `{base}/messages?beta=true`.
        assert_eq!(
            messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        // The override base is honoured too (this is how the conformance harness
        // aims the component at its loopback mock).
        assert_eq!(
            messages_url(&resolve_base_url(Some("http://127.0.0.1:8080"))),
            "http://127.0.0.1:8080/messages?beta=true"
        );
    }

    #[test]
    fn completion_body_carries_the_required_claude_code_system_marker() {
        // The OAuth endpoint rejects a subscription bearer without the
        // Claude-Code leading system block — so unlike the x-api-key path, this
        // body MUST carry it.
        let body: Value =
            serde_json::from_slice(&build_completion_body("claude-opus-4-8", "hi", None, None))
                .unwrap();
        assert_eq!(
            body["system"],
            serde_json::json!([
                {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}
            ]),
            "the OAuth request must lead with the Claude-Code system marker",
        );
        // The flat prompt is still exactly one user turn, and max_tokens is
        // present (Anthropic rejects a request without it).
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn oauth_beta_flag_is_present_in_the_beta_header_value() {
        // The `oauth-2025-04-20` flag is what marks the request as OAuth-bearer;
        // its absence is a silent auth failure, so pin it explicitly.
        assert!(
            ANTHROPIC_OAUTH_BETA.contains("oauth-2025-04-20"),
            "the anthropic-beta value must carry the oauth-2025-04-20 flag",
        );
        assert_eq!(OAUTH_PROFILE, "anthropic-oauth");
        assert_eq!(ANTHROPIC_VERSION, "2023-06-01");
    }

    #[test]
    fn oauth_errors_map_without_leaking_a_token_and_stay_actionable() {
        // denied/expired -> actionable invalid-request naming the provider.
        assert_eq!(
            map_oauth_error(OAuthFail::Denied),
            ProviderFail::InvalidRequest(
                "Anthropic (Claude subscription) is not connected — connect it in Settings > Providers.".to_string()
            )
        );
        assert!(matches!(
            map_oauth_error(OAuthFail::Expired),
            ProviderFail::InvalidRequest(message) if message.contains("expired")
        ));
        // A host transport failure stays a Failed carrying the provider label and
        // the host's (token-free) message — this is what the conformance
        // timeout check reads as "caught it, didn't hang".
        match map_oauth_error(OAuthFail::Failed("connect timeout".to_string())) {
            ProviderFail::Failed(message) => {
                assert!(message.contains("Anthropic (Claude subscription)"));
                assert!(message.contains("failed"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // A host-originated invalid-request message passes through unchanged.
        assert_eq!(
            map_oauth_error(OAuthFail::InvalidRequest("bad method".to_string())),
            ProviderFail::InvalidRequest("bad method".to_string())
        );
    }

    #[test]
    fn a_mapped_oauth_error_never_contains_a_bearer_token() {
        // Belt-and-braces: even if a hostile host message tried to smuggle a
        // token, our denied/expired arms originate their OWN text and never
        // interpolate the host message.
        let rendered = format!("{:?}", map_oauth_error(OAuthFail::Denied));
        assert!(!rendered.to_lowercase().contains("bearer"));
        assert!(!rendered.contains("access_token"));
    }
}
