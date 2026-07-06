//! Provider model discovery.
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use reqwest::StatusCode;
use serde_json::Value;

use crate::llm_router::connections::{self, ConnectionRow};
use crate::llm_router::registry::{AuthScheme, ProviderDescriptor};
use crate::store::Store;

pub const ANTHROPIC_OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";
pub const CLAUDE_CODE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
pub const CODEX_MODELS_URL: &str =
    "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0";

/// Anthropic OAuth tokens for Claude subscription traffic require the
/// Claude-Code-branded leading system block. Keep this shared so normal chat
/// requests and lightweight model probes satisfy the same upstream contract.
pub fn inject_claude_code_system_prompt(body: &mut Value) {
    let prefix = serde_json::json!({"type": "text", "text": CLAUDE_CODE_SYSTEM_PROMPT});
    let current = body.get("system").cloned().unwrap_or(Value::Null);
    let new_system = match current {
        Value::String(s) => serde_json::json!([prefix, {"type": "text", "text": s}]),
        Value::Array(mut arr) => {
            if arr.first() != Some(&prefix) {
                arr.insert(0, prefix);
            }
            Value::Array(arr)
        }
        _ => serde_json::json!([prefix]),
    };
    body["system"] = new_system;
}

fn raw_models(data: &Value) -> Vec<&Value> {
    if let Value::Array(items) = data {
        return items.iter().collect();
    }
    ["data", "models", "results"]
        .iter()
        .find_map(|k| data.get(*k).and_then(Value::as_array))
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn model_id(item: &Value) -> Option<&str> {
    if let Some(s) = item.as_str() {
        return Some(s);
    }
    ["id", "slug", "model", "name"]
        .iter()
        .find_map(|k| item.get(*k).and_then(Value::as_str))
}

fn is_codex_chat_model(item: &Value, id: &str) -> bool {
    let kind = item
        .get("kind")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("llm")
        .to_ascii_lowercase();
    !matches!(kind.as_str(), "image" | "embedding" | "tts" | "stt")
        && !id.to_ascii_lowercase().contains("embed")
}

pub fn parse_model_ids(provider: &str, data: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();

    for item in raw_models(data) {
        let Some(id) = model_id(item).map(str::trim).filter(|id| !id.is_empty()) else {
            continue;
        };
        if seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
        if provider == "openai-oauth" && is_codex_chat_model(item, id) && !id.ends_with("-review") {
            let review = format!("{id}-review");
            if seen.insert(review.clone()) {
                ids.push(review);
            }
        }
    }

    ids
}

fn strip_generation_path(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    for suffix in ["/chat/completions", "/messages"] {
        if let Some(prefix) = trimmed.strip_suffix(suffix) {
            return prefix.to_string();
        }
    }
    trimmed.to_string()
}

fn model_list_url(desc: &ProviderDescriptor, row: &ConnectionRow) -> Option<String> {
    if row.provider == "openai-oauth" {
        return Some(CODEX_MODELS_URL.to_string());
    }
    let base = connections::effective_base_url(desc, row)?;
    Some(format!("{}/models", strip_generation_path(&base)))
}

fn secret_for_models(row: &ConnectionRow) -> String {
    if connections::is_oauth(row) {
        row.data.access_token.clone().unwrap_or_default()
    } else {
        row.data.api_key.clone().unwrap_or_default()
    }
}

pub(crate) fn chatgpt_account_id(row: &ConnectionRow) -> Option<&str> {
    let data = row.data.provider_specific.as_ref()?;
    data.get("chatgpt_account_id")
        .or_else(|| data.get("chatgptAccountId"))
        .or_else(|| data.get("accountId"))
        .or_else(|| data.get("workspaceId"))
        .and_then(Value::as_str)
}

pub fn build_model_list_request(
    http: &reqwest::Client,
    desc: &ProviderDescriptor,
    row: &ConnectionRow,
) -> Result<reqwest::RequestBuilder> {
    let url = model_list_url(desc, row)
        .ok_or_else(|| anyhow!("connection {} has no models URL", row.id))?;
    let token = secret_for_models(row);
    let mut req = http.get(url).header("content-type", "application/json");

    if row.provider == "openai-oauth" {
        req = req
            .header("accept", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .header("originator", "codex_cli_rs")
            .header("user-agent", "codex_cli_rs/0.136.0");
        if let Some(account_id) = chatgpt_account_id(row) {
            req = req.header("chatgpt-account-id", account_id);
        }
        return Ok(req);
    }

    if row.provider == "anthropic-oauth" {
        return Ok(req
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", ANTHROPIC_OAUTH_BETA)
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
            .header("x-app", "cli"));
    }

    if desc.no_auth {
        if row.provider == "opencode-free" {
            req = req
                .header("authorization", "Bearer public")
                .header("x-opencode-client", "desktop");
        }
        return Ok(req);
    }

    match desc.auth {
        AuthScheme::XApiKey => Ok(req
            .header("x-api-key", token)
            .header("anthropic-version", "2023-06-01")),
        AuthScheme::Bearer => Ok(req.header("authorization", format!("Bearer {token}"))),
        AuthScheme::None => Ok(req),
    }
}

pub async fn fetch_models(
    http: &reqwest::Client,
    desc: &ProviderDescriptor,
    row: &ConnectionRow,
) -> Result<Vec<String>> {
    let (status, models) = fetch_models_once(http, desc, row).await?;
    if !status.is_success() {
        return Err(anyhow!(
            "model list request for {} failed with status {status}",
            row.provider
        ));
    }
    Ok(models)
}

async fn fetch_models_once(
    http: &reqwest::Client,
    desc: &ProviderDescriptor,
    row: &ConnectionRow,
) -> Result<(StatusCode, Vec<String>)> {
    let resp = build_model_list_request(http, desc, row)?.send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Ok((status, Vec::new()));
    }
    let value: Value = resp.json().await?;
    Ok((status, parse_model_ids(&row.provider, &value)))
}

pub async fn fetch_connection_models(
    store: &Arc<Store>,
    http: &reqwest::Client,
    desc: &ProviderDescriptor,
    row: &mut ConnectionRow,
) -> Result<(StatusCode, Vec<String>)> {
    if connections::is_oauth(row) {
        if let Err(err) = crate::llm_router::oauth::refresh::ensure_fresh(store, http, row).await {
            if row.data.needs_relogin == Some(true) {
                return Err(err);
            }
        }
    }

    let (status, models) = fetch_models_once(http, desc, row).await?;
    if connections::is_oauth(row)
        && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && row.data.refresh_token.is_some()
    {
        crate::llm_router::oauth::refresh::force_refresh(store, http, row).await?;
        return fetch_models_once(http, desc, row).await;
    }

    Ok((status, models))
}

pub async fn refresh_connection_models(
    store: &Arc<Store>,
    http: &reqwest::Client,
    row: &mut ConnectionRow,
) -> Result<Vec<String>> {
    let desc = crate::llm_router::registry::descriptor(&row.provider)
        .ok_or_else(|| anyhow!("unknown provider: {}", row.provider))?;
    let (status, models) = fetch_connection_models(store, http, desc, row).await?;
    if !status.is_success() {
        return Err(anyhow!(
            "model list request for {} failed with status {status}",
            row.provider
        ));
    }
    if !models.is_empty() {
        row.data.models_override = Some(models.clone());
        row.updated_at = crate::paths::now_ms();
        connections::update_connection(store, row.clone()).await?;
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use crate::llm_router::registry;

    use super::*;

    fn row(provider: &str, data: ConnectionData) -> ConnectionRow {
        ConnectionRow {
            id: "c1".into(),
            provider: provider.into(),
            auth_type: if provider.ends_with("-oauth") {
                "oauth".into()
            } else {
                "api_key".into()
            },
            label: "test".into(),
            priority: 0,
            enabled: true,
            data,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn parses_openai_and_anthropic_model_list_shapes() {
        let ids = parse_model_ids(
            "openai",
            &json!({"object": "list", "data": [
                {"id": "gpt-a"},
                {"id": "gpt-b"},
                {"id": "gpt-a"}
            ]}),
        );
        assert_eq!(ids, vec!["gpt-a", "gpt-b"]);

        let ids = parse_model_ids(
            "anthropic",
            &json!({"data": [
                {"id": "claude-sonnet-live", "display_name": "Claude Sonnet Live"},
                {"id": "claude-opus-live"}
            ]}),
        );
        assert_eq!(ids, vec!["claude-sonnet-live", "claude-opus-live"]);
    }

    #[test]
    fn parses_codex_models_and_adds_review_variants() {
        let ids = parse_model_ids(
            "openai-oauth",
            &json!({"data": [
                {"id": "gpt-5.4", "name": "GPT 5.4"},
                {"slug": "gpt-5.4-mini", "display_name": "GPT 5.4 Mini"},
                {"id": "gpt-5.4-image", "type": "image"}
            ]}),
        );
        assert_eq!(
            ids,
            vec![
                "gpt-5.4",
                "gpt-5.4-review",
                "gpt-5.4-mini",
                "gpt-5.4-mini-review",
                "gpt-5.4-image"
            ]
        );
    }

    #[test]
    fn injects_claude_code_system_prompt_once() {
        let mut body = json!({"system": [{"type": "text", "text": "custom"}]});
        inject_claude_code_system_prompt(&mut body);
        inject_claude_code_system_prompt(&mut body);

        assert_eq!(body["system"][0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(body["system"][1]["text"], "custom");
        assert_eq!(body["system"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn anthropic_oauth_models_request_uses_bearer_beta_and_models_url() {
        let http = reqwest::Client::new();
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let req = build_model_list_request(
            &http,
            desc,
            &row(
                "anthropic-oauth",
                ConnectionData {
                    access_token: Some("at-claude".into()),
                    ..Default::default()
                },
            ),
        )
        .unwrap()
        .build()
        .unwrap();

        assert_eq!(req.url().as_str(), "https://api.anthropic.com/v1/models");
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-claude"
        );
        assert_eq!(
            req.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        let beta = req
            .headers()
            .get("anthropic-beta")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(beta.contains("claude-code-20250219"));
        assert!(beta.contains("oauth-2025-04-20"));
    }

    #[test]
    fn codex_models_request_uses_backend_endpoint_and_account_header() {
        let http = reqwest::Client::new();
        let desc = registry::descriptor("openai-oauth").unwrap();
        let req = build_model_list_request(
            &http,
            desc,
            &row(
                "openai-oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    provider_specific: Some(json!({"chatgpt_account_id": "acct-1"})),
                    ..Default::default()
                },
            ),
        )
        .unwrap()
        .build()
        .unwrap();

        assert_eq!(
            req.url().as_str(),
            "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-codex"
        );
        assert_eq!(req.headers().get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acct-1");
    }

    #[tokio::test]
    async fn refresh_connection_models_persists_discovered_ids_over_seed_models() {
        use axum::{routing::get, Json, Router};

        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Json(json!({"data": [
                    {"id": "live-model-a"},
                    {"id": "live-model-b"}
                ]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = reqwest::Client::new();
        let mut conn = row(
            "openai",
            ConnectionData {
                api_key: Some("sk-test".into()),
                base_url_override: Some(format!("http://127.0.0.1:{port}/v1")),
                ..Default::default()
            },
        );
        connections::add_connection(&store, conn.clone())
            .await
            .unwrap();

        let models = refresh_connection_models(&store, &http, &mut conn)
            .await
            .unwrap();
        assert_eq!(models, vec!["live-model-a", "live-model-b"]);

        let stored = connections::get_connection(&store, "c1")
            .await
            .unwrap()
            .unwrap();
        let desc = registry::descriptor("openai").unwrap();
        assert_eq!(
            connections::effective_models(desc, &stored),
            vec!["live-model-a".to_string(), "live-model-b".to_string()]
        );
    }
}
