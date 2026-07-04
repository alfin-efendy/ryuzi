//! Gateways screen commands. The local host is always first with live
//! telemetry; WSL distros are detected; SSH remotes are persisted config with
//! a TCP reachability probe. Remote execution needs the future daemon — these
//! entries are monitoring/config surfaces, and the UI says so.

use crate::error::CmdError;
use ryuzi_core::gateways::{self, GatewayRow};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResourceInfo {
    pub label: String,
    pub sub: String,
    pub pct: u32,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInfo {
    pub id: String,
    pub name: String,
    pub badge: String,
    /// local | wsl | ssh
    pub kind: String,
    pub detail: String,
    pub meta_line: String,
    /// connected | offline
    pub status: String,
    pub latency: Option<String>,
    pub daemon_version: String,
    pub uptime: Option<String>,
    pub last_seen_ms: Option<i64>,
    pub resources: Vec<GatewayResourceInfo>,
    pub fingerprint: Option<String>,
    pub fs_mode: String,
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEventInfo {
    pub at: i64,
    pub level: String,
    pub text: String,
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
        .set_setting(&key, &ryuzi_core::paths::now_ms().to_string())
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
    let used_cores = (snap.cpu_pct as f64 / 100.0) * snap.cores as f64;
    let mem_pct = if snap.mem_total_gb > 0.0 {
        (snap.mem_used_gb / snap.mem_total_gb * 100.0).round() as u32
    } else {
        0
    };
    let disk_pct = if snap.disk_total_gb > 0.0 {
        (snap.disk_used_gb / snap.disk_total_gb * 100.0).round() as u32
    } else {
        0
    };
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
        last_seen_ms: Some(ryuzi_core::paths::now_ms()),
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

#[tauri::command]
#[specta::specta]
pub async fn list_gateways(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<GatewayInfo>> {
    Ok(assemble(&cp, false).await?)
}

/// Live probe: local telemetry, WSL detection, and SSH TCP reachability.
#[tauri::command]
#[specta::specta]
pub async fn probe_gateways(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<GatewayInfo>> {
    Ok(assemble(&cp, true).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn add_gateway(
    cp: State<'_, Arc<ControlPlane>>,
    name: String,
    host: String,
    port: u16,
    username: String,
) -> R<Vec<GatewayInfo>> {
    let id = format!(
        "ssh-{}",
        ryuzi_core::paths::new_id()
            .chars()
            .take(8)
            .collect::<String>()
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
    Ok(assemble(&cp, true).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_gateway(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<GatewayInfo>> {
    if id == "local" {
        return Err(CmdError {
            message: "the local gateway can't be removed".into(),
        });
    }
    gateways::remove_row(cp.store(), &id).await?;
    Ok(assemble(&cp, false).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_gateway(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    fs_mode: String,
    paths: Vec<String>,
) -> R<Vec<GatewayInfo>> {
    let mut row = gateways::get_row(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown gateway: {id}"),
        })?;
    row.fs_mode = fs_mode;
    row.paths = paths;
    gateways::upsert_row(cp.store(), row).await?;
    Ok(assemble(&cp, false).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn gateway_events(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<GatewayEventInfo>> {
    let events = gateways::list_events(cp.store(), &id, 100).await?;
    Ok(events
        .into_iter()
        .map(|e| GatewayEventInfo {
            at: e.at,
            level: e.level,
            text: e.text,
        })
        .collect())
}
