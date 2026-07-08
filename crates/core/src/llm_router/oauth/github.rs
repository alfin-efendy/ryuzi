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
/// `copilot_internal/v2/token`.
pub async fn exchange_copilot_token(
    http: &reqwest::Client,
    gh_token: &str,
    url: &str,
) -> Result<CopilotToken> {
    let resp = http
        .get(url)
        .header("authorization", format!("token {gh_token}"))
        .header("user-agent", "GitHubCopilotChat/0.38.0")
        .header("editor-version", "vscode/1.110.0")
        .header("editor-plugin-version", "copilot-chat/0.38.0")
        .header("x-github-api-version", "2025-04-01")
        .header("accept", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("copilot token exchange failed ({status}): {text}"));
    }
    let json: Value = resp.json().await?;
    parse_copilot_token(&json)
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
