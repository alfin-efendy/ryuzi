//! Endpoint tab commands: server lifecycle, port/autostart settings, keys,
//! and usage series. Moved verbatim (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/endpoint_cmd.rs`; that file keeps its own
//! copy until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::llm_router::keys;
use crate::llm_router::secrets;
use crate::llm_router::server::{RouterServer, DEFAULT_PORT};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "endpoint_status",
    "start_endpoint",
    "stop_endpoint",
    "set_endpoint_config",
    "list_endpoint_keys",
    "create_endpoint_key",
    "revoke_endpoint_key",
    "connection_usage",
    "endpoint_usage",
];

#[derive(Deserialize)]
struct SetEndpointConfigP {
    port: u16,
    autostart: bool,
}
#[derive(Deserialize)]
struct NameP {
    name: String,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct ConnectionUsageP {
    connection_id: String,
    days: i64,
}
#[derive(Deserialize)]
struct DaysP {
    days: i64,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    let srv = &state.router_server;
    match method {
        "endpoint_status" => ok(status_info(cp, srv).await),
        "start_endpoint" => {
            let port = configured_port(cp).await;
            srv.start(port).await?;
            ok(status_info(cp, srv).await)
        }
        "stop_endpoint" => {
            srv.stop().await;
            ok(status_info(cp, srv).await)
        }
        "set_endpoint_config" => {
            let a: SetEndpointConfigP = params(p)?;
            ok(set_endpoint_config(state, a.port, a.autostart).await?)
        }
        "list_endpoint_keys" => ok(keys::list_keys(cp.store())
            .await?
            .into_iter()
            .map(to_key_info)
            .collect::<Vec<_>>()),
        "create_endpoint_key" => {
            let a: NameP = params(p)?;
            keys::create_key(cp.store(), &a.name).await?;
            ok(keys::list_keys(cp.store())
                .await?
                .into_iter()
                .map(to_key_info)
                .collect::<Vec<_>>())
        }
        "revoke_endpoint_key" => {
            let a: IdP = params(p)?;
            keys::revoke_key(cp.store(), &a.id).await?;
            ok(keys::list_keys(cp.store())
                .await?
                .into_iter()
                .map(to_key_info)
                .collect::<Vec<_>>())
        }
        "connection_usage" => {
            let a: ConnectionUsageP = params(p)?;
            ok(connection_usage(cp, a.connection_id, a.days).await?)
        }
        "endpoint_usage" => {
            let a: DaysP = params(p)?;
            ok(endpoint_usage(cp, a.days).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

pub(crate) async fn configured_port(cp: &ControlPlane) -> u16 {
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
        keychain_status: secrets::keychain_status(),
    }
}

/// Persist port + autostart; restart the server when it was running.
async fn set_endpoint_config(
    state: &ApiState,
    port: u16,
    autostart: bool,
) -> Result<EndpointStatusInfo, ApiError> {
    let cp = &state.cp;
    let srv = &state.router_server;
    cp.store()
        .set_setting("endpoint_port", &port.to_string())
        .await?;
    cp.store()
        .set_setting("endpoint_autostart", if autostart { "1" } else { "0" })
        .await?;
    if srv.status().running {
        srv.start(port).await?;
    }
    Ok(status_info(cp, srv).await)
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

/// Inclusive lower bound (UTC calendar day) of a usage window ending at
/// `now_ms`; the window length is clamped to 1..=90 days.
fn since_day_from(now_ms: i64, days: i64) -> String {
    let clamped = days.clamp(1, 90);
    let ms = now_ms - clamped * 24 * 60 * 60 * 1000;
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

fn since_day(days: i64) -> String {
    since_day_from(crate::paths::now_ms(), days)
}

fn today() -> String {
    chrono::DateTime::from_timestamp_millis(crate::paths::now_ms())
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

/// Collapse per-model daily rows into one point per day. Today's totals are
/// filled in by the caller after this returns.
fn to_series(rows: Vec<crate::store::UsageDayRow>) -> UsageSeries {
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

async fn connection_usage(
    cp: &ControlPlane,
    connection_id: String,
    days: i64,
) -> anyhow::Result<UsageSeries> {
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

async fn endpoint_usage(cp: &ControlPlane, days: i64) -> anyhow::Result<UsageSeries> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use crate::store::UsageDayRow;
    use serde_json::json;

    fn row(day: &str, model: &str, requests: i64, input: i64, output: i64) -> UsageDayRow {
        UsageDayRow {
            day: day.into(),
            connection_id: "c1".into(),
            model: model.into(),
            requests,
            input_tokens: input,
            output_tokens: output,
        }
    }

    fn noon_utc() -> i64 {
        chrono::DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn since_day_counts_back_whole_days() {
        assert_eq!(since_day_from(noon_utc(), 7), "2026-06-27");
    }

    #[test]
    fn since_day_clamps_the_window_to_1_through_90() {
        let now = noon_utc();
        assert_eq!(since_day_from(now, 0), since_day_from(now, 1));
        assert_eq!(since_day_from(now, -5), since_day_from(now, 1));
        assert_eq!(since_day_from(now, 1000), since_day_from(now, 90));
        assert_eq!(since_day_from(now, 90), "2026-04-05");
    }

    #[test]
    fn series_folds_same_day_models_into_one_point() {
        let series = to_series(vec![
            row("2026-07-01", "model-a", 2, 10, 20),
            row("2026-07-01", "model-b", 3, 30, 40),
        ]);
        assert_eq!(series.days.len(), 1);
        let p = &series.days[0];
        assert_eq!(p.day, "2026-07-01");
        assert_eq!(p.requests, 5);
        assert_eq!(p.input_tokens, 40);
        assert_eq!(p.output_tokens, 60);
    }

    #[test]
    fn series_orders_days_ascending_and_leaves_today_totals_zero() {
        let series = to_series(vec![
            row("2026-07-02", "model-a", 1, 1, 1),
            row("2026-07-01", "model-a", 1, 1, 1),
        ]);
        let days: Vec<&str> = series.days.iter().map(|p| p.day.as_str()).collect();
        assert_eq!(days, vec!["2026-07-01", "2026-07-02"]);
        assert_eq!(series.today_requests, 0);
        assert_eq!(series.today_input_tokens, 0);
        assert_eq!(series.today_output_tokens, 0);
    }

    #[tokio::test]
    async fn endpoint_keys_crud_via_rpc() {
        let s = state().await;
        let keys = dispatch(&s, "create_endpoint_key", json!({"name": "n1"}))
            .await
            .unwrap();
        assert_eq!(keys.as_array().unwrap().len(), 1);
        let id = keys[0]["id"].as_str().unwrap().to_string();
        let after = dispatch(&s, "revoke_endpoint_key", json!({"id": id}))
            .await
            .unwrap();
        assert_eq!(after, json!([]));
    }

    #[tokio::test]
    async fn endpoint_status_reports_stopped_initially() {
        let s = state().await;
        let st = dispatch(&s, "endpoint_status", json!({})).await.unwrap();
        assert_eq!(st["running"], false);
    }

    #[tokio::test]
    async fn set_endpoint_config_persists_port_and_autostart() {
        let s = state().await;
        let st = dispatch(
            &s,
            "set_endpoint_config",
            json!({"port": 21999, "autostart": true}),
        )
        .await
        .unwrap();
        assert_eq!(st["port"], 21999);
        assert_eq!(st["autostart"], true);
    }

    #[tokio::test]
    async fn endpoint_usage_reports_zero_totals_with_no_data() {
        let s = state().await;
        let out = dispatch(&s, "endpoint_usage", json!({"days": 7}))
            .await
            .unwrap();
        assert_eq!(out["todayRequests"], 0);
        assert_eq!(out["days"], json!([]));
    }
}
