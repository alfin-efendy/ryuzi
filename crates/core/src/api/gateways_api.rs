//! Gateways screen commands. The local host is always first with live
//! telemetry; WSL distros are detected; SSH remotes are persisted config with
//! a TCP reachability probe. Remote execution needs the future daemon — these
//! entries are monitoring/config surfaces, and the UI says so. Moved
//! verbatim (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/gateways_cmd.rs`; that file keeps its own
//! copy until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::gateways::{self, GatewayRow};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "list_gateways",
    "probe_gateways",
    "add_gateway",
    "remove_gateway",
    "update_gateway",
    "gateway_events",
];

#[derive(Deserialize)]
struct AddGatewayP {
    name: String,
    host: String,
    port: u16,
    username: String,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct UpdateGatewayP {
    id: String,
    fs_mode: String,
    paths: Vec<String>,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_gateways" => ok(assemble(cp, false).await?),
        "probe_gateways" => ok(assemble(cp, true).await?),
        "add_gateway" => {
            let a: AddGatewayP = params(p)?;
            ok(add_gateway(state, a.name, a.host, a.port, a.username).await?)
        }
        "remove_gateway" => {
            let a: IdP = params(p)?;
            ok(remove_gateway(state, a.id).await?)
        }
        "update_gateway" => {
            let a: UpdateGatewayP = params(p)?;
            ok(update_gateway(state, a.id, a.fs_mode, a.paths).await?)
        }
        "gateway_events" => {
            let a: IdP = params(p)?;
            let events = gateways::list_events(cp.store(), &a.id, 100).await?;
            ok(events
                .into_iter()
                .map(|e| GatewayEventInfo {
                    at: e.at,
                    level: e.level,
                    text: e.text,
                })
                .collect::<Vec<_>>())
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

fn humanize_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// CPU usage expressed as cores-in-use, plus memory and disk usage as rounded
/// whole percentages. A zero-sized total (a snapshot can fail to size a
/// resource) reports 0% instead of dividing by zero.
fn resource_percentages(
    cpu_pct: u32,
    cores: usize,
    mem_used_gb: f64,
    mem_total_gb: f64,
    disk_used_gb: f64,
    disk_total_gb: f64,
) -> (f64, u32, u32) {
    let used_cores = (cpu_pct as f64 / 100.0) * cores as f64;
    let mem_pct = if mem_total_gb > 0.0 {
        (mem_used_gb / mem_total_gb * 100.0).round() as u32
    } else {
        0
    };
    let disk_pct = if disk_total_gb > 0.0 {
        (disk_used_gb / disk_total_gb * 100.0).round() as u32
    } else {
        0
    };
    (used_cores, mem_pct, disk_pct)
}

async fn ensure_local_row(cp: &ControlPlane) -> anyhow::Result<GatewayRow> {
    if let Some(row) = gateways::get_row(cp.store(), "local").await? {
        return Ok(row);
    }
    let snap = tokio::task::spawn_blocking(gateways::local_snapshot).await?;
    let row = GatewayRow {
        id: "local".into(),
        name: snap.host_name.clone(),
        kind: "local".into(),
        host: None,
        port: None,
        username: None,
        fs_mode: "projects".into(),
        paths: vec![],
    };
    gateways::upsert_row(cp.store(), row.clone()).await?;
    gateways::add_event(cp.store(), "local", "info", "local gateway registered").await?;
    Ok(row)
}

async fn set_last_seen(cp: &ControlPlane, id: &str) {
    let key = format!("gateway_last_seen.{id}");
    let _ = cp
        .store()
        .set_setting(
            crate::domain::WriteOrigin::User,
            &key,
            &crate::paths::now_ms().to_string(),
        )
        .await;
}

async fn last_seen(cp: &ControlPlane, id: &str) -> Option<i64> {
    let key = format!("gateway_last_seen.{id}");
    cp.store()
        .get_setting(&key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

async fn assemble(cp: &ControlPlane, probe: bool) -> anyhow::Result<Vec<GatewayInfo>> {
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let local_row = ensure_local_row(cp).await?;
    let mut out = Vec::new();

    // --- Local host: live telemetry ------------------------------------
    let snap = tokio::task::spawn_blocking(gateways::local_snapshot).await?;
    set_last_seen(cp, "local").await;
    let (used_cores, mem_pct, disk_pct) = resource_percentages(
        snap.cpu_pct,
        snap.cores,
        snap.mem_used_gb,
        snap.mem_total_gb,
        snap.disk_used_gb,
        snap.disk_total_gb,
    );
    let is_windows = cfg!(windows);
    out.push(GatewayInfo {
        id: "local".into(),
        name: if is_windows {
            "This PC".into()
        } else {
            "This Mac".into()
        },
        badge: if is_windows {
            "WIN".into()
        } else {
            "MAC".into()
        },
        kind: "local".into(),
        detail: format!("{} · {}", snap.os_label, snap.host_name),
        meta_line: format!(
            "{} · {} · {} cores · {:.0} GB",
            snap.os_label, snap.arch, snap.cores, snap.mem_total_gb
        ),
        status: "connected".into(),
        latency: Some("0ms".into()),
        daemon_version: format!("v{app_version}"),
        uptime: Some(humanize_uptime(snap.uptime_secs)),
        last_seen_ms: Some(crate::paths::now_ms()),
        resources: vec![
            GatewayResourceInfo {
                label: "CPU".into(),
                sub: format!("{used_cores:.1}/{} cores", snap.cores),
                pct: snap.cpu_pct.min(100),
            },
            GatewayResourceInfo {
                label: "Memory".into(),
                sub: format!("{:.1}/{:.0} GB", snap.mem_used_gb, snap.mem_total_gb),
                pct: mem_pct.min(100),
            },
            GatewayResourceInfo {
                label: "Disk".into(),
                sub: format!("{:.0}/{:.0} GB", snap.disk_used_gb, snap.disk_total_gb),
                pct: disk_pct.min(100),
            },
        ],
        fingerprint: None,
        fs_mode: local_row.fs_mode.clone(),
        paths: local_row.paths.clone(),
    });

    // --- WSL distros: real detection ------------------------------------
    for distro in gateways::list_wsl().await {
        let id = format!("wsl-{}", distro.name.to_lowercase());
        // Persist a row so fs_mode/paths survive; identity refreshes each probe.
        let row = match gateways::get_row(cp.store(), &id).await? {
            Some(r) => r,
            None => {
                let r = GatewayRow {
                    id: id.clone(),
                    name: format!("wsl · {}", distro.name.to_lowercase()),
                    kind: "wsl".into(),
                    host: None,
                    port: None,
                    username: None,
                    fs_mode: "projects".into(),
                    paths: vec![],
                };
                gateways::upsert_row(cp.store(), r.clone()).await?;
                r
            }
        };
        if distro.running {
            set_last_seen(cp, &id).await;
        }
        out.push(GatewayInfo {
            id: id.clone(),
            name: row.name.clone(),
            badge: "WSL".into(),
            kind: "wsl".into(),
            detail: format!("{} · localhost", distro.name),
            meta_line: format!("{} · WSL 2 · shares local hardware", distro.name),
            status: if distro.running {
                "connected"
            } else {
                "offline"
            }
            .into(),
            latency: if distro.running {
                Some("0ms".into())
            } else {
                None
            },
            daemon_version: format!("v{app_version}"),
            uptime: None,
            last_seen_ms: last_seen(cp, &id).await,
            resources: vec![],
            fingerprint: None,
            fs_mode: row.fs_mode,
            paths: row.paths,
        });
    }

    // --- Persisted SSH remotes: TCP probe --------------------------------
    for row in gateways::list_rows(cp.store()).await? {
        if row.kind != "ssh" {
            continue;
        }
        let host = row.host.clone().unwrap_or_default();
        let port = row.port.unwrap_or(22);
        let latency = if probe {
            let l = gateways::probe_tcp(&host, port).await;
            match l {
                Some(ms) => {
                    set_last_seen(cp, &row.id).await;
                    let _ = gateways::add_event(
                        cp.store(),
                        &row.id,
                        "info",
                        &format!("probe ok ({ms}ms)"),
                    )
                    .await;
                }
                None => {
                    let _ = gateways::add_event(
                        cp.store(),
                        &row.id,
                        "error",
                        "probe failed — host unreachable",
                    )
                    .await;
                }
            }
            l
        } else {
            None
        };
        let seen = last_seen(cp, &row.id).await;
        let connected = latency.is_some();
        out.push(GatewayInfo {
            id: row.id.clone(),
            name: row.name.clone(),
            badge: "SSH".into(),
            kind: "ssh".into(),
            detail: format!("ssh · {host}:{port}"),
            meta_line: format!(
                "ssh · {}@{host}:{port} · monitoring only until the remote daemon ships",
                row.username.clone().unwrap_or_else(|| "user".into())
            ),
            status: if connected { "connected" } else { "offline" }.into(),
            latency: latency.map(|ms| format!("{ms}ms")),
            daemon_version: "—".into(),
            uptime: None,
            last_seen_ms: seen,
            resources: vec![],
            fingerprint: None,
            fs_mode: row.fs_mode,
            paths: row.paths,
        });
    }

    Ok(out)
}

async fn add_gateway(
    state: &ApiState,
    name: String,
    host: String,
    port: u16,
    username: String,
) -> Result<Vec<GatewayInfo>, ApiError> {
    let cp = &state.cp;
    let id = format!(
        "ssh-{}",
        crate::paths::new_id().chars().take(8).collect::<String>()
    );
    gateways::upsert_row(
        cp.store(),
        GatewayRow {
            id: id.clone(),
            name,
            kind: "ssh".into(),
            host: Some(host),
            port: Some(port),
            username: Some(username),
            fs_mode: "projects".into(),
            paths: vec![],
        },
    )
    .await?;
    gateways::add_event(cp.store(), &id, "info", "gateway added").await?;
    Ok(assemble(cp, true).await?)
}

async fn remove_gateway(state: &ApiState, id: String) -> Result<Vec<GatewayInfo>, ApiError> {
    let cp = &state.cp;
    if id == "local" {
        return Err(ApiError::bad_request("the local gateway can't be removed"));
    }
    gateways::remove_row(cp.store(), &id).await?;
    Ok(assemble(cp, false).await?)
}

async fn update_gateway(
    state: &ApiState,
    id: String,
    fs_mode: String,
    paths: Vec<String>,
) -> Result<Vec<GatewayInfo>, ApiError> {
    let cp = &state.cp;
    let mut row = gateways::get_row(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown gateway: {id}")))?;
    row.fs_mode = fs_mode;
    row.paths = paths;
    gateways::upsert_row(cp.store(), row).await?;
    Ok(assemble(cp, false).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[test]
    fn uptime_under_an_hour_shows_minutes() {
        assert_eq!(humanize_uptime(59), "0m");
        assert_eq!(humanize_uptime(300), "5m");
        assert_eq!(humanize_uptime(3_599), "59m");
    }

    #[test]
    fn uptime_under_a_day_shows_hours_and_minutes() {
        assert_eq!(humanize_uptime(3_600), "1h 0m");
        assert_eq!(humanize_uptime(5 * 3_600 + 42 * 60), "5h 42m");
    }

    #[test]
    fn uptime_over_a_day_shows_days_and_hours() {
        assert_eq!(humanize_uptime(86_400), "1d 0h");
        assert_eq!(humanize_uptime(2 * 86_400 + 3 * 3_600 + 59 * 60), "2d 3h");
    }

    #[test]
    fn percentages_from_used_and_total() {
        let (cores, mem, disk) = resource_percentages(50, 8, 8.0, 16.0, 100.0, 200.0);
        assert_eq!(cores, 4.0);
        assert_eq!(mem, 50);
        assert_eq!(disk, 50);
    }

    #[test]
    fn percentages_round_to_nearest() {
        let (_, mem, disk) = resource_percentages(0, 4, 1.0, 3.0, 2.0, 3.0);
        assert_eq!(mem, 33);
        assert_eq!(disk, 67);
    }

    #[test]
    fn zero_totals_report_zero_percent() {
        let (cores, mem, disk) = resource_percentages(25, 4, 8.0, 0.0, 100.0, 0.0);
        assert_eq!(cores, 1.0);
        assert_eq!(mem, 0);
        assert_eq!(disk, 0);
    }

    #[tokio::test]
    async fn list_gateways_includes_local_via_rpc() {
        let s = state().await;
        let out = dispatch(&s, "list_gateways", json!({})).await.unwrap();
        assert_eq!(out[0]["id"], "local");
        assert_eq!(out[0]["kind"], "local");
    }
}
