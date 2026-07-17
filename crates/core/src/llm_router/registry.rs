//! Static, declarative provider catalog for the local router.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! concept and provider list from open-sse/providers/registry/.

use crate::harness::native::capabilities::{TransportToolCapabilities, WireProtocol};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

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

/// Whether a connection uses the endpoint declared by the provider catalog
/// or substitutes a user-configured compatible endpoint. Catalog-specific
/// extensions cannot be assumed for an override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderEndpointSource {
    Catalog,
    ConnectionOverride,
}

/// Typed facts about the provider adapter's tool wire contract. These facts
/// belong to the adapter, never to a requested model identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderToolTransport {
    capabilities: TransportToolCapabilities,
}

impl ProviderToolTransport {
    /// Conservative transport contract derived only from the declared wire
    /// format. Runtime-defined providers use this constructor because their
    /// endpoints are compatibility surfaces, not first-party extensions.
    pub const fn for_format(format: ApiFormat) -> Self {
        match format {
            ApiFormat::Anthropic => Self::ANTHROPIC_FUNCTIONS,
            ApiFormat::OpenAi => Self::OPENAI_FUNCTIONS,
        }
    }

    pub const ANTHROPIC_FUNCTIONS: Self = Self {
        capabilities: TransportToolCapabilities::function_only(WireProtocol::AnthropicMessages),
    };
    pub const OPENAI_FUNCTIONS: Self = Self {
        capabilities: TransportToolCapabilities::function_only(WireProtocol::OpenAiChat),
    };
    pub const OPENAI_STRICT_CHAT: Self = Self {
        capabilities: TransportToolCapabilities {
            wire_protocol: WireProtocol::OpenAiChat,
            supports_function_tools: true,
            supports_custom_freeform_tools: false,
            supports_parallel_tool_calls: true,
            supports_strict_function_schema: true,
            supports_tool_output_schema: false,
            schema_budget_tokens: 16_000,
        },
    };
    pub const OPENAI_STRICT_RESPONSES: Self = Self {
        capabilities: TransportToolCapabilities {
            wire_protocol: WireProtocol::OpenAiResponses,
            supports_function_tools: true,
            supports_custom_freeform_tools: true,
            supports_parallel_tool_calls: true,
            supports_strict_function_schema: true,
            supports_tool_output_schema: true,
            schema_budget_tokens: 16_000,
        },
    };

    pub const fn capabilities(self) -> TransportToolCapabilities {
        self.capabilities
    }

    pub const fn capabilities_for_endpoint(
        self,
        endpoint_source: ProviderEndpointSource,
    ) -> TransportToolCapabilities {
        match endpoint_source {
            ProviderEndpointSource::Catalog => self.capabilities,
            ProviderEndpointSource::ConnectionOverride => {
                TransportToolCapabilities::function_only(self.capabilities.wire_protocol)
            }
        }
    }
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
    pub tool_transport: ProviderToolTransport,
    /// Chat base. OpenAi format usually ends with `/v1` (path
    /// `/chat/completions` is appended); a vendor host without a version
    /// segment (e.g. `github-copilot` → `https://api.githubcopilot.com`) is
    /// also valid — the path is still appended directly. Anthropic format
    /// ends with `/v1` (path `/messages`).
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
    /// API-key provider with a genuinely free usage path (free-tier key).
    /// Drives the "Free tier" badge in the UI; orthogonal to `category`.
    pub free_tier: bool,
    /// Reuses a consumer subscription/quota through unofficial endpoints —
    /// the UI must warn that this can risk account suspension.
    pub risk_notice: bool,
    /// Nonstandard chat-generation path appended to the base URL instead of
    /// the wire format's default (`/chat/completions` | `/messages`).
    /// mimo-free's endpoint ends at `/chat`.
    pub chat_path: Option<&'static str>,
    /// Whether this provider exposes an OpenAI-compatible /models list endpoint
    /// we can fetch. False for providers that only ship a seeded model list
    /// (their /models route 404s), so the refresh path must not treat the
    /// absence of a live catalog as an error.
    pub has_models_endpoint: bool,
    /// OpenAI's current generation (gpt-5.x / o-series) rejects `max_tokens`
    /// with HTTP 400 and requires `max_completion_tokens` instead. When set,
    /// OpenAI-format request bodies get the field renamed post-translation
    /// (both the model probe and the real chat path). True ONLY for the
    /// first-party `openai` entry — every other OpenAI-compatible provider
    /// still speaks `max_tokens`.
    pub uses_max_completion_tokens: bool,
    /// RFC 8628 device-authorization grant config (Qwen, GitHub Copilot).
    /// Mutually exclusive with `oauth` and `device_flow`. Ported constants
    /// from 9router (MIT, (c) 2024-2026 decolua and contributors).
    pub device_grant: Option<DeviceGrantConfig>,
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

/// RFC 8628 OAuth 2.0 device-authorization grant config (Qwen Code, GitHub
/// Copilot). Distinct from `DeviceFlowConfig` (Kiro's AWS SSO-OIDC flow):
/// there is no dynamic client registration — a static `client_id` is used,
/// with optional PKCE and an optional second-leg token exchange (Copilot).
pub struct DeviceGrantConfig {
    pub client_id: &'static str,
    pub device_code_url: &'static str,
    pub token_url: &'static str,
    pub scope: &'static str,
    /// RFC 7636 PKCE (S256). Qwen: true; GitHub: false.
    pub use_pkce: bool,
    /// Second-leg token exchange (GitHub Copilot only): the GitHub token is
    /// swapped for a short-lived Copilot token. `None` for Qwen.
    pub token_exchange: Option<CopilotTokenExchange>,
    /// Proactive refresh lead (ms).
    pub refresh_lead_ms: i64,
}

/// GitHub → Copilot token exchange endpoint. GET with
/// `Authorization: token <gh_token>` returns `{ token, expires_at }`.
pub struct CopilotTokenExchange {
    pub url: &'static str,
}

pub const QWEN_DEVICE_GRANT: DeviceGrantConfig = DeviceGrantConfig {
    client_id: "f0304373b74a44d2b584a3fb70ca9e56",
    device_code_url: "https://chat.qwen.ai/api/v1/oauth2/device/code",
    token_url: "https://chat.qwen.ai/api/v1/oauth2/token",
    scope: "openid profile email model.completion",
    use_pkce: true,
    token_exchange: None,
    refresh_lead_ms: 1_200_000,
};

pub const GITHUB_DEVICE_GRANT: DeviceGrantConfig = DeviceGrantConfig {
    client_id: "Iv1.b507a08c87ecfe98",
    device_code_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    scope: "read:user",
    use_pkce: false,
    token_exchange: Some(CopilotTokenExchange {
        url: "https://api.github.com/copilot_internal/v2/token",
    }),
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
        tool_transport: ProviderToolTransport::ANTHROPIC_FUNCTIONS,
        base_url: Some("https://api.anthropic.com/v1"),
        auth: AuthScheme::XApiKey,
        models: &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "openai",
        name: "OpenAI",
        family: "openai",
        color: "#0FA47F",
        initial: "O",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_STRICT_CHAT,
        base_url: Some("https://api.openai.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["gpt-5.2", "gpt-5.2-codex", "o5-mini"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: true,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "openrouter",
        name: "OpenRouter",
        family: "openrouter",
        color: "#6E56CF",
        initial: "R",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://openrouter.ai/api/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "groq",
        name: "Groq",
        family: "groq",
        color: "#F55036",
        initial: "G",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.groq.com/openai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "deepseek",
        name: "DeepSeek",
        family: "deepseek",
        color: "#4D6BFE",
        initial: "D",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.deepseek.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["deepseek-chat", "deepseek-reasoner"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "mistral",
        name: "Mistral",
        family: "mistral",
        color: "#FA5111",
        initial: "M",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.mistral.ai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "xai",
        name: "xAI",
        family: "xai",
        color: "#9CA3AF",
        initial: "X",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.x.ai/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "google",
        name: "Google (Gemini)",
        family: "google",
        color: "#4285F4",
        initial: "G",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        auth: AuthScheme::Bearer,
        models: &["gemini-3.0-pro", "gemini-3.0-flash"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "ollama",
        name: "Ollama (local)",
        family: "ollama",
        color: "#8B8B8B",
        initial: "L",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("http://127.0.0.1:11434/v1"),
        auth: AuthScheme::None,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
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
        tool_transport: ProviderToolTransport::ANTHROPIC_FUNCTIONS,
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
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "openai-oauth",
        name: "OpenAI (ChatGPT)",
        family: "openai",
        color: "#0FA47F",
        initial: "O",
        category: OAuth,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_STRICT_RESPONSES,
        base_url: Some("https://api.openai.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["gpt-5.2-codex", "gpt-5.2", "o5-mini"],
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
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
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
        tool_transport: ProviderToolTransport::ANTHROPIC_FUNCTIONS,
        base_url: None,
        auth: AuthScheme::Bearer,
        models: &[
            "claude-sonnet-5",
            "claude-sonnet-4.5",
            "claude-haiku-4.5",
            "deepseek-3.2",
            "qwen3-coder-next",
            "glm-5",
            // CodeWhisperer model ids are lowercase; "MiniMax-M2.5" is
            // rejected upstream with 400 INVALID_MODEL_ID.
            "minimax-m2.5",
        ],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: Some(KIRO_DEVICE_FLOW),
        free_tier: false,
        risk_notice: true,
        chat_path: None,
        // No live /models route — CodeWhisperer isn't OpenAI-compatible for
        // discovery; the seeded list above stands and "Refresh models"
        // reports it instead of erroring.
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "opencode-free",
        name: "OpenCode (free)",
        family: "opencode-free",
        color: "#F5A623",
        initial: "OC",
        category: Free,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://opencode.ai/zen/v1"),
        auth: AuthScheme::None,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: true,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "opencode",
        name: "OpenCode (Go)",
        family: "opencode-free",
        color: "#F5A623",
        initial: "OC",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::for_format(ApiFormat::OpenAi),
        // OpenCode Go subscription ($5/mo); key from https://opencode.ai/auth.
        base_url: Some("https://opencode.ai/zen/go/v1"),
        auth: AuthScheme::Bearer,
        models: &[
            "glm-5.2",
            "kimi-k2.7-code",
            "deepseek-v4-pro",
            "mimo-v2.5-pro",
        ],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        // Seed list stands; the /models route is not relied upon here.
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "mimo-free",
        name: "MiMo (free)",
        family: "mimo-free",
        color: "#FF6900",
        initial: "M",
        category: Free,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.xiaomimimo.com/api/free-ai/openai"),
        auth: AuthScheme::None,
        // No /models endpoint on this host (9router discovers via models.dev)
        // — the live refresh 404s harmlessly and these seeds stand.
        models: &["mimo-auto"],
        requires_base_url: false,
        oauth: None,
        no_auth: true,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: Some("/chat"),
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "mimo",
        name: "MiMo (Token Plan)",
        family: "mimo-free",
        color: "#FF6900",
        initial: "M",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::for_format(ApiFormat::OpenAi),
        // Xiaomi MiMo Token Plan (key starts with `tp-`); key from
        // https://mimo.xiaomi.com. The region-specific host
        // (token-plan-<sgp|cn|ams>.xiaomimimo.com) is chosen in the Add Account
        // region picker and stored as the connection's base_url_override; this
        // static default is the sgp cluster.
        base_url: Some("https://token-plan-sgp.xiaomimimo.com/v1"),
        auth: AuthScheme::Bearer,
        models: &["mimo-v2.5-pro", "mimo-v2.5"],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "nvidia",
        name: "NVIDIA NIM",
        family: "nvidia",
        color: "#76B900",
        initial: "N",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://integrate.api.nvidia.com/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "huggingface",
        name: "Hugging Face",
        family: "huggingface",
        color: "#FFD21E",
        initial: "H",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://router.huggingface.co/v1"),
        auth: AuthScheme::Bearer,
        models: &[],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: true,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "cloudflare-ai",
        name: "Cloudflare Workers AI",
        family: "cloudflare-ai",
        color: "#F6821F",
        initial: "CF",
        category: ApiKey,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        // User pastes https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1
        base_url: None,
        auth: AuthScheme::Bearer,
        // The account URL has no OpenAI-compatible /models route — seed a
        // few Workers AI text models; users can override per connection.
        models: &[
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
            "@cf/openai/gpt-oss-120b",
            "@cf/meta/llama-3.1-8b-instruct",
        ],
        requires_base_url: true,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: true,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: None,
    },
    ProviderDescriptor {
        id: "qwen",
        name: "Qwen Code",
        family: "qwen",
        color: "#10B981",
        initial: "Q",
        category: OAuth,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://portal.qwen.ai/v1"),
        auth: AuthScheme::Bearer,
        models: &[
            "qwen3-coder-plus",
            "qwen3-coder-flash",
            "vision-model",
            "coder-model",
        ],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: false,
        chat_path: None,
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: Some(QWEN_DEVICE_GRANT),
    },
    ProviderDescriptor {
        id: "github-copilot",
        name: "GitHub Copilot",
        family: "github-copilot",
        color: "#333333",
        initial: "GH",
        category: OAuth,
        format: ApiFormat::OpenAi,
        tool_transport: ProviderToolTransport::OPENAI_FUNCTIONS,
        base_url: Some("https://api.githubcopilot.com"),
        auth: AuthScheme::Bearer,
        models: &[
            "gpt-5.2",
            "gpt-5.2-codex",
            "gpt-5.4",
            "claude-opus-4.5",
            "claude-sonnet-4.6",
            "gemini-3.1-pro-preview",
            "grok-code-fast-1",
        ],
        requires_base_url: false,
        oauth: None,
        no_auth: false,
        device_flow: None,
        free_tier: false,
        risk_notice: true,
        chat_path: None,
        has_models_endpoint: false,
        uses_max_completion_tokens: false,
        device_grant: Some(GITHUB_DEVICE_GRANT),
    },
];

/// Runtime cache of user-defined custom providers, each leaked to `&'static`
/// so `descriptor` can return it like a built-in. Keyed by provider id.
static CUSTOM_DESCRIPTORS: OnceLock<Mutex<HashMap<String, &'static ProviderDescriptor>>> =
    OnceLock::new();

fn custom_cache() -> &'static Mutex<HashMap<String, &'static ProviderDescriptor>> {
    CUSTOM_DESCRIPTORS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Insert (or replace) a leaked custom descriptor into the cache.
pub fn register_custom_descriptor(desc: &'static ProviderDescriptor) {
    custom_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(desc.id.to_string(), desc);
}

/// Drop a custom descriptor from the cache. The leaked allocation is not
/// reclaimed, but the id becomes unresolvable.
pub fn unregister_custom_descriptor(id: &str) {
    custom_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(id);
}

/// Resolve a provider id to its descriptor. Checks the static `CATALOG` first
/// (no lock, keeping the routing hot path lock-free for built-ins), then the
/// leaked custom-provider cache on a miss.
pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    if let Some(d) = CATALOG.iter().find(|d| d.id == id) {
        return Some(d);
    }
    custom_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .copied()
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

pub fn device_grant_config(id: &str) -> Option<&'static DeviceGrantConfig> {
    descriptor(id).and_then(|d| d.device_grant.as_ref())
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
    fn first_party_openai_responses_declares_its_typed_tool_contract() {
        let capabilities = descriptor("openai-oauth")
            .unwrap()
            .tool_transport
            .capabilities();

        assert_eq!(
            capabilities.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::OpenAiResponses
        );
        assert!(capabilities.supports_function_tools);
        assert!(capabilities.supports_custom_freeform_tools);
        assert!(capabilities.supports_strict_function_schema);
        assert!(capabilities.supports_tool_output_schema);
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
    fn openai_oauth_has_seed_models_when_live_discovery_is_unavailable() {
        let d = descriptor("openai-oauth").unwrap();
        assert!(
            !d.models.is_empty(),
            "empty seed makes an OAuth-only OpenAI family contribute zero models"
        );
        assert!(d.models.contains(&"gpt-5.2-codex"));
        assert!(d.models.contains(&"gpt-5.2"));
        assert!(d.models.contains(&"o5-mini"));
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
            assert_eq!(
                head.family, head.id,
                "family head {} must be its own family",
                head.id
            );
        }
    }

    #[test]
    fn oauth_variants_join_vendor_families() {
        assert_eq!(family_of("anthropic-oauth"), Some("anthropic"));
        assert_eq!(family_of("openai-oauth"), Some("openai"));
        assert_eq!(family_of("anthropic"), Some("anthropic"));
        assert_eq!(family_of("kiro"), Some("kiro"));
        assert_eq!(family_of("nope"), None);
    }

    #[test]
    fn mimo_and_opencode_subscription_members_join_the_free_family() {
        // Subscription members share the free tier's family head so they pool
        // into one provider card and trigger the Free/Subscription chooser.
        let mimo = descriptor("mimo").expect("mimo descriptor");
        assert_eq!(mimo.family, "mimo-free");
        assert_eq!(mimo.category, ProviderCategory::ApiKey);
        assert!(matches!(mimo.format, ApiFormat::OpenAi));
        assert!(matches!(mimo.auth, AuthScheme::Bearer));
        assert_eq!(family_of("mimo"), Some("mimo-free"));

        let oc = descriptor("opencode").expect("opencode descriptor");
        assert_eq!(oc.family, "opencode-free");
        assert_eq!(oc.category, ProviderCategory::ApiKey);
        assert!(matches!(oc.auth, AuthScheme::Bearer));
        assert_eq!(family_of("opencode"), Some("opencode-free"));
    }

    #[test]
    fn phase_a_free_and_free_tier_providers_are_wellformed() {
        let mimo = descriptor("mimo-free").unwrap();
        assert_eq!(mimo.category, ProviderCategory::Free);
        assert!(mimo.no_auth);
        assert_eq!(
            mimo.base_url,
            Some("https://api.xiaomimimo.com/api/free-ai/openai")
        );
        assert_eq!(mimo.chat_path, Some("/chat"));
        assert_eq!(mimo.models, &["mimo-auto"]);

        let nvidia = descriptor("nvidia").unwrap();
        assert_eq!(nvidia.category, ProviderCategory::ApiKey);
        assert!(nvidia.free_tier);
        assert_eq!(nvidia.base_url, Some("https://integrate.api.nvidia.com/v1"));

        let hf = descriptor("huggingface").unwrap();
        assert!(hf.free_tier);
        assert_eq!(hf.base_url, Some("https://router.huggingface.co/v1"));

        let cf = descriptor("cloudflare-ai").unwrap();
        assert!(cf.free_tier);
        assert!(
            cf.requires_base_url,
            "user pastes the account-scoped /ai/v1 URL"
        );
        assert!(
            !cf.models.is_empty(),
            "no /models endpoint on the account URL — seeds required"
        );
    }

    #[test]
    fn free_tier_and_risk_notice_flags_mark_the_agreed_entries() {
        // Free-tier: API-key providers with a genuinely free usage path.
        for id in ["openrouter", "groq", "google"] {
            assert!(descriptor(id).unwrap().free_tier, "{id} must be free_tier");
        }
        for id in [
            "anthropic",
            "openai",
            "deepseek",
            "mistral",
            "xai",
            "ollama",
            "anthropic-oauth",
            "openai-oauth",
            "kiro",
            "opencode-free",
            "mimo-free",
        ] {
            assert!(
                !descriptor(id).unwrap().free_tier,
                "{id} must NOT be free_tier"
            );
        }
        // Risk notice: subscription/quota reuse through unofficial endpoints.
        assert!(descriptor("kiro").unwrap().risk_notice);
        assert!(!descriptor("anthropic").unwrap().risk_notice);
        assert!(!descriptor("opencode-free").unwrap().risk_notice);
    }

    #[test]
    fn only_seed_only_providers_lack_a_models_endpoint() {
        assert!(!descriptor("mimo-free").unwrap().has_models_endpoint);
        assert!(!descriptor("cloudflare-ai").unwrap().has_models_endpoint);
        assert!(!descriptor("kiro").unwrap().has_models_endpoint);
        assert!(descriptor("openai").unwrap().has_models_endpoint);
    }

    #[test]
    fn only_openai_requires_max_completion_tokens() {
        // OpenAI's gpt-5.x / o-series rejects `max_tokens` (HTTP 400) and
        // requires `max_completion_tokens`; no other OpenAI-format provider
        // does, so the flag must stay scoped to the first-party entry.
        assert!(descriptor("openai").unwrap().uses_max_completion_tokens);
        for d in CATALOG.iter().filter(|d| d.id != "openai") {
            assert!(
                !d.uses_max_completion_tokens,
                "{} must NOT set uses_max_completion_tokens",
                d.id
            );
        }
    }

    #[test]
    fn kiro_uses_its_seeded_model_list() {
        // Kiro has no OpenAI-compatible /models route — "Refresh models"
        // must report the seeded-list message instead of erroring.
        assert!(!descriptor("kiro").unwrap().has_models_endpoint);
    }

    #[test]
    fn kiro_seeded_model_ids_use_codewhisperer_casing() {
        // CodeWhisperer validates modelId case-sensitively: probing
        // "MiniMax-M2.5" returns 400 INVALID_MODEL_ID while "minimax-m2.5"
        // passes validation (verified live 2026-07-10 against
        // runtime.us-east-1.kiro.dev). Every other seeded id already
        // validates; lock the lowercase form so it can't regress.
        let models = descriptor("kiro").unwrap().models;
        assert!(models.contains(&"minimax-m2.5"));
        assert!(!models.contains(&"MiniMax-M2.5"));
    }

    #[test]
    fn qwen_and_github_copilot_are_device_grant_oauth_providers() {
        let q = descriptor("qwen").expect("qwen present");
        assert_eq!(q.category, ProviderCategory::OAuth);
        assert_eq!(q.family, "qwen");
        assert_eq!(q.base_url, Some("https://portal.qwen.ai/v1"));
        assert!(!q.has_models_endpoint);
        assert!(!q.risk_notice);
        assert!(q.oauth.is_none() && q.device_flow.is_none());
        let qg = device_grant_config("qwen").expect("qwen device grant");
        assert_eq!(qg.client_id, "f0304373b74a44d2b584a3fb70ca9e56");
        assert_eq!(
            qg.device_code_url,
            "https://chat.qwen.ai/api/v1/oauth2/device/code"
        );
        assert!(qg.use_pkce);
        assert!(qg.token_exchange.is_none());
        assert_eq!(qg.refresh_lead_ms, 1_200_000);
        assert!(q.models.contains(&"qwen3-coder-plus"));

        let g = descriptor("github-copilot").expect("github-copilot present");
        assert_eq!(g.category, ProviderCategory::OAuth);
        assert_eq!(g.base_url, Some("https://api.githubcopilot.com"));
        assert!(g.risk_notice);
        assert!(!g.has_models_endpoint);
        let gg = device_grant_config("github-copilot").expect("gh device grant");
        assert_eq!(gg.client_id, "Iv1.b507a08c87ecfe98");
        assert!(!gg.use_pkce);
        assert_eq!(
            gg.token_exchange.as_ref().map(|t| t.url),
            Some("https://api.github.com/copilot_internal/v2/token")
        );
        assert_eq!(gg.refresh_lead_ms, 300_000);
        assert!(g.models.contains(&"gpt-5.2"));
        assert!(!g.models.iter().any(|m| m.contains("embedding")));
    }
}
