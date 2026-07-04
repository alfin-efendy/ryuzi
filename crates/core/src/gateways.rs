//! Gateways domain: the machines Cockpit can run work on. The local host is
//! always present with live sysinfo telemetry; WSL distros are detected from
//! `wsl.exe -l -v`; SSH remotes are persisted config probed over TCP.

use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use std::time::Duration;

/// Persisted gateway configuration row.
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayRow {
    pub id: String,
    pub name: String,
    /// local | wsl | ssh
    pub kind: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub fs_mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GatewayEvent {
    pub at: i64,
    pub level: String,
    pub text: String,
}

/// One-shot local machine telemetry.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSnapshot {
    pub host_name: String,
    pub os_label: String,
    pub arch: String,
    pub cores: usize,
    pub cpu_pct: u32,
    pub mem_used_gb: f64,
    pub mem_total_gb: f64,
    pub disk_used_gb: f64,
    pub disk_total_gb: f64,
    pub uptime_secs: u64,
}

pub fn local_snapshot() -> LocalSnapshot {
    use sysinfo::{Disks, System};
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    sys.refresh_memory();

    let disks = Disks::new_with_refreshed_list();
    // Report the largest disk (the one project data realistically lives on).
    let (total, avail) = disks
        .iter()
        .map(|d| (d.total_space(), d.available_space()))
        .max_by_key(|(t, _)| *t)
        .unwrap_or((0, 0));

    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    LocalSnapshot {
        host_name: System::host_name().unwrap_or_else(|| "local".into()),
        os_label: System::long_os_version()
            .unwrap_or_else(|| System::name().unwrap_or_else(|| "unknown OS".into())),
        arch: System::cpu_arch(),
        cores: sys.cpus().len(),
        cpu_pct: sys.global_cpu_usage().round() as u32,
        mem_used_gb: sys.used_memory() as f64 / GB,
        mem_total_gb: sys.total_memory() as f64 / GB,
        disk_used_gb: (total.saturating_sub(avail)) as f64 / GB,
        disk_total_gb: total as f64 / GB,
        uptime_secs: System::uptime(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WslDistro {
    pub name: String,
    pub running: bool,
}

/// `wsl.exe -l -v` prints UTF-16LE; decode it tolerantly.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    // Heuristic: UTF-16LE output has NUL as every second byte.
    let nul_ratio = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    if bytes.len() >= 4 && nul_ratio * 2 >= bytes.len() / 2 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Parse `wsl -l -v` output (already decoded): skips the header row, strips
/// the `*` default marker, keeps NAME + STATE.
pub fn parse_wsl_list(text: &str) -> Vec<WslDistro> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let line = line.trim_start_matches('*').trim();
            if line.is_empty() {
                return None;
            }
            let mut cols = line.split_whitespace();
            let name = cols.next()?.to_string();
            let state = cols.next().unwrap_or("");
            Some(WslDistro {
                name,
                running: state.eq_ignore_ascii_case("Running"),
            })
        })
        .collect()
}

/// Detect WSL distros (Windows only; empty elsewhere or when WSL is absent).
pub async fn list_wsl() -> Vec<WslDistro> {
    if !cfg!(windows) {
        return vec![];
    }
    let mut cmd = tokio::process::Command::new("wsl.exe");
    cmd.args(["-l", "-v"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    parse_wsl_list(&decode_wsl_output(&out.stdout))
}

/// TCP reachability probe: connect latency in milliseconds, or None.
pub async fn probe_tcp(host: &str, port: u16) -> Option<u32> {
    let started = std::time::Instant::now();
    let fut = tokio::net::TcpStream::connect((host, port));
    match tokio::time::timeout(Duration::from_secs(3), fut).await {
        Ok(Ok(_stream)) => Some(started.elapsed().as_millis() as u32),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<GatewayRow> {
    let paths: String = r.get(7)?;
    Ok(GatewayRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        host: r.get(3)?,
        port: r.get::<_, Option<i64>>(4)?.map(|p| p as u16),
        username: r.get(5)?,
        fs_mode: r.get(6)?,
        paths: serde_json::from_str(&paths).unwrap_or_default(),
    })
}

const GW_COLS: &str = "id,name,kind,host,port,username,fs_mode,paths";

pub async fn list_rows(store: &Store) -> anyhow::Result<Vec<GatewayRow>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {GW_COLS} FROM gateways ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map([], row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn get_row(store: &Store, id: &str) -> anyhow::Result<Option<GatewayRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {GW_COLS} FROM gateways WHERE id=?1"),
                params![id],
                row_from,
            )
            .optional()
        })
        .await
}

pub async fn upsert_row(store: &Store, row: GatewayRow) -> anyhow::Result<()> {
    let paths = serde_json::to_string(&row.paths)?;
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO gateways(id,name,kind,host,port,username,fs_mode,paths,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
                 ON CONFLICT(id) DO UPDATE SET \
                   name=excluded.name, kind=excluded.kind, host=excluded.host, \
                   port=excluded.port, username=excluded.username, \
                   fs_mode=excluded.fs_mode, paths=excluded.paths",
                params![
                    row.id,
                    row.name,
                    row.kind,
                    row.host,
                    row.port.map(|p| p as i64),
                    row.username,
                    row.fs_mode,
                    paths,
                    now
                ],
            )
            .map(|_| ())
        })
        .await
}

pub async fn remove_row(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM gateways WHERE id=?1", params![id])?;
            c.execute(
                "DELETE FROM gateway_events WHERE gateway_id=?1",
                params![id],
            )
            .map(|_| ())
        })
        .await
}

pub async fn add_event(
    store: &Store,
    gateway_id: &str,
    level: &str,
    text: &str,
) -> anyhow::Result<()> {
    let gateway_id = gateway_id.to_string();
    let level = level.to_string();
    let text = text.to_string();
    let at = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO gateway_events(gateway_id, at, level, text) VALUES (?1,?2,?3,?4)",
                params![gateway_id, at, level, text],
            )
            .map(|_| ())
        })
        .await
}

/// Most recent `limit` events, oldest first (log renders top-down).
pub async fn list_events(
    store: &Store,
    gateway_id: &str,
    limit: u32,
) -> anyhow::Result<Vec<GatewayEvent>> {
    let gateway_id = gateway_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT at, level, text FROM ( \
                   SELECT at, level, text FROM gateway_events \
                   WHERE gateway_id=?1 ORDER BY at DESC, id DESC LIMIT ?2 \
                 ) ORDER BY at ASC",
            )?;
            let rows = stmt
                .query_map(params![gateway_id, limit], |r| {
                    Ok(GatewayEvent {
                        at: r.get(0)?,
                        level: r.get(1)?,
                        text: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wsl_list_with_default_marker_and_states() {
        let text = "  NAME            STATE           VERSION\n\
                    * Ubuntu          Running         2\n\
                      Debian          Stopped         2\n";
        let distros = parse_wsl_list(text);
        assert_eq!(distros.len(), 2);
        assert_eq!(
            distros[0],
            WslDistro {
                name: "Ubuntu".into(),
                running: true
            }
        );
        assert_eq!(
            distros[1],
            WslDistro {
                name: "Debian".into(),
                running: false
            }
        );
    }

    #[test]
    fn decodes_utf16le_wsl_output() {
        let text = "  NAME\n* Ubuntu Running 2\n";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_wsl_output(&bytes), text);
        // Plain UTF-8 passes through untouched.
        assert_eq!(decode_wsl_output(text.as_bytes()), text);
    }

    #[tokio::test]
    async fn gateway_rows_and_events_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        upsert_row(
            &store,
            GatewayRow {
                id: "ssh-dev".into(),
                name: "devbox".into(),
                kind: "ssh".into(),
                host: Some("10.0.0.4".into()),
                port: Some(22),
                username: Some("dev".into()),
                fs_mode: "projects".into(),
                paths: vec!["/srv/app".into()],
            },
        )
        .await
        .unwrap();

        let rows = list_rows(&store).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host.as_deref(), Some("10.0.0.4"));
        assert_eq!(rows[0].paths, vec!["/srv/app".to_string()]);

        // fs_mode update via upsert keeps identity fields.
        let mut row = rows[0].clone();
        row.fs_mode = "read".into();
        upsert_row(&store, row).await.unwrap();
        assert_eq!(
            get_row(&store, "ssh-dev").await.unwrap().unwrap().fs_mode,
            "read"
        );

        add_event(&store, "ssh-dev", "info", "probe ok (12ms)")
            .await
            .unwrap();
        add_event(&store, "ssh-dev", "error", "probe failed")
            .await
            .unwrap();
        let events = list_events(&store, "ssh-dev", 10).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "probe ok (12ms)");
        assert_eq!(events[1].level, "error");

        remove_row(&store, "ssh-dev").await.unwrap();
        assert!(list_rows(&store).await.unwrap().is_empty());
        assert!(list_events(&store, "ssh-dev", 10).await.unwrap().is_empty());
    }

    #[test]
    fn local_snapshot_reports_sane_values() {
        let snap = local_snapshot();
        assert!(snap.cores > 0);
        assert!(snap.mem_total_gb > 0.0);
        assert!(!snap.host_name.is_empty());
    }
}
