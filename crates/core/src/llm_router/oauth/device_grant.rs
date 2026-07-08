//! RFC 8628 OAuth 2.0 device-authorization grant (+ optional PKCE), used by
//! Qwen Code and GitHub Copilot (leg 1). Distinct from `oauth::device`, which
//! is Kiro's AWS SSO-OIDC flow (dynamic client registration, camelCase JSON).
//! Bodies here are `application/x-www-form-urlencoded` with `Accept: json`.
use anyhow::{anyhow, Result};
use serde_json::Value;

use super::device::DeviceAuth;
use super::pkce::Pkce;
use crate::llm_router::registry::DeviceGrantConfig;
use crate::paths::now_ms;

/// Tokens from a completed device grant. `raw` is the full token JSON so
/// callers can pull provider-specific extras (Qwen's `resource_url`).
#[derive(Debug, Clone)]
pub struct GrantTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    pub raw: Value,
}

/// Result of one poll of the token endpoint (RFC 8628 §3.5).
pub enum GrantPoll {
    Pending,
    SlowDown,
    Ready(GrantTokens),
    Denied,
    Expired,
}

fn device_code_params(cfg: &DeviceGrantConfig, pkce: Option<&Pkce>) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("client_id", cfg.client_id.to_string()),
        ("scope", cfg.scope.to_string()),
    ];
    if let Some(p) = pkce {
        params.push(("code_challenge", p.challenge.clone()));
        params.push(("code_challenge_method", "S256".to_string()));
    }
    params
}

fn token_params(
    cfg: &DeviceGrantConfig,
    device_code: &str,
    pkce: Option<&Pkce>,
) -> Vec<(&'static str, String)> {
    let mut params = vec![
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        ),
        ("client_id", cfg.client_id.to_string()),
        ("device_code", device_code.to_string()),
    ];
    if let Some(p) = pkce {
        params.push(("code_verifier", p.verifier.clone()));
    }
    params
}

fn parse_device_auth(json: &Value) -> Result<DeviceAuth> {
    let get = |k: &str| json.get(k).and_then(|v| v.as_str()).map(String::from);
    let device_code =
        get("device_code").ok_or_else(|| anyhow!("device code response missing `device_code`"))?;
    let user_code =
        get("user_code").ok_or_else(|| anyhow!("device code response missing `user_code`"))?;
    let verification_uri = get("verification_uri")
        .ok_or_else(|| anyhow!("device code response missing `verification_uri`"))?;
    // RFC 8628 makes verification_uri_complete OPTIONAL (GitHub omits it) —
    // fall back to verification_uri so the browser-open + UI still work.
    let verification_uri_complete =
        get("verification_uri_complete").unwrap_or_else(|| verification_uri.clone());
    let expires_in = json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(900);
    let interval = json.get("interval").and_then(|v| v.as_i64()).unwrap_or(5);
    Ok(DeviceAuth {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval,
    })
}

fn classify_poll(json: &Value) -> GrantPoll {
    if let Some(access) = json.get("access_token").and_then(|v| v.as_str()) {
        let expires_in = json.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
        return GrantPoll::Ready(GrantTokens {
            access_token: access.to_string(),
            refresh_token: json
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(String::from),
            expires_at: now_ms() + expires_in * 1000,
            raw: json.clone(),
        });
    }
    match json.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => GrantPoll::Pending,
        Some("slow_down") => GrantPoll::SlowDown,
        Some("access_denied") => GrantPoll::Denied,
        Some("expired_token") => GrantPoll::Expired,
        _ => GrantPoll::Pending, // unknown transient → keep polling until deadline
    }
}

/// POST the device-authorization request; returns the code+URL to show the user.
pub async fn request_device_code(
    http: &reqwest::Client,
    cfg: &DeviceGrantConfig,
    pkce: Option<&Pkce>,
) -> Result<DeviceAuth> {
    let resp = http
        .post(cfg.device_code_url)
        .header("accept", "application/json")
        .form(&device_code_params(cfg, pkce))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("device code request failed ({status}): {text}"));
    }
    let json: Value = resp.json().await?;
    parse_device_auth(&json)
}

/// Poll the token endpoint once. Callers loop on the reported interval.
pub async fn poll_token_once(
    http: &reqwest::Client,
    cfg: &DeviceGrantConfig,
    device_code: &str,
    pkce: Option<&Pkce>,
) -> Result<GrantPoll> {
    let resp = http
        .post(cfg.token_url)
        .header("accept", "application/json")
        .form(&token_params(cfg, device_code, pkce))
        .send()
        .await?;
    let json: Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    Ok(classify_poll(&json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::oauth::pkce;
    use crate::llm_router::registry::{GITHUB_DEVICE_GRANT, QWEN_DEVICE_GRANT};

    #[test]
    fn device_code_params_include_pkce_only_when_configured() {
        let p = pkce::generate();
        let with = device_code_params(&QWEN_DEVICE_GRANT, Some(&p));
        assert!(with.iter().any(|(k, _)| *k == "client_id"));
        assert!(with
            .iter()
            .any(|(k, v)| *k == "code_challenge" && *v == p.challenge));
        assert!(with
            .iter()
            .any(|(k, v)| *k == "code_challenge_method" && v == "S256"));

        let without = device_code_params(&GITHUB_DEVICE_GRANT, None);
        assert!(without
            .iter()
            .any(|(k, v)| *k == "scope" && v == "read:user"));
        assert!(!without.iter().any(|(k, _)| *k == "code_challenge"));
    }

    #[test]
    fn token_params_carry_verifier_only_with_pkce() {
        let p = pkce::generate();
        let with = token_params(&QWEN_DEVICE_GRANT, "DEV123", Some(&p));
        assert!(with.iter().any(
            |(k, v)| *k == "grant_type" && v == "urn:ietf:params:oauth:grant-type:device_code"
        ));
        assert!(with
            .iter()
            .any(|(k, v)| *k == "device_code" && v == "DEV123"));
        assert!(with
            .iter()
            .any(|(k, v)| *k == "code_verifier" && *v == p.verifier));

        let without = token_params(&GITHUB_DEVICE_GRANT, "DEV123", None);
        assert!(!without.iter().any(|(k, _)| *k == "code_verifier"));
    }

    #[test]
    fn parse_device_auth_falls_back_to_verification_uri() {
        // GitHub omits verification_uri_complete.
        let json = serde_json::json!({
            "device_code": "d1", "user_code": "WXYZ-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900, "interval": 5
        });
        let auth = parse_device_auth(&json).unwrap();
        assert_eq!(
            auth.verification_uri_complete,
            "https://github.com/login/device"
        );
        assert_eq!(auth.user_code, "WXYZ-1234");
        assert_eq!(auth.interval, 5);
    }

    #[test]
    fn classify_poll_maps_errors_and_success() {
        assert!(matches!(
            classify_poll(&serde_json::json!({"error": "authorization_pending"})),
            GrantPoll::Pending
        ));
        assert!(matches!(
            classify_poll(&serde_json::json!({"error": "slow_down"})),
            GrantPoll::SlowDown
        ));
        assert!(matches!(
            classify_poll(&serde_json::json!({"error": "access_denied"})),
            GrantPoll::Denied
        ));
        assert!(matches!(
            classify_poll(&serde_json::json!({"error": "expired_token"})),
            GrantPoll::Expired
        ));
        let ok = serde_json::json!({
            "access_token": "at", "refresh_token": "rt",
            "expires_in": 3600, "resource_url": "portal.qwen.ai"
        });
        match classify_poll(&ok) {
            GrantPoll::Ready(t) => {
                assert_eq!(t.access_token, "at");
                assert_eq!(t.refresh_token.as_deref(), Some("rt"));
                assert!(t.expires_at > 0);
                assert_eq!(t.raw.get("resource_url").unwrap(), "portal.qwen.ai");
            }
            _ => panic!("expected Ready"),
        }
    }
}
