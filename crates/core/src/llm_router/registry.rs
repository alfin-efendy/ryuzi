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
    /// Vendor family this descriptor belongs to. Descriptors sharing a family
    /// (e.g. `anthropic` + `anthropic-oauth`) render as ONE provider in the UI
    /// and pool their accounts for routing/failover. The family id is always
    /// the id of a catalog entry (the "family head") whose display identity
    /// (name/color/initial) represents the group.
    pub family: &'static str,
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
    /// Set for providers that authenticate via an AWS SSO-OIDC device-code
    /// flow (Kiro) rather than redirect+PKCE OAuth or a static API key.
    pub device_flow: Option<DeviceFlowConfig>,
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

/// AWS SSO-OIDC device-code flow config (Kiro). Distinct from `OAuthConfig`
/// (redirect+PKCE): device flow has register/device-auth/token endpoints and
/// no redirect. Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
pub struct DeviceFlowConfig {
    pub register_url: &'static str,
    pub device_auth_url: &'static str,
    pub token_url: &'static str,
    pub client_name: &'static str,
    pub scopes: &'static [&'static str],
    pub grant_types: &'static [&'static str],
    pub issuer_url: &'static str,
    pub start_url: &'static str,
    /// Proactive refresh lead (ms). Kiro has no provider-specific value in
    /// 9router; use the generic 5-minute buffer.
    pub refresh_lead_ms: i64,
}

pub const KIRO_DEVICE_FLOW: DeviceFlowConfig = DeviceFlowConfig {
    register_url: "https://oidc.us-east-1.amazonaws.com/client/register",
    device_auth_url: "https://oidc.us-east-1.amazonaws.com/device_authorization",
    token_url: "https://oidc.us-east-1.amazonaws.com/token",
    client_name: "kiro-oauth-client",
    scopes: &[
        "codewhisperer:completions",
        "codewhisperer:analysis",
        "codewhisperer:conversations",
    ],
    grant_types: &[
        "urn:ietf:params:oauth:grant-type:device_code",
        "refresh_token",
    ],
    issuer_url: "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6",
    start_url: "https://view.awsapps.com/start",
    refresh_lead_ms: 300_000,
};

use ProviderCategory::*;

pub const CATALOG: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "anthropic",
        name: "Anthropic",
        family: "anthropic",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "openai",
        name: "OpenAI",
        family: "openai",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "openrouter",
        name: "OpenRouter",
        family: "openrouter",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "groq",
        name: "Groq",
        family: "groq",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "deepseek",
        name: "DeepSeek",
        family: "deepseek",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "mistral",
        name: "Mistral",
        family: "mistral",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "xai",
        name: "xAI",
        family: "xai",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "google",
        name: "Google (Gemini)",
        family: "google",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "ollama",
        name: "Ollama (local)",
        family: "ollama",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "custom-openai",
        name: "Custom (OpenAI-compatible)",
        family: "custom-openai",
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "custom-anthropic",
        name: "Custom (Anthropic-compatible)",
        family: "custom-anthropic",
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
        device_flow: None,
    },
    // F2/F3 teasers: visible in the catalog, greyed "Coming soon" in the UI.
    // Not connectable in F1 (add_connection refuses non-ApiKey categories).
    ProviderDescriptor {
        id: "anthropic-oauth",
        name: "Anthropic (Claude subscription)",
        family: "anthropic",
        color: "#D97757",
        initial: "A",
        category: OAuth,
        format: ApiFormat::Anthropic,
        base_url: Some("https://api.anthropic.com/v1"),
        auth: AuthScheme::Bearer,
        models: &[
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
        ],
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
        device_flow: None,
    },
    ProviderDescriptor {
        id: "openai-oauth",
        name: "OpenAI (ChatGPT)",
        family: "openai",
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
        device_flow: None,
        // NOTE: openai-oauth upstream is chatgpt.com/backend-api/codex/responses
        // (Responses wire) — applied in server.rs, not via base_url here.
    },
    ProviderDescriptor {
        id: "kiro",
        name: "Kiro (free tier)",
        family: "kiro",
        color: "#7C3AED",
        initial: "K",
        category: Free,
        format: ApiFormat::OpenAi,
        base_url: None,
        auth: AuthScheme::Bearer,
        models: &[
            "claude-sonnet-5",
            "claude-sonnet-4.5",
            "claude-haiku-4.5",
            "deepseek-3.2",
            "qwen3-coder-next",
            "glm-5",
            "MiniMax-M2.5",
        ],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: Some(KIRO_DEVICE_FLOW),
    },
    ProviderDescriptor {
        id: "opencode-free",
        name: "OpenCode (free)",
        family: "opencode-free",
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
        device_flow: None,
    },
];

pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    CATALOG.iter().find(|d| d.id == id)
}

pub fn family_of(id: &str) -> Option<&'static str> {
    descriptor(id).map(|d| d.family)
}

pub fn oauth_config(id: &str) -> Option<&'static OAuthConfig> {
    descriptor(id).and_then(|d| d.oauth.as_ref())
}

pub fn device_flow_config(id: &str) -> Option<&'static DeviceFlowConfig> {
    descriptor(id).and_then(|d| d.device_flow.as_ref())
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
            if !d.requires_base_url && d.device_flow.is_none() {
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
    fn anthropic_oauth_has_seed_models_when_live_discovery_is_unavailable() {
        let d = descriptor("anthropic-oauth").unwrap();
        assert!(d.models.contains(&"claude-opus-4-8"));
        assert!(d.models.contains(&"claude-sonnet-4-6"));
        assert!(d.models.contains(&"claude-haiku-4-5-20251001"));
    }

    #[test]
    fn opencode_free_is_no_auth_free() {
        let d = descriptor("opencode-free").unwrap();
        assert_eq!(d.category, ProviderCategory::Free);
        assert!(d.no_auth);
        assert_eq!(d.base_url, Some("https://opencode.ai/zen/v1"));
    }

    #[test]
    fn kiro_is_a_device_flow_provider() {
        let d = descriptor("kiro").expect("kiro present");
        assert!(matches!(d.category, ProviderCategory::Free));
        assert!(d.oauth.is_none());
        let df = device_flow_config("kiro").expect("kiro device flow");
        assert_eq!(
            df.register_url,
            "https://oidc.us-east-1.amazonaws.com/client/register"
        );
        assert_eq!(
            df.scopes,
            &[
                "codewhisperer:completions",
                "codewhisperer:analysis",
                "codewhisperer:conversations"
            ]
        );
        assert_eq!(
            df.issuer_url,
            "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6"
        );
        assert_eq!(df.refresh_lead_ms, 300_000);
        assert!(d.models.contains(&"claude-sonnet-4.5"));
    }

    #[test]
    fn families_are_wellformed() {
        for d in CATALOG {
            // every family resolves to a "family head" descriptor whose id IS the family
            let head = descriptor(d.family).expect("family head exists");
            assert_eq!(head.family, head.id, "family head {} must be its own family", head.id);
        }
    }

    #[test]
    fn oauth_variants_join_vendor_families() {
        assert_eq!(family_of("anthropic-oauth"), Some("anthropic"));
        assert_eq!(family_of("openai-oauth"), Some("openai"));
        assert_eq!(family_of("anthropic"), Some("anthropic"));
        assert_eq!(family_of("kiro"), Some("kiro"));
        assert_eq!(family_of("custom-openai"), Some("custom-openai"));
        assert_eq!(family_of("nope"), None);
    }
}
