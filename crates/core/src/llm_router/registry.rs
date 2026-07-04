//! Static, declarative provider catalog for the local router.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! concept and provider list from open-sse/providers/registry/.

/// Wire format the provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFormat {
    Anthropic,
    OpenAi,
}

/// How the upstream credential is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// `x-api-key: <key>` (+ `anthropic-version` header).
    XApiKey,
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// No credential (local endpoints like Ollama).
    None,
}

/// F1 activates ApiKey only; OAuth/Free are greyed out in the catalog UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCategory {
    ApiKey,
    OAuth,
    Free,
}

pub struct ProviderDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub color: &'static str,
    pub initial: &'static str,
    pub category: ProviderCategory,
    pub format: ApiFormat,
    /// Chat base. OpenAi format: ends with `/v1` (path `/chat/completions`
    /// is appended). Anthropic format: ends with `/v1` (path `/messages`).
    pub base_url: Option<&'static str>,
    pub auth: AuthScheme,
    /// Seed model list shown before the user overrides per connection.
    pub models: &'static [&'static str],
    /// True for the generic `custom-*` entries: user must supply a base URL.
    pub requires_base_url: bool,
    /// Set for OAuth-category providers that can actually run the flow.
    pub oauth: Option<OAuthConfig>,
    /// True for free-tier passthrough providers that need no credential
    /// (distinct from `AuthScheme::None`, which just means "no header sent").
    pub no_auth: bool,
}

/// How the OAuth redirect (loopback) listener is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    /// Random loopback port, path `/callback` (Anthropic).
    LoopbackRandom,
    /// Fixed loopback port + path `/auth/callback` (Codex requires 1455).
    LoopbackFixed(u16),
}

pub struct OAuthConfig {
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scope: &'static str,
    pub redirect: RedirectMode,
    pub refresh_lead_ms: i64,
    pub max_refresh_age_ms: Option<i64>,
}

use ProviderCategory::*;

pub const CATALOG: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "anthropic",
        name: "Anthropic",
        color: "#D97757",
        initial: "A",
        category: ApiKey,
        format: ApiFormat::Anthropic,
        base_url: Some("https://api.anthropic.com/v1"),
        auth: AuthScheme::XApiKey,
        models: &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "openai",
        name: "OpenAI",
        color: "#0FA47F",
        initial: "O",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.openai.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["gpt-5.2", "gpt-5.2-codex", "o5-mini"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "openrouter",
        name: "OpenRouter",
        color: "#6E56CF",
        initial: "R",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://openrouter.ai/api/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "groq",
        name: "Groq",
        color: "#F55036",
        initial: "G",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.groq.com/openai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "deepseek",
        name: "DeepSeek",
        color: "#4D6BFE",
        initial: "D",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.deepseek.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["deepseek-chat", "deepseek-reasoner"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "mistral",
        name: "Mistral",
        color: "#FA5111",
        initial: "M",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.mistral.ai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "xai",
        name: "xAI",
        color: "#9CA3AF",
        initial: "X",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.x.ai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "google",
        name: "Google (Gemini)",
        color: "#4285F4",
        initial: "G",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        auth: AuthScheme::Bearer,
        models: &["gemini-3.0-pro", "gemini-3.0-flash"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "ollama",
        name: "Ollama (local)",
        color: "#8B8B8B",
        initial: "L",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: Some("http://127.0.0.1:11434/v1"),
        auth: AuthScheme::None,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "custom-openai",
        name: "Custom (OpenAI-compatible)",
        color: "#8B8B8B",
        initial: "C",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        base_url: None,
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: true,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "custom-anthropic",
        name: "Custom (Anthropic-compatible)",
        color: "#8B8B8B",
        initial: "C",
        category: ApiKey,
        format: ApiFormat::Anthropic,
        base_url: None,
        auth: AuthScheme::XApiKey,
        models: &[],
        requires_base_url: true,
        oauth: None,
        no_auth: false,
    },
    // F2/F3 teasers: visible in the catalog, greyed "Coming soon" in the UI.
    // Not connectable in F1 (add_connection refuses non-ApiKey categories).
    ProviderDescriptor {
        id: "anthropic-oauth",
        name: "Anthropic (Claude subscription)",
        color: "#D97757",
        initial: "A",
        category: OAuth,
        format: ApiFormat::Anthropic,
        base_url: Some("https://api.anthropic.com/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: Some(OAuthConfig {
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            authorize_url: "https://claude.ai/oauth/authorize",
            token_url: "https://api.anthropic.com/v1/oauth/token",
            scope: "org:create_api_key user:profile user:inference",
            redirect: RedirectMode::LoopbackRandom,
            refresh_lead_ms: 14_400_000,
            max_refresh_age_ms: None,
        }),
        no_auth: false,
    },
    ProviderDescriptor {
        id: "openai-oauth",
        name: "OpenAI (ChatGPT)",
        color: "#0FA47F",
        initial: "O",
        category: OAuth,
        format: ApiFormat::OpenAi,
        base_url: Some("https://api.openai.com/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: Some(OAuthConfig {
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
            authorize_url: "https://auth.openai.com/oauth/authorize",
            token_url: "https://auth.openai.com/oauth/token",
            scope: "openid profile email offline_access",
            redirect: RedirectMode::LoopbackFixed(1455),
            refresh_lead_ms: 432_000_000,
            max_refresh_age_ms: Some(691_200_000),
        }),
        no_auth: false,
        // NOTE: openai-oauth upstream is chatgpt.com/backend-api/codex/responses
        // (Responses wire) — applied in server.rs, not via base_url here.
    },
    ProviderDescriptor {
        id: "kiro",
        name: "Kiro (free tier)",
        color: "#7C3AED",
        initial: "K",
        category: Free,
        format: ApiFormat::OpenAi,
        base_url: None,
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: true,
        oauth: None,
        no_auth: false,
    },
    ProviderDescriptor {
        id: "opencode-free",
        name: "OpenCode (free)",
        color: "#F5A623",
        initial: "OC",
        category: Free,
        format: ApiFormat::OpenAi,
        base_url: Some("https://opencode.ai/zen/v1"),
        auth: AuthScheme::None,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: true,
    },
];

pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    CATALOG.iter().find(|d| d.id == id)
}

pub fn oauth_config(id: &str) -> Option<&'static OAuthConfig> {
    descriptor(id).and_then(|d| d.oauth.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_wellformed() {
        let mut seen = std::collections::HashSet::new();
        for d in CATALOG {
            assert!(seen.insert(d.id), "duplicate id {}", d.id);
            assert!(!d.name.is_empty());
            if !d.requires_base_url {
                let url = d.base_url.expect("fixed-URL provider needs base_url");
                assert!(url.starts_with("https://") || url.starts_with("http://127.0.0.1"));
                assert!(!url.ends_with('/'), "no trailing slash: {}", url);
            }
        }
    }

    #[test]
    fn descriptor_lookup_works() {
        assert_eq!(
            descriptor("anthropic").unwrap().format,
            ApiFormat::Anthropic
        );
        assert_eq!(descriptor("openai").unwrap().format, ApiFormat::OpenAi);
        assert!(descriptor("nope").is_none());
    }

    #[test]
    fn f1_catalog_is_api_key_only_active() {
        // OAuth/free entries may exist but must be marked by category so the
        // UI can grey them out; every ApiKey entry must be usable.
        for d in CATALOG
            .iter()
            .filter(|d| d.category == ProviderCategory::ApiKey)
        {
            assert!(d.requires_base_url || d.base_url.is_some());
        }
    }

    #[test]
    fn catalog_has_oauth_and_free_teasers() {
        // Phase 1 shows OAuth/Free entries in the catalog, greyed "Coming
        // soon" in the UI; add_connection refuses to activate them.
        assert!(CATALOG
            .iter()
            .any(|d| d.category == ProviderCategory::OAuth));
        assert!(CATALOG.iter().any(|d| d.category == ProviderCategory::Free));
    }

    #[test]
    fn oauth_providers_have_config_with_verbatim_constants() {
        let a = oauth_config("anthropic-oauth").unwrap();
        assert_eq!(a.client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(a.authorize_url, "https://claude.ai/oauth/authorize");
        assert!(matches!(a.redirect, RedirectMode::LoopbackRandom));
        let o = oauth_config("openai-oauth").unwrap();
        assert_eq!(o.client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
        assert!(matches!(o.redirect, RedirectMode::LoopbackFixed(1455)));
        assert_eq!(o.max_refresh_age_ms, Some(691_200_000));
    }

    #[test]
    fn opencode_free_is_no_auth_free() {
        let d = descriptor("opencode-free").unwrap();
        assert_eq!(d.category, ProviderCategory::Free);
        assert!(d.no_auth);
        assert_eq!(d.base_url, Some("https://opencode.ai/zen/v1"));
    }
}
