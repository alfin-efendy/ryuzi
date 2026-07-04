//! Endpoint tab commands: server lifecycle, port/autostart settings, keys.
use crate::error::CmdError;
use ryuzi_core::llm_router::keys;
use ryuzi_core::llm_router::server::{RouterServer, DEFAULT_PORT};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointStatusInfo {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub autostart: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointKeyInfo {
    pub id: String,
    pub name: String,
    pub key: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

pub async fn configured_port(cp: &ControlPlane) -> u16 {
    cp.store()
        .get_setting("endpoint_port")
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

async fn status_info(cp: &ControlPlane, srv: &RouterServer) -> EndpointStatusInfo {
    let st = srv.status();
    let port = if st.running {
        st.port
    } else {
        configured_port(cp).await
    };
    let autostart = cp
        .store()
        .get_setting("endpoint_autostart")
        .await
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false);
    EndpointStatusInfo {
        running: st.running,
        port,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        autostart,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn endpoint_status(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    Ok(status_info(&cp, &srv).await)
}

#[tauri::command]
#[specta::specta]
pub async fn start_endpoint(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    let port = configured_port(&cp).await;
    srv.start(port).await.map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    Ok(status_info(&cp, &srv).await)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_endpoint(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    srv.stop().await;
    Ok(status_info(&cp, &srv).await)
}

/// Persist port + autostart; restart the server when it was running.
#[tauri::command]
#[specta::specta]
pub async fn set_endpoint_config(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
    port: u16,
    autostart: bool,
) -> R<EndpointStatusInfo> {
    cp.store()
        .set_setting("endpoint_port", &port.to_string())
        .await?;
    cp.store()
        .set_setting("endpoint_autostart", if autostart { "1" } else { "0" })
        .await?;
    if srv.status().running {
        srv.start(port).await.map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    }
    Ok(status_info(&cp, &srv).await)
}

fn to_key_info(k: keys::EndpointKey) -> EndpointKeyInfo {
    EndpointKeyInfo {
        id: k.id,
        name: k.name,
        key: k.key,
        created_at: k.created_at,
        last_used_at: k.last_used_at,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn list_endpoint_keys(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<EndpointKeyInfo>> {
    Ok(keys::list_keys(cp.store())
        .await?
        .into_iter()
        .map(to_key_info)
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn create_endpoint_key(
    cp: State<'_, Arc<ControlPlane>>,
    name: String,
) -> R<Vec<EndpointKeyInfo>> {
    keys::create_key(cp.store(), &name).await?;
    Ok(keys::list_keys(cp.store())
        .await?
        .into_iter()
        .map(to_key_info)
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_endpoint_key(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<EndpointKeyInfo>> {
    keys::revoke_key(cp.store(), &id).await?;
    Ok(keys::list_keys(cp.store())
        .await?
        .into_iter()
        .map(to_key_info)
        .collect())
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub day: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageSeries {
    pub days: Vec<UsagePoint>,
    pub today_requests: i64,
    pub today_input_tokens: i64,
    pub today_output_tokens: i64,
}

fn since_day(days: i64) -> String {
    let clamped = days.clamp(1, 90);
    let ms = ryuzi_core::paths::now_ms() - clamped * 24 * 60 * 60 * 1000;
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

fn today() -> String {
    chrono::DateTime::from_timestamp_millis(ryuzi_core::paths::now_ms())
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

/// Collapse per-model daily rows into one point per day. Today's totals are
/// filled in by the caller after this returns.
fn to_series(rows: Vec<ryuzi_core::store::UsageDayRow>) -> UsageSeries {
    use std::collections::BTreeMap;
    let mut by_day: BTreeMap<String, UsagePoint> = BTreeMap::new();
    for r in rows {
        let e = by_day.entry(r.day.clone()).or_insert(UsagePoint {
            day: r.day.clone(),
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
        });
        e.requests += r.requests;
        e.input_tokens += r.input_tokens;
        e.output_tokens += r.output_tokens;
    }
    UsageSeries {
        days: by_day.into_values().collect(),
        today_requests: 0,
        today_input_tokens: 0,
        today_output_tokens: 0,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn connection_usage(
    cp: State<'_, Arc<ControlPlane>>,
    connection_id: String,
    days: i64,
) -> R<UsageSeries> {
    let rows = cp
        .store()
        .usage_daily(Some(&connection_id), &since_day(days))
        .await?;
    let mut series = to_series(rows);
    let totals = cp.store().today_totals(&today()).await?;
    if let Some(t) = totals.iter().find(|t| t.connection_id == connection_id) {
        series.today_requests = t.requests;
        series.today_input_tokens = t.input_tokens;
        series.today_output_tokens = t.output_tokens;
    }
    Ok(series)
}

#[tauri::command]
#[specta::specta]
pub async fn endpoint_usage(cp: State<'_, Arc<ControlPlane>>, days: i64) -> R<UsageSeries> {
    let rows = cp.store().usage_daily(None, &since_day(days)).await?;
    let mut series = to_series(rows);
    let totals = cp.store().today_totals(&today()).await?;
    for t in totals {
        series.today_requests += t.requests;
        series.today_input_tokens += t.input_tokens;
        series.today_output_tokens += t.output_tokens;
    }
    Ok(series)
}
