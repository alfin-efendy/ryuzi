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
}

use ProviderCategory::*;

pub const CATALOG: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "anthropic", name: "Anthropic", color: "#D97757", initial: "A",
        category: ApiKey, format: ApiFormat::Anthropic,
        base_url: Some("https://api.anthropic.com/v1"), auth: AuthScheme::XApiKey,
        models: &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"],
        requires_base_url: false,
    },
    ProviderDescriptor {
        id: "openai", name: "OpenAI", color: "#0FA47F", initial: "O",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://api.openai.com/v1"), auth: AuthScheme::Bearer,
        models: &["gpt-5.2", "gpt-5.2-codex", "o5-mini"],
        requires_base_url: false,
    },
    ProviderDescriptor {
        id: "openrouter", name: "OpenRouter", color: "#6E56CF", initial: "R",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://openrouter.ai/api/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "groq", name: "Groq", color: "#F55036", initial: "G",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://api.groq.com/openai/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "deepseek", name: "DeepSeek", color: "#4D6BFE", initial: "D",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://api.deepseek.com/v1"), auth: AuthScheme::Bearer,
        models: &["deepseek-chat", "deepseek-reasoner"], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "mistral", name: "Mistral", color: "#FA5111", initial: "M",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://api.mistral.ai/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "xai", name: "xAI", color: "#9CA3AF", initial: "X",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://api.x.ai/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "google", name: "Google (Gemini)", color: "#4285F4", initial: "G",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        auth: AuthScheme::Bearer,
        models: &["gemini-3.0-pro", "gemini-3.0-flash"], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "ollama", name: "Ollama (local)", color: "#8B8B8B", initial: "L",
        category: ApiKey, format: ApiFormat::OpenAi,
        base_url: Some("http://127.0.0.1:11434/v1"), auth: AuthScheme::None,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "custom-openai", name: "Custom (OpenAI-compatible)", color: "#8B8B8B",
        initial: "C", category: ApiKey, format: ApiFormat::OpenAi,
        base_url: None, auth: AuthScheme::Bearer, models: &[], requires_base_url: true,
    },
    ProviderDescriptor {
        id: "custom-anthropic", name: "Custom (Anthropic-compatible)", color: "#8B8B8B",
        initial: "C", category: ApiKey, format: ApiFormat::Anthropic,
        base_url: None, auth: AuthScheme::XApiKey, models: &[], requires_base_url: true,
    },
    // F2/F3 teasers: visible in the catalog, greyed "Coming soon" in the UI.
    // Not connectable in F1 (add_connection refuses non-ApiKey categories).
    ProviderDescriptor {
        id: "anthropic-oauth", name: "Anthropic (Claude subscription)", color: "#D97757",
        initial: "A", category: OAuth, format: ApiFormat::Anthropic,
        base_url: Some("https://api.anthropic.com/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "openai-oauth", name: "OpenAI (ChatGPT)", color: "#0FA47F",
        initial: "O", category: OAuth, format: ApiFormat::OpenAi,
        base_url: Some("https://api.openai.com/v1"), auth: AuthScheme::Bearer,
        models: &[], requires_base_url: false,
    },
    ProviderDescriptor {
        id: "kiro", name: "Kiro (free tier)", color: "#7C3AED",
        initial: "K", category: Free, format: ApiFormat::OpenAi,
        base_url: None, auth: AuthScheme::Bearer,
        models: &[], requires_base_url: true,
    },
];

pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    CATALOG.iter().find(|d| d.id == id)
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
        assert_eq!(descriptor("anthropic").unwrap().format, ApiFormat::Anthropic);
        assert_eq!(descriptor("openai").unwrap().format, ApiFormat::OpenAi);
        assert!(descriptor("nope").is_none());
    }

    #[test]
    fn f1_catalog_is_api_key_only_active() {
        // OAuth/free entries may exist but must be marked by category so the
        // UI can grey them out; every ApiKey entry must be usable.
        for d in CATALOG.iter().filter(|d| d.category == ProviderCategory::ApiKey) {
            assert!(d.requires_base_url || d.base_url.is_some());
        }
    }

    #[test]
    fn catalog_has_oauth_and_free_teasers() {
        // Phase 1 shows OAuth/Free entries in the catalog, greyed "Coming
        // soon" in the UI; add_connection refuses to activate them.
        assert!(CATALOG.iter().any(|d| d.category == ProviderCategory::OAuth));
        assert!(CATALOG.iter().any(|d| d.category == ProviderCategory::Free));
    }
}
