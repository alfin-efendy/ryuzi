//! OAuth provider connections: PKCE, authorize-URL + token exchange, loopback
//! callback, and live-traffic token refresh.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! flows from src/lib/oauth/* and open-sse/services/tokenRefresh/*.
pub mod callback;
pub mod device;
pub mod flow;
pub mod import;
pub mod pkce;
pub mod refresh;

/// Tokens returned by a completed OAuth code exchange (or refresh).
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix epoch milliseconds.
    pub expires_at: i64,
    /// Provider-specific extras (e.g. Codex's `chatgpt_account_id` + plan
    /// pulled out of the `id_token` JWT).
    pub provider_specific: Option<serde_json::Value>,
}
