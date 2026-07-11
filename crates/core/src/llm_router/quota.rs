//! Live provider quota for subscription-backed OAuth connections.
use std::sync::Arc;

use anyhow::{anyhow, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use uuid::Uuid;

use crate::llm_router::connections::{self, ConnectionRow};
use crate::llm_router::models;
use crate::store::Store;

pub const ANTHROPIC_OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub const CODEX_RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
pub const CODEX_RESET_CREDITS_CONSUME_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume";

#[derive(Debug, Serialize, Deserialize, Type, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProviderQuotaCapability {
    Claude,
    Codex,
}

pub fn capability(row: &ConnectionRow) -> Option<ProviderQuotaCapability> {
    if !connections::is_oauth(row) {
        return None;
    }
    match row.provider.as_str() {
        "anthropic-oauth" => Some(ProviderQuotaCapability::Claude),
        "openai-oauth" => Some(ProviderQuotaCapability::Codex),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuotaInfo {
    pub provider: String,
    pub plan: Option<String>,
    pub message: Option<String>,
    pub limit_reached: bool,
    pub review_limit_reached: bool,
    pub reset_credits: Option<CodexResetCreditsInfo>,
    pub quotas: Vec<QuotaWindowInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindowInfo {
    pub label: String,
    pub used: f64,
    pub total: f64,
    pub remaining: f64,
    pub used_percentage: f64,
    pub remaining_percentage: f64,
    pub reset_at: Option<String>,
    pub unlimited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCreditsInfo {
    pub available_count: u32,
    pub credits: Vec<CodexResetCreditInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCreditInfo {
    pub status: String,
    pub granted_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCreditResult {
    pub reset: bool,
    pub code: Option<String>,
    pub windows_reset: u32,
    pub message: Option<String>,
    pub redeem_request_id: Option<String>,
}

impl ProviderQuotaInfo {
    fn empty(provider: &str) -> Self {
        Self {
            provider: provider.to_string(),
            plan: None,
            message: None,
            limit_reached: false,
            review_limit_reached: false,
            reset_credits: None,
            quotas: Vec::new(),
        }
    }
}

fn access_token(row: &ConnectionRow) -> Result<&str> {
    row.data
        .access_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "{} has no OAuth access token; reconnect it first",
                row.label
            )
        })
}

fn to_finite_number(value: Option<&Value>, default: f64) -> f64 {
    let Some(value) = value else {
        return default;
    };
    let parsed = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    parsed.filter(|n| n.is_finite()).unwrap_or(default)
}

fn to_u32(value: Option<&Value>) -> u32 {
    to_finite_number(value, 0.0).max(0.0).floor() as u32
}

fn clamp_percentage(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

fn parse_reset_time(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(n) = trimmed.parse::<f64>() {
                return parse_epoch(n);
            }
            chrono::DateTime::parse_from_rfc3339(trimmed)
                .ok()
                .map(|dt| {
                    dt.to_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                })
                .or_else(|| Some(trimmed.to_string()))
        }
        Value::Number(n) => n.as_f64().and_then(parse_epoch),
        _ => None,
    }
}

fn parse_epoch(value: f64) -> Option<String> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let millis = if value < 1_000_000_000_000.0 {
        (value * 1000.0).round() as i64
    } else {
        value.round() as i64
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn quota_window(label: impl Into<String>, used: f64, reset_at: Option<String>) -> QuotaWindowInfo {
    let used = clamp_percentage(used);
    let remaining = (100.0 - used).max(0.0);
    QuotaWindowInfo {
        label: label.into(),
        used,
        total: 100.0,
        remaining,
        used_percentage: used,
        remaining_percentage: remaining,
        reset_at,
        unlimited: false,
    }
}

fn has_utilization(value: &Value) -> bool {
    value.is_object() && value.get("utilization").is_some()
}

pub(crate) fn parse_anthropic_usage(data: &Value) -> Result<ProviderQuotaInfo> {
    let mut info = ProviderQuotaInfo::empty("anthropic-oauth");
    info.plan = Some("Claude Code".to_string());

    if let Some(window) = data.get("five_hour").filter(|v| has_utilization(v)) {
        info.quotas.push(quota_window(
            "5 hour",
            to_finite_number(window.get("utilization"), 0.0),
            parse_reset_time(window.get("resets_at")),
        ));
    }

    if let Some(window) = data.get("seven_day").filter(|v| has_utilization(v)) {
        info.quotas.push(quota_window(
            "7 day",
            to_finite_number(window.get("utilization"), 0.0),
            parse_reset_time(window.get("resets_at")),
        ));
    }

    if let Some(obj) = data.as_object() {
        let mut keys = obj
            .keys()
            .filter(|key| key.starts_with("seven_day_") && key.as_str() != "seven_day")
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let Some(window) = data.get(&key).filter(|v| has_utilization(v)) else {
                continue;
            };
            let model = key
                .trim_start_matches("seven_day_")
                .replace('_', " ")
                .trim()
                .to_string();
            info.quotas.push(quota_window(
                format!("7 day {model}"),
                to_finite_number(window.get("utilization"), 0.0),
                parse_reset_time(window.get("resets_at")),
            ));
        }
    }

    if info.quotas.is_empty() {
        info.message = Some("Provider quota unavailable".to_string());
    }
    Ok(info)
}

fn codex_rate_limit_body(snapshot: &Value) -> Option<&Value> {
    if !snapshot.is_object() {
        return None;
    }
    snapshot
        .get("rate_limit")
        .filter(|v| v.is_object())
        .or(Some(snapshot))
}

fn codex_window<'a>(snapshot: &'a Value, key_a: &str, key_b: &str) -> Option<&'a Value> {
    let body = codex_rate_limit_body(snapshot)?;
    body.get(key_a)
        .or_else(|| body.get(key_b))
        .or_else(|| snapshot.get(key_a))
        .or_else(|| snapshot.get(key_b))
        .filter(|v| v.is_object())
}

fn format_codex_window(label: &str, window: &Value) -> QuotaWindowInfo {
    quota_window(
        label,
        to_finite_number(
            window
                .get("used_percent")
                .or_else(|| window.get("percent_used")),
            0.0,
        ),
        parse_reset_time(
            window
                .get("reset_at")
                .or_else(|| window.get("resets_at"))
                .or_else(|| window.get("resetAt")),
        ),
    )
}

fn append_codex_quota_windows(quotas: &mut Vec<QuotaWindowInfo>, prefix: &str, snapshot: &Value) {
    let Some(body) = codex_rate_limit_body(snapshot) else {
        return;
    };

    let primary = codex_window(snapshot, "primary_window", "primary");
    let secondary = codex_window(snapshot, "secondary_window", "secondary");

    if let Some(window) = primary {
        let label = if prefix == "review" {
            "Code review primary"
        } else {
            "Codex primary"
        };
        quotas.push(format_codex_window(label, window));
    }
    if let Some(window) = secondary {
        let label = if prefix == "review" {
            "Code review secondary"
        } else {
            "Codex secondary"
        };
        quotas.push(format_codex_window(label, window));
    }

    if primary.is_none()
        && secondary.is_none()
        && (body.get("used_percent").is_some() || body.get("percent_used").is_some())
    {
        let label = if prefix == "review" {
            "Code review"
        } else {
            "Codex"
        };
        quotas.push(format_codex_window(label, body));
    }
}

fn codex_review_rate_limit(data: &Value) -> Option<&Value> {
    data.get("code_review_rate_limit")
        .or_else(|| data.get("review_rate_limit"))
        .or_else(|| {
            let by_limit_id = data.get("rate_limits_by_limit_id")?;
            by_limit_id
                .get("code_review")
                .or_else(|| by_limit_id.get("codex_review"))
                .or_else(|| by_limit_id.get("review"))
        })
        .or_else(|| {
            data.get("additional_rate_limits")
                .and_then(Value::as_array)?
                .iter()
                .find(|entry| {
                    let id = entry
                        .get("limit_name")
                        .or_else(|| entry.get("metered_feature"))
                        .or_else(|| entry.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    id == "code_review"
                        || id == "codex_review"
                        || id == "review"
                        || id.contains("review")
                })
        })
}

pub(crate) fn parse_codex_usage(data: &Value) -> Result<ProviderQuotaInfo> {
    let normal = data
        .get("rate_limit")
        .or_else(|| data.get("rate_limits"))
        .or_else(|| {
            data.get("rate_limits_by_limit_id")
                .and_then(|v| v.get("codex"))
        })
        .unwrap_or(&Value::Null);
    let review = codex_review_rate_limit(data);

    let mut info = ProviderQuotaInfo::empty("openai-oauth");
    info.plan = data
        .get("plan_type")
        .or_else(|| data.get("plan"))
        .or_else(|| data.get("summary").and_then(|v| v.get("plan")))
        .and_then(Value::as_str)
        .map(str::to_string);

    info.limit_reached = codex_rate_limit_body(normal)
        .and_then(|v| v.get("limit_reached"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    info.review_limit_reached = review
        .and_then(codex_rate_limit_body)
        .and_then(|v| v.get("limit_reached"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    append_codex_quota_windows(&mut info.quotas, "", normal);
    if let Some(review) = review {
        append_codex_quota_windows(&mut info.quotas, "review", review);
    }

    info.reset_credits = Some(CodexResetCreditsInfo {
        available_count: to_u32(
            data.get("rate_limit_reset_credits")
                .and_then(|v| v.get("available_count"))
                .or_else(|| {
                    data.get("rate_limit_reset_credits")
                        .and_then(|v| v.get("availableCount"))
                }),
        ),
        credits: Vec::new(),
    });

    if info.quotas.is_empty() {
        info.message = Some("Provider quota unavailable".to_string());
    }
    Ok(info)
}

pub(crate) fn parse_codex_reset_credits(data: &Value) -> CodexResetCreditsInfo {
    let credits = data
        .get("credits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| CodexResetCreditInfo {
                    status: item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    granted_at: parse_reset_time(
                        item.get("granted_at").or_else(|| item.get("grantedAt")),
                    ),
                    expires_at: parse_reset_time(
                        item.get("expires_at").or_else(|| item.get("expiresAt")),
                    ),
                })
                .collect()
        })
        .unwrap_or_default();

    CodexResetCreditsInfo {
        available_count: to_u32(
            data.get("available_count")
                .or_else(|| data.get("availableCount")),
        ),
        credits,
    }
}

pub(crate) fn parse_codex_reset_consume(data: &Value) -> CodexResetCreditResult {
    let code = match data.get("code").and_then(Value::as_str) {
        Some("reset") => Some("reset".to_string()),
        Some("no_credit") => Some("no_credit".to_string()),
        _ => None,
    };
    let windows_reset = to_u32(
        data.get("windows_reset")
            .or_else(|| data.get("windowsReset")),
    );
    let reset = code.as_deref() == Some("reset") || windows_reset > 0;
    let message = if reset {
        "Reset credit applied"
    } else if code.as_deref() == Some("no_credit") {
        "No reset credits available"
    } else {
        "Provider quota unavailable"
    };
    CodexResetCreditResult {
        reset,
        code,
        windows_reset,
        message: Some(message.to_string()),
        redeem_request_id: None,
    }
}

fn anthropic_usage_request(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<reqwest::RequestBuilder> {
    let token = access_token(row)?;
    Ok(http
        .get(ANTHROPIC_OAUTH_USAGE_URL)
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01"))
}

fn codex_usage_request(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<reqwest::RequestBuilder> {
    let token = access_token(row)?;
    let mut req = http
        .get(CODEX_USAGE_URL)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json");
    if let Some(account_id) = models::chatgpt_account_id(row) {
        req = req.header("chatgpt-account-id", account_id);
    }
    Ok(req)
}

fn codex_reset_credits_request(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<reqwest::RequestBuilder> {
    let token = access_token(row)?;
    let mut req = http
        .get(CODEX_RESET_CREDITS_URL)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("openai-beta", "codex-1")
        .header("originator", "codex_cli_rs");
    if let Some(account_id) = models::chatgpt_account_id(row) {
        req = req.header("chatgpt-account-id", account_id);
    }
    Ok(req)
}

fn codex_reset_consume_request(
    http: &reqwest::Client,
    row: &ConnectionRow,
    redeem_request_id: &str,
) -> Result<reqwest::RequestBuilder> {
    let token = access_token(row)?;
    let mut req = http
        .post(CODEX_RESET_CREDITS_CONSUME_URL)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("openai-beta", "codex-1")
        .header("originator", "codex_cli_rs");
    if let Some(account_id) = models::chatgpt_account_id(row) {
        req = req.header("chatgpt-account-id", account_id);
    }
    Ok(req.json(&serde_json::json!({
        "redeem_request_id": redeem_request_id,
    })))
}

async fn response_json(resp: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = resp.status();
    let text = resp.text().await?;
    let data = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    Ok((status, data))
}

fn unavailable_message(provider: &str, status: StatusCode, _data: &Value) -> ProviderQuotaInfo {
    let mut info = ProviderQuotaInfo::empty(provider);
    info.message = Some(
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "Reconnect required",
            StatusCode::TOO_MANY_REQUESTS => "Quota temporarily unavailable",
            _ => "Provider quota unavailable",
        }
        .to_string(),
    );
    info
}

async fn fetch_anthropic_usage_once(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<(StatusCode, ProviderQuotaInfo)> {
    let (status, data) = response_json(anthropic_usage_request(http, row)?.send().await?).await?;
    if !status.is_success() {
        return Ok((
            status,
            unavailable_message("anthropic-oauth", status, &data),
        ));
    }
    Ok((status, parse_anthropic_usage(&data)?))
}

async fn fetch_codex_reset_credits_once(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<(StatusCode, CodexResetCreditsInfo)> {
    let (status, data) =
        response_json(codex_reset_credits_request(http, row)?.send().await?).await?;
    if !status.is_success() {
        return Ok((
            status,
            CodexResetCreditsInfo {
                available_count: 0,
                credits: Vec::new(),
            },
        ));
    }
    Ok((status, parse_codex_reset_credits(&data)))
}

async fn fetch_codex_usage_once(
    http: &reqwest::Client,
    row: &ConnectionRow,
) -> Result<(StatusCode, ProviderQuotaInfo)> {
    let (status, data) = response_json(codex_usage_request(http, row)?.send().await?).await?;
    if !status.is_success() {
        return Ok((status, unavailable_message("openai-oauth", status, &data)));
    }
    let mut info = parse_codex_usage(&data)?;
    if let Ok((reset_status, credits)) = fetch_codex_reset_credits_once(http, row).await {
        if reset_status.is_success() {
            info.reset_credits = Some(credits);
        }
    }
    Ok((status, info))
}

async fn ensure_oauth_fresh(
    store: &Arc<Store>,
    http: &reqwest::Client,
    row: &mut ConnectionRow,
) -> Result<()> {
    if connections::is_oauth(row) {
        if let Err(err) = crate::llm_router::oauth::refresh::ensure_fresh(store, http, row).await {
            if row.data.needs_relogin == Some(true) {
                return Err(err);
            }
        }
    }
    Ok(())
}

pub async fn fetch_provider_quota(
    store: &Arc<Store>,
    http: &reqwest::Client,
    row: &mut ConnectionRow,
) -> Result<ProviderQuotaInfo> {
    if !connections::is_oauth(row) {
        return Err(anyhow!(
            "quota is only available for OAuth subscription connections"
        ));
    }
    ensure_oauth_fresh(store, http, row).await?;

    let (status, info) = match row.provider.as_str() {
        "anthropic-oauth" => fetch_anthropic_usage_once(http, row).await?,
        "openai-oauth" => fetch_codex_usage_once(http, row).await?,
        other => return Err(anyhow!("quota is not supported for provider `{other}`")),
    };

    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && row.data.refresh_token.is_some()
    {
        crate::llm_router::oauth::refresh::force_refresh(store, http, row).await?;
        return Ok(match row.provider.as_str() {
            "anthropic-oauth" => fetch_anthropic_usage_once(http, row).await?.1,
            "openai-oauth" => fetch_codex_usage_once(http, row).await?.1,
            other => return Err(anyhow!("quota is not supported for provider `{other}`")),
        });
    }

    Ok(info)
}

async fn consume_codex_reset_credit_once(
    http: &reqwest::Client,
    row: &ConnectionRow,
    redeem_request_id: &str,
) -> Result<(StatusCode, CodexResetCreditResult)> {
    let (status, data) = response_json(
        codex_reset_consume_request(http, row, redeem_request_id)?
            .send()
            .await?,
    )
    .await?;
    let mut result = parse_codex_reset_consume(&data);
    result.redeem_request_id = Some(redeem_request_id.to_string());
    if !status.is_success() {
        result.reset = false;
    }
    Ok((status, result))
}

fn finish_codex_reset_result(
    status: StatusCode,
    result: CodexResetCreditResult,
) -> Result<CodexResetCreditResult> {
    if status.is_success() || result.code.as_deref() == Some("no_credit") {
        return Ok(result);
    }

    Err(anyhow!("Provider quota unavailable"))
}

pub async fn consume_codex_reset_credit(
    store: &Arc<Store>,
    http: &reqwest::Client,
    row: &mut ConnectionRow,
) -> Result<CodexResetCreditResult> {
    if row.provider != "openai-oauth" || !connections::is_oauth(row) {
        return Err(anyhow!(
            "Codex reset credits are only available for OpenAI ChatGPT subscription connections"
        ));
    }
    ensure_oauth_fresh(store, http, row).await?;

    let redeem_request_id = Uuid::new_v4().to_string();
    let (status, result) = consume_codex_reset_credit_once(http, row, &redeem_request_id).await?;
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && row.data.refresh_token.is_some()
    {
        crate::llm_router::oauth::refresh::force_refresh(store, http, row).await?;
        let (retry_status, retry_result) =
            consume_codex_reset_credit_once(http, row, &redeem_request_id).await?;
        return finish_codex_reset_result(retry_status, retry_result);
    }

    finish_codex_reset_result(status, result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn connection(provider: &str, auth_type: &str) -> ConnectionRow {
        ConnectionRow {
            id: "quota-test".into(),
            provider: provider.into(),
            auth_type: auth_type.into(),
            label: "Quota test".into(),
            priority: 0,
            enabled: true,
            data: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn quota_capability_is_core_owned_and_auth_aware() {
        assert_eq!(
            capability(&connection("anthropic-oauth", "oauth")),
            Some(ProviderQuotaCapability::Claude)
        );
        assert_eq!(
            capability(&connection("openai-oauth", "oauth")),
            Some(ProviderQuotaCapability::Codex)
        );
        assert_eq!(capability(&connection("anthropic-oauth", "api_key")), None);
        assert_eq!(capability(&connection("openai-oauth", "api_key")), None);
        assert_eq!(capability(&connection("anthropic", "oauth")), None);
        assert_eq!(capability(&connection("qwen", "oauth")), None);
    }

    #[test]
    fn unavailable_quota_message_never_copies_provider_body() {
        const SENTINEL: &str = "secret-provider-body-sentinel";
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let info = unavailable_message(
                "openai-oauth",
                status,
                &json!({"message": SENTINEL, "token": SENTINEL}),
            );
            let message = info.message.unwrap();
            assert!(!message.contains(SENTINEL));
            assert!(matches!(
                message.as_str(),
                "Reconnect required"
                    | "Quota temporarily unavailable"
                    | "Provider quota unavailable"
            ));
        }
    }

    #[test]
    fn reset_credit_result_never_copies_provider_message() {
        const SENTINEL: &str = "secret-provider-body-sentinel";
        for payload in [
            json!({"code": "reset", "windows_reset": 1, "message": SENTINEL}),
            json!({"code": "no_credit", "message": SENTINEL}),
            json!({"code": "upstream_error", "error": SENTINEL}),
        ] {
            let result = parse_codex_reset_consume(&payload);
            let message = result.message.unwrap();
            assert!(!message.contains(SENTINEL));
            assert!(matches!(
                message.as_str(),
                "Reset credit applied"
                    | "No reset credits available"
                    | "Provider quota unavailable"
            ));
        }
    }

    #[test]
    fn parses_claude_subscription_usage_windows() {
        let quota = parse_anthropic_usage(&json!({
            "five_hour": {"utilization": 42.5, "resets_at": "2026-07-05T10:00:00Z"},
            "seven_day": {"utilization": 80, "resets_at": "2026-07-11T10:00:00Z"},
            "extra_usage": {"enabled": true}
        }))
        .unwrap();

        assert_eq!(quota.plan.as_deref(), Some("Claude Code"));
        assert_eq!(quota.quotas.len(), 2);
        assert_eq!(quota.quotas[0].label, "5 hour");
        assert_eq!(quota.quotas[0].used_percentage, 42.5);
        assert_eq!(quota.quotas[0].remaining_percentage, 57.5);
        assert_eq!(
            quota.quotas[0].reset_at.as_deref(),
            Some("2026-07-05T10:00:00Z")
        );
        assert_eq!(quota.quotas[1].label, "7 day");
    }

    #[test]
    fn parses_codex_usage_and_reset_credit_count() {
        let quota = parse_codex_usage(&json!({
            "plan": "Plus",
            "rate_limits_by_limit_id": {
                "codex": {
                    "primary_window": {"used_percent": 64, "reset_at": "2026-07-05T11:00:00Z"},
                    "secondary_window": {"percent_used": "12.5", "resets_at": 1783260000000i64}
                },
                "code_review": {"used_percent": 91, "reset_at": "2026-07-05T12:00:00Z", "limit_reached": true}
            },
            "rate_limit_reset_credits": {"available_count": 2}
        }))
        .unwrap();

        assert_eq!(quota.plan.as_deref(), Some("Plus"));
        assert_eq!(quota.reset_credits.as_ref().unwrap().available_count, 2);
        assert_eq!(quota.quotas.len(), 3);
        assert_eq!(quota.quotas[0].label, "Codex primary");
        assert_eq!(quota.quotas[0].used_percentage, 64.0);
        assert_eq!(quota.quotas[1].label, "Codex secondary");
        assert_eq!(quota.quotas[1].used_percentage, 12.5);
        assert_eq!(quota.quotas[2].label, "Code review");
        assert!(quota.review_limit_reached);
    }

    #[test]
    fn parses_codex_reset_credits_detail() {
        let credits = parse_codex_reset_credits(&json!({
            "available_count": 1,
            "credits": [
                {"status": "available", "granted_at": "2026-07-01T00:00:00Z", "expires_at": "2026-08-01T00:00:00Z"}
            ]
        }));

        assert_eq!(credits.available_count, 1);
        assert_eq!(credits.credits[0].status, "available");
        assert_eq!(
            credits.credits[0].expires_at.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
    }

    #[test]
    fn parses_codex_reset_consume_result() {
        let result = parse_codex_reset_consume(&json!({
            "code": "reset",
            "windows_reset": 2
        }));

        assert!(result.reset);
        assert_eq!(result.code.as_deref(), Some("reset"));
        assert_eq!(result.windows_reset, 2);

        let no_credit = parse_codex_reset_consume(&json!({
            "code": "no_credit",
            "message": "No Codex reset credits available."
        }));
        assert!(!no_credit.reset);
        assert_eq!(no_credit.code.as_deref(), Some("no_credit"));
    }

    #[test]
    fn reset_credit_status_finalizer_allows_no_credit_but_rejects_other_failures() {
        let no_credit = parse_codex_reset_consume(&json!({
            "code": "no_credit",
            "message": "No Codex reset credits available."
        }));
        assert!(finish_codex_reset_result(StatusCode::CONFLICT, no_credit).is_ok());

        let failed = parse_codex_reset_consume(&json!({
            "message": "upstream unavailable"
        }));
        let err = finish_codex_reset_result(StatusCode::BAD_GATEWAY, failed).unwrap_err();
        assert_eq!(err.to_string(), "Provider quota unavailable");
    }
}
