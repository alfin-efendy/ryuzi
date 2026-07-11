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
    "save_runner",
    "remove_gateway",
    "update_gateway",
    "gateway_events",
    // Backend-only: see `list_runner_credentials`'s doc comment. Never wrap
    // this in a Cockpit `#[tauri::command]` / `collect_commands!` entry —
    // doing so would hand the decrypted device token to the webview.
    "list_runner_credentials",
];

#[derive(Deserialize)]
struct AddGatewayP {
    name: String,
    host: String,
    port: u16,
    username: String,
}
#[derive(Deserialize)]
struct SaveRunnerP {
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    device_token: String,
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
        "save_runner" => {
            let a: SaveRunnerP = params(p)?;
            ok(save_runner(state, a.name, a.host, a.port, a.fingerprint, a.device_token).await?)
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
        "list_runner_credentials" => ok(list_runner_credentials(cp).await?),
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
        fingerprint: None,
        device_token: None,
    };
    gateways::upsert_row(cp.store(), row.clone()).await?;
    gateways::add_event(cp.store(), "local", "info", "local gateway registered").await?;
    Ok(row)
}

async fn set_last_seen(cp: &ControlPlane, id: &str) {
    let key = format!("gateway_last_seen.{id}");
    let _ = cp
        .store()
        .set_setting(&key, &crate::paths::now_ms().to_string())
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
                    fingerprint: None,
                    device_token: None,
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

    // --- Paired remote runners: TCP probe --------------------------------
    // v1 reachability check. Follow-up: a TLS-handshake probe that also
    // re-verifies the pinned cert fingerprint (not just plain TCP connect).
    for row in gateways::list_rows(cp.store()).await? {
        if row.kind != "remote" {
            continue;
        }
        let host = row.host.clone().unwrap_or_default();
        let port = row.port.unwrap_or(0);
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
                        "probe failed — runner unreachable",
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
            badge: "RMT".into(),
            kind: "remote".into(),
            detail: format!("remote · {host}:{port}"),
            meta_line: format!("remote · {host}:{port} · paired runner"),
            status: if connected { "connected" } else { "offline" }.into(),
            latency: latency.map(|ms| format!("{ms}ms")),
            daemon_version: "—".into(),
            uptime: None,
            last_seen_ms: seen,
            resources: vec![],
            fingerprint: row.fingerprint.clone(),
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
            fingerprint: None,
            device_token: None,
        },
    )
    .await?;
    gateways::add_event(cp.store(), &id, "info", "gateway added").await?;
    Ok(assemble(cp, true).await?)
}

/// Persists a paired remote runner. The pairing HTTP call (`POST /pair`
/// against the host the user typed, over a pinned client) happens in
/// Cockpit's `add_runner` command — core stays free of outbound HTTP to
/// arbitrary hosts. This just stores the result: `device_token` is
/// value-encrypted (recoverable — Cockpit replays it as a bearer on every
/// call to the runner), never hashed.
async fn save_runner(
    state: &ApiState,
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    device_token: String,
) -> Result<Vec<GatewayInfo>, ApiError> {
    let cp = &state.cp;
    let id = format!("remote-{}", crate::paths::new_id());
    gateways::upsert_row(
        cp.store(),
        GatewayRow {
            id: id.clone(),
            name,
            kind: "remote".into(),
            host: Some(host),
            port: Some(port),
            username: None,
            fs_mode: "projects".into(),
            paths: vec![],
            fingerprint: Some(fingerprint),
            device_token: Some(crate::llm_router::secrets::encrypt_field(&device_token)),
        },
    )
    .await?;
    gateways::add_event(cp.store(), &id, "info", "runner paired").await?;
    Ok(assemble(cp, true).await?)
}

/// A paired remote runner row with its device token DECRYPTED. This is the
/// wire shape of the backend-only `list_runner_credentials` RPC method.
///
/// SECURITY: this method exists on the local engine's `/rpc/*` surface,
/// which `crate::serve::require_token`'s two-tier auth accepts a valid
/// DEVICE token for from ANY peer — NOT loopback-only (only the daemon's own
/// `control_token` is loopback-gated). On an engine bound non-loopback (a
/// standalone runner with `listen_addr` set, or a hub's `ryuzi serve`), any
/// device-token holder could otherwise call this and exfiltrate every paired
/// runner's plaintext bearer token. `crate::serve::LOOPBACK_ONLY_METHODS`
/// closes that gap by rejecting this method's name specifically for any
/// non-loopback peer, enforced in `crate::serve::rpc` BEFORE `dispatch` (see
/// `crate::serve::method_allowed_for_peer`) — so the invariant holds
/// regardless of how any given daemon is bound, not just for Cockpit's
/// always-loopback caller. Additionally — unlike every other gateways
/// method — this one is deliberately NOT also wrapped by a Cockpit
/// `#[tauri::command]` in `apps/cockpit/src-tauri/src/gateways_cmd.rs`, nor
/// listed in `lib.rs`'s `collect_commands!`. Tauri only exposes a method to
/// the webview's `invoke()` if it is a `#[tauri::command]` registered in
/// `collect_commands!` — a plain RPC method with neither is simply
/// unreachable from JS. The ONLY caller is
/// `apps/cockpit/src-tauri/src/engine_manager.rs`'s
/// `EngineManager::load_remotes`, which calls it directly via
/// `EngineClient::rpc` from backend Rust code (always over loopback to its
/// own local engine) and uses the decrypted `device_token` solely to build a
/// pinned `EngineClient` (its `Authorization` bearer header) — the token is
/// never placed on an emitted Tauri event or returned by a
/// `#[tauri::command]`. Do not add such a command; that would defeat this
/// invariant.
#[derive(Debug, Clone, serde::Serialize)]
struct RunnerCredentialInfo {
    id: String,
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    device_token: String,
}

async fn list_runner_credentials(cp: &ControlPlane) -> anyhow::Result<Vec<RunnerCredentialInfo>> {
    let mut out = Vec::new();
    for row in gateways::list_rows(cp.store()).await? {
        if row.kind != "remote" {
            continue;
        }
        let (Some(host), Some(port), Some(fingerprint), Some(enc_token)) =
            (row.host, row.port, row.fingerprint, row.device_token)
        else {
            continue;
        };
        // A single corrupt/undecryptable token (tampered row, wrong/rotated
        // keychain key, truncated blob, ...) must not fail the WHOLE call —
        // that would block `EngineManager::load_remotes` from bridging every
        // OTHER, perfectly healthy runner just because one row is bad. Log
        // and skip the offending row instead, same as the missing-field
        // `continue` above.
        let device_token = match crate::llm_router::secrets::decrypt_field(&enc_token) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(
                    runner_id = %row.id,
                    error = %e,
                    "list_runner_credentials: skipping runner with an undecryptable device token"
                );
                continue;
            }
        };
        out.push(RunnerCredentialInfo {
            id: row.id,
            name: row.name,
            host,
            port,
            fingerprint,
            device_token,
        });
    }
    Ok(out)
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

    async fn save_a_runner(s: &crate::serve::ApiState) -> Value {
        dispatch(
            s,
            "save_runner",
            json!({
                "name": "gpu-box",
                "host": "10.0.0.9",
                "port": 7443,
                "fingerprint": "b64ssh256fingerprint==",
                "device_token": "plaintext-device-token"
            }),
        )
        .await
        .unwrap()
    }

    fn find_remote(out: &Value) -> &Value {
        out.as_array()
            .unwrap()
            .iter()
            .find(|g| g["kind"] == "remote")
            .expect("remote row present")
    }

    /// `save_runner` must persist the device token so Cockpit can REPLAY it
    /// as a bearer (encrypted, not hashed). Assert the stored column isn't
    /// the plaintext, and that it round-trips through `decrypt_field` — this
    /// is robust whether the test keychain yields a real `enc:` blob or
    /// (headless CI) a pass-through, since `encrypt_field`/`decrypt_field`
    /// always agree with each other even when the keychain is unavailable.
    #[tokio::test]
    async fn save_runner_persists_encrypted_device_token() {
        let s = state().await;
        let out = save_a_runner(&s).await;
        let id = find_remote(&out)["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("remote-"), "id={id}");

        let row = gateways::get_row(s.cp.store(), &id)
            .await
            .unwrap()
            .expect("row persisted");
        let stored = row.device_token.clone().expect("device_token stored");
        assert_ne!(
            stored, "plaintext-device-token",
            "device_token column must not hold plaintext"
        );
        let decrypted = crate::llm_router::secrets::decrypt_field(&stored).unwrap();
        assert_eq!(decrypted, "plaintext-device-token");
    }

    #[tokio::test]
    async fn assemble_surfaces_remote_row_with_fingerprint_and_badge() {
        let s = state().await;
        save_a_runner(&s).await;
        let out = dispatch(&s, "list_gateways", json!({})).await.unwrap();
        let remote = find_remote(&out);
        assert_eq!(remote["badge"], "RMT");
        assert_eq!(remote["fingerprint"], "b64ssh256fingerprint==");
        assert_eq!(remote["name"], "gpu-box");
    }

    #[tokio::test]
    async fn remove_gateway_removes_remote_row() {
        let s = state().await;
        let out = save_a_runner(&s).await;
        let id = find_remote(&out)["id"].as_str().unwrap().to_string();

        let after = dispatch(&s, "remove_gateway", json!({ "id": id }))
            .await
            .unwrap();
        assert!(
            after
                .as_array()
                .unwrap()
                .iter()
                .all(|g| g["id"] != id.as_str()),
            "remote row must be gone after remove_gateway"
        );
        assert!(gateways::get_row(s.cp.store(), &id)
            .await
            .unwrap()
            .is_none());
    }

    /// `list_runner_credentials` is the ONLY RPC method that returns a
    /// decrypted `device_token` — assert it round-trips to plaintext (so
    /// `EngineManager::load_remotes` can hand it to `new_pinned` as a
    /// bearer) and that `list_gateways`, the method actually reachable
    /// through a Cockpit `#[tauri::command]`, never does.
    #[tokio::test]
    async fn list_runner_credentials_decrypts_the_device_token() {
        let s = state().await;
        save_a_runner(&s).await;

        let creds = dispatch(&s, "list_runner_credentials", json!({}))
            .await
            .unwrap();
        let creds = creds.as_array().unwrap();
        assert_eq!(creds.len(), 1);
        assert_eq!(creds[0]["host"], "10.0.0.9");
        assert_eq!(creds[0]["port"], 7443);
        assert_eq!(creds[0]["fingerprint"], "b64ssh256fingerprint==");
        assert_eq!(creds[0]["device_token"], "plaintext-device-token");

        // `list_gateways` (the method Cockpit's `gateways_cmd::list_gateways`
        // command proxies to the webview) must never carry the token.
        let gw = dispatch(&s, "list_gateways", json!({})).await.unwrap();
        let remote = find_remote(&gw);
        assert!(
            remote.get("device_token").is_none(),
            "list_gateways must not expose device_token: {remote:?}"
        );
    }

    /// A single corrupt/undecryptable `device_token` on one `remote` row
    /// must not fail the whole call — it should be logged and skipped, same
    /// as the existing "missing host/port/fingerprint/token" `continue`
    /// above it in the loop, so one bad row doesn't block every OTHER
    /// runner from loading (see `EngineManager::load_remotes`, the only
    /// caller). `"enc:AAAA"` decodes to a 3-byte blob — always shorter than
    /// the 24-byte nonce `SecretCipher::decrypt` requires — so this fails
    /// deterministically regardless of whether the test keychain yields a
    /// real key or the headless-CI pass-through fallback.
    #[tokio::test]
    async fn list_runner_credentials_skips_a_row_with_a_corrupt_token() {
        let s = state().await;
        let good = save_a_runner(&s).await;
        let good_id = find_remote(&good)["id"].as_str().unwrap().to_string();

        gateways::upsert_row(
            s.cp.store(),
            gateways::GatewayRow {
                id: "remote-corrupt".to_string(),
                name: "bad-box".to_string(),
                kind: "remote".to_string(),
                host: Some("10.0.0.10".to_string()),
                port: Some(7443),
                username: None,
                fs_mode: "projects".to_string(),
                paths: vec![],
                fingerprint: Some("otherfingerprint==".to_string()),
                device_token: Some("enc:AAAA".to_string()),
            },
        )
        .await
        .unwrap();

        let creds = dispatch(&s, "list_runner_credentials", json!({}))
            .await
            .expect("a single corrupt row must not fail the whole call");
        let creds = creds.as_array().unwrap();
        assert_eq!(
            creds.len(),
            1,
            "the corrupt row must be skipped, not returned: {creds:?}"
        );
        assert_eq!(creds[0]["id"], good_id);
        assert_eq!(creds[0]["device_token"], "plaintext-device-token");
    }
}
