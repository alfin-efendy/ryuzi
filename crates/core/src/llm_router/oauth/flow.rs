//! OAuth authorize-URL builders + token exchange (per provider), plus the
//! manual-code paste split. Pure request-building + HTTP; no loopback here
//! (see `oauth::listen` in a later task).
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
use anyhow::{bail, Context, Result};
use base64::Engine;

use crate::llm_router::oauth::pkce::Pkce;
use crate::llm_router::oauth::OAuthTokens;
use crate::llm_router::registry::oauth_config;

/// Build the browser-facing authorize URL for `provider`.
///
/// Anthropic and Codex (OpenAI) each expect the OAuth `scope` (and only the
/// scope, here — everything else is either alphanumeric or already
/// URL-safe base64) to be space-joined and percent-encoded as `%20`, not the
/// `application/x-www-form-urlencoded` `+`. We build the query with
/// `url::form_urlencoded` (which gives `+` for spaces) and then flip `+` to
/// `%20` on the whole serialized query string. That flip is only safe
/// because none of the values we put in this query can ever contain a
/// literal `+`: the client ids/scopes are static ASCII, and the PKCE
/// challenge/state are URL-safe-base64 (alphabet `A-Za-z0-9-_`, no `+`/`/`).
pub fn authorize_url(provider: &str, pkce: &Pkce, redirect_uri: &str) -> Result<String> {
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;

    let pairs: Vec<(&str, &str)> = match provider {
        "anthropic-oauth" => vec![
            ("code", "true"),
            ("client_id", cfg.client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", cfg.scope),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &pkce.state),
        ],
        "openai-oauth" => vec![
            ("response_type", "code"),
            ("client_id", cfg.client_id),
            ("redirect_uri", redirect_uri),
            ("scope", cfg.scope),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
            ("state", &pkce.state),
        ],
        _ => bail!("unsupported OAuth provider `{provider}`"),
    };

    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.extend_pairs(pairs);
    let query = ser.finish().replace('+', "%20");

    Ok(format!("{}?{}", cfg.authorize_url, query))
}

/// Exchange an authorization `code` for tokens, against an explicit
/// `token_url` (so tests can point this at a mock server). The public
/// [`exchange_code`] delegates here with the registry's configured URL.
pub async fn exchange_code_at(
    http: &reqwest::Client,
    provider: &str,
    token_url: &str,
    code: &str,
    state: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<OAuthTokens> {
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;

    match provider {
        "anthropic-oauth" => {
            let body = serde_json::json!({
                "code": code,
                "state": state,
                "grant_type": "authorization_code",
                "client_id": cfg.client_id,
                "redirect_uri": redirect_uri,
                "code_verifier": verifier,
            });
            let resp: serde_json::Value = http
                .post(token_url)
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok(tokens_from_response(&resp, None))
        }
        "openai-oauth" => {
            let form = [
                ("grant_type", "authorization_code"),
                ("client_id", cfg.client_id),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ];
            let resp: serde_json::Value = http
                .post(token_url)
                .form(&form)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let provider_specific = resp
                .get("id_token")
                .and_then(|v| v.as_str())
                .and_then(decode_id_token_claims);
            Ok(tokens_from_response(&resp, provider_specific))
        }
        _ => bail!("unsupported OAuth provider `{provider}`"),
    }
}

/// Exchange an authorization `code` for tokens against the provider's
/// registered token endpoint.
pub async fn exchange_code(
    http: &reqwest::Client,
    provider: &str,
    code: &str,
    state: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<OAuthTokens> {
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;
    exchange_code_at(
        http,
        provider,
        cfg.token_url,
        code,
        state,
        redirect_uri,
        verifier,
    )
    .await
}

fn tokens_from_response(
    resp: &serde_json::Value,
    provider_specific: Option<serde_json::Value>,
) -> OAuthTokens {
    let access_token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let refresh_token = resp
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = resp.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
    let expires_at = crate::paths::now_ms() + expires_in * 1000;
    OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
        provider_specific,
    }
}

/// Decode the (unverified — this is a local CLI token exchange, not a
/// security boundary) middle segment of a Codex `id_token` JWT and pull out
/// the `chatgpt_account_id` + plan, wherever the token puts them.
fn decode_id_token_claims(id_token: &str) -> Option<serde_json::Value> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;

    let auth_ns = claims.get("https://api.openai.com/auth");
    let chatgpt_account_id = claims
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            auth_ns
                .and_then(|ns| ns.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
        });
    let plan = claims.get("plan").and_then(|v| v.as_str()).or_else(|| {
        auth_ns
            .and_then(|ns| ns.get("chatgpt_plan_type"))
            .and_then(|v| v.as_str())
    });

    if chatgpt_account_id.is_none() && plan.is_none() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    if let Some(id) = chatgpt_account_id {
        obj.insert(
            "chatgpt_account_id".to_string(),
            serde_json::Value::String(id.to_string()),
        );
    }
    if let Some(p) = plan {
        obj.insert("plan".to_string(), serde_json::Value::String(p.to_string()));
    }
    Some(serde_json::Value::Object(obj))
}

/// Split a manually-pasted Anthropic callback value (`code#state`) into its
/// parts. Anthropic's manual-entry flow concatenates them with `#`.
pub fn split_manual_code(pasted: &str) -> (String, Option<String>) {
    match pasted.split_once('#') {
        Some((code, state)) => (code.to_string(), Some(state.to_string())),
        None => (pasted.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::oauth::pkce;

    #[test]
    fn anthropic_authorize_url_has_code_true_and_pkce() {
        let p = pkce::Pkce {
            verifier: "v".into(),
            challenge: "CH".into(),
            state: "ST".into(),
        };
        let url = authorize_url("anthropic-oauth", &p, "http://127.0.0.1:9/callback").unwrap();
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code=true"));
        assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(url.contains("code_challenge=CH"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=ST"));
        assert!(url.contains("response_type=code"));
        // space-joined scope, url-encoded
        assert!(url.contains("scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference"));
    }

    #[test]
    fn codex_authorize_url_uses_percent20_scope_and_extra_params() {
        let p = pkce::Pkce {
            verifier: "v".into(),
            challenge: "CH".into(),
            state: "ST".into(),
        };
        let url = authorize_url("openai-oauth", &p, "http://localhost:1455/auth/callback").unwrap();
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("originator=codex_cli_rs"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn manual_code_splits_on_hash() {
        assert_eq!(
            split_manual_code("abc#xyz"),
            ("abc".into(), Some("xyz".into()))
        );
        assert_eq!(split_manual_code("abc"), ("abc".into(), None));
    }

    #[tokio::test]
    async fn anthropic_exchange_parses_json_token_response() {
        // mock token server returning Anthropic-style JSON
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/v1/oauth/token",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["grant_type"], "authorization_code");
                assert_eq!(b["code"], "the-code");
                Json(
                    serde_json::json!({"access_token":"at","refresh_token":"rt","expires_in":3600}),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // point the exchange at the mock by overriding token_url via a test hook:
        let http = reqwest::Client::new();
        let toks = exchange_code_at(
            &http,
            "anthropic-oauth",
            &format!("http://127.0.0.1:{port}/v1/oauth/token"),
            "the-code",
            "st",
            "http://127.0.0.1:9/callback",
            "verifier",
        )
        .await
        .unwrap();
        assert_eq!(toks.access_token, "at");
        assert_eq!(toks.refresh_token.as_deref(), Some("rt"));
        assert!(toks.expires_at > crate::paths::now_ms());
    }
}
