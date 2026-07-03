//! Providers screen commands. Configuration (accounts, rotation, failover,
//! user-set limits) persists in SQLite; quota bars track locally-estimated
//! usage against those limits; the usage chart aggregates the messages table.
//! Credentials stay with the agent CLIs (`claude login`) — never stored here.

use crate::error::CmdError;
use ryuzi_core::providers::{self, AccountRow, ProviderRow};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuotaInfo {
    pub label: String,
    pub pct: u32,
    pub used: String,
    pub max: String,
    pub resets: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub email: String,
    pub plan: String,
    pub active: bool,
    pub session_limit_tokens: Option<i64>,
    pub weekly_limit_tokens: Option<i64>,
    pub quotas: Vec<QuotaInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub day: String,
    /// Estimated tokens that day (chars/4 over persisted transcripts).
    pub tok: f64,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub color: String,
    pub initial: String,
    pub kind: String,
    pub enabled: bool,
    pub strategy: String,
    pub fail_auto: bool,
    pub threshold: u32,
    pub return_to_primary: bool,
    pub accounts: Vec<AccountInfo>,
    pub usage: Vec<UsagePoint>,
    /// Whether local usage data is attributable to this provider (the engine
    /// runs Claude today, so only `anthropic` gets the local usage series).
    pub tracks_usage: bool,
}

const SESSION_WINDOW_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn pct_of(used: f64, limit: i64) -> u32 {
    if limit <= 0 {
        return 0;
    }
    ((used / limit as f64) * 100.0).round().min(100.0) as u32
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<ProviderInfo>> {
    providers::seed_defaults(cp.store()).await?;
    let now = ryuzi_core::paths::now_ms();
    let session_used = providers::est_tokens_since(cp.store(), now - SESSION_WINDOW_MS).await?;
    let week_used = providers::est_tokens_since(cp.store(), now - WEEK_WINDOW_MS).await?;
    let usage_days = providers::usage_by_day(cp.store(), 8).await?;

    let mut out = Vec::new();
    for p in providers::list_providers(cp.store()).await? {
        let tracks_usage = p.id == "anthropic";
        let accounts = providers::list_accounts(cp.store(), &p.id).await?;
        let account_infos = accounts
            .into_iter()
            .map(|a| {
                let mut quotas = Vec::new();
                // Usage is attributable to the ACTIVE account only — that's the
                // login the CLI actually uses.
                if tracks_usage && a.active {
                    if let Some(limit) = a.session_limit_tokens {
                        quotas.push(QuotaInfo {
                            label: "Session (5h)".into(),
                            pct: pct_of(session_used, limit),
                            used: format!("≈{}", providers::fmt_tokens(session_used)),
                            max: format!("{} tok", providers::fmt_tokens(limit as f64)),
                            resets: "rolling 5h window".into(),
                        });
                    }
                    if let Some(limit) = a.weekly_limit_tokens {
                        quotas.push(QuotaInfo {
                            label: "Weekly".into(),
                            pct: pct_of(week_used, limit),
                            used: format!("≈{}", providers::fmt_tokens(week_used)),
                            max: format!("{} tok", providers::fmt_tokens(limit as f64)),
                            resets: "rolling 7-day window".into(),
                        });
                    }
                }
                AccountInfo {
                    id: a.id,
                    label: a.label,
                    email: a.email,
                    plan: a.plan,
                    active: a.active,
                    session_limit_tokens: a.session_limit_tokens,
                    weekly_limit_tokens: a.weekly_limit_tokens,
                    quotas,
                }
            })
            .collect();

        let initial = p.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
        out.push(ProviderInfo {
            id: p.id.clone(),
            name: p.name,
            color: p.color,
            initial,
            kind: p.kind,
            enabled: p.enabled,
            strategy: p.strategy,
            fail_auto: p.fail_auto,
            threshold: p.threshold,
            return_to_primary: p.return_to_primary,
            accounts: account_infos,
            usage: if tracks_usage {
                usage_days
                    .iter()
                    .map(|d| UsagePoint {
                        day: d.day.clone(),
                        tok: d.est_tokens,
                    })
                    .collect()
            } else {
                vec![]
            },
            tracks_usage,
        });
    }
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn list_providers(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<ProviderInfo>> {
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn add_provider(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    name: String,
    kind: String,
    color: String,
) -> R<Vec<ProviderInfo>> {
    providers::upsert_provider(
        cp.store(),
        ProviderRow {
            id,
            name,
            kind,
            color,
            enabled: true,
            strategy: "priority".into(),
            fail_auto: false,
            threshold: 95,
            return_to_primary: true,
        },
    )
    .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_provider(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<ProviderInfo>> {
    providers::remove_provider(cp.store(), &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn update_provider(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
    strategy: String,
    fail_auto: bool,
    threshold: u32,
    return_to_primary: bool,
) -> R<Vec<ProviderInfo>> {
    let mut row = providers::get_provider(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown provider: {id}"),
        })?;
    row.enabled = enabled;
    row.strategy = strategy;
    row.fail_auto = fail_auto;
    row.threshold = threshold;
    row.return_to_primary = return_to_primary;
    providers::upsert_provider(cp.store(), row).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn add_provider_account(
    cp: State<'_, Arc<ControlPlane>>,
    provider_id: String,
    label: String,
    email: String,
    plan: String,
    session_limit_tokens: Option<i64>,
    weekly_limit_tokens: Option<i64>,
) -> R<Vec<ProviderInfo>> {
    providers::add_account(
        cp.store(),
        AccountRow {
            id: ryuzi_core::paths::new_id(),
            provider_id,
            label,
            email,
            plan,
            sort: 0,
            active: false,
            session_limit_tokens,
            weekly_limit_tokens,
        },
    )
    .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_provider_account(
    cp: State<'_, Arc<ControlPlane>>,
    account_id: String,
) -> R<Vec<ProviderInfo>> {
    providers::remove_account(cp.store(), &account_id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_active_account(
    cp: State<'_, Arc<ControlPlane>>,
    provider_id: String,
    account_id: String,
) -> R<Vec<ProviderInfo>> {
    providers::set_active_account(cp.store(), &provider_id, &account_id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn move_provider_account(
    cp: State<'_, Arc<ControlPlane>>,
    provider_id: String,
    account_id: String,
    dir: i32,
) -> R<Vec<ProviderInfo>> {
    providers::move_account(cp.store(), &provider_id, &account_id, dir).await?;
    Ok(assemble(&cp).await?)
}
