//! GitHub Copilot two-leg auth: after the GitHub device grant yields a durable
//! GitHub token, swap it for a short-lived Copilot token used as the Bearer
//! against api.githubcopilot.com. Ported from 9router (MIT, (c) 2024-2026
//! decolua and contributors).
use anyhow::{anyhow, Result};
use serde_json::Value;

/// Short-lived Copilot token. `expires_at_ms` is Unix epoch milliseconds
/// (GitHub returns `expires_at` in seconds).
#[derive(Debug, Clone)]
pub struct CopilotToken {
    pub token: String,
    pub expires_at_ms: i64,
}

/// Why [`exchange_copilot_token`] failed. Callers that refresh an existing
/// connection (`refresh_github_copilot_at`) need to tell these apart: only
/// `Unauthorized` means the durable GitHub token is actually dead and the
/// connection should be marked `needs_relogin`; a `Transient` failure (a
/// network hiccup, a non-2xx that isn't 401/403, or a body that didn't
/// parse) is worth retrying later and must not force a re-login.
#[derive(Debug)]
pub enum CopilotExchangeError {
    /// GitHub rejected the durable token outright (HTTP 401/403) — it's
    /// revoked or invalid, no retry will help.
    Unauthorized,
    /// Anything else: network error, a non-2xx status other than 401/403,
    /// or a response body that failed to parse.
    Transient(anyhow::Error),
}

impl std::fmt::Display for CopilotExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopilotExchangeError::Unauthorized => {
                write!(
                    f,
                    "copilot token exchange unauthorized: github token rejected"
                )
            }
            CopilotExchangeError::Transient(e) => write!(f, "copilot token exchange failed: {e}"),
        }
    }
}

impl std::error::Error for CopilotExchangeError {}

fn parse_copilot_token(json: &Value) -> Result<CopilotToken> {
    let token = json
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("copilot token response missing `token`"))?
        .to_string();
    let expires_at_ms = json
        .get("expires_at")
        .and_then(|v| v.as_i64())
        .map(|s| s * 1000)
        .unwrap_or(0);
    Ok(CopilotToken {
        token,
        expires_at_ms,
    })
}

/// Exchange a GitHub token for a Copilot token. `url` is
/// `copilot_internal/v2/token`. Returns [`CopilotExchangeError::Unauthorized`]
/// on a 401/403 (the GitHub token is dead — genuine re-login needed) and
/// [`CopilotExchangeError::Transient`] for everything else (network error,
/// other non-2xx status, or an unparseable body) so callers can retry later
/// instead of forcing a re-login.
pub async fn exchange_copilot_token(
    http: &reqwest::Client,
    gh_token: &str,
    url: &str,
) -> Result<CopilotToken, CopilotExchangeError> {
    let resp = http
        .get(url)
        .header("authorization", format!("token {gh_token}"))
        .header("user-agent", "GitHubCopilotChat/0.38.0")
        .header("editor-version", "vscode/1.110.0")
        .header("editor-plugin-version", "copilot-chat/0.38.0")
        .header("x-github-api-version", "2025-04-01")
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| CopilotExchangeError::Transient(e.into()))?;
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(CopilotExchangeError::Unauthorized);
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CopilotExchangeError::Transient(anyhow!(
            "copilot token exchange failed ({status}): {text}"
        )));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| CopilotExchangeError::Transient(e.into()))?;
    parse_copilot_token(&json).map_err(CopilotExchangeError::Transient)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_copilot_token_converts_expiry_seconds_to_ms() {
        let json = serde_json::json!({ "token": "cop-tok", "expires_at": 1_900_000_000i64 });
        let t = parse_copilot_token(&json).unwrap();
        assert_eq!(t.token, "cop-tok");
        assert_eq!(t.expires_at_ms, 1_900_000_000_000i64);
    }

    #[test]
    fn parse_copilot_token_errors_without_token() {
        let json = serde_json::json!({ "expires_at": 1_900_000_000i64 });
        assert!(parse_copilot_token(&json).is_err());
    }
}
