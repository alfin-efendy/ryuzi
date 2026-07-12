use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatusFile {
    pub pid: i32,
    pub state: DaemonFileState,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Bound control-API port (None while connecting / for pre-API daemons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The control API's URL scheme: `"http"` (loopback bind) or `"https"`
    /// (non-loopback bind — see `daemon_cmd::start_control_api`, P2-7). None
    /// while connecting / for pre-remote-runner daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// The bound IP the control API is listening on. None while connecting /
    /// for pre-remote-runner daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Base64 SHA-256 pin of the TLS leaf certificate (see
    /// `tls::fingerprint_cert_der`) — only present for `https` (non-loopback)
    /// binds; `None` for `http`/loopback daemons, which have no TLS material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DaemonFileState {
    Connecting,
    Running,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaemonState {
    pub running: bool,
    pub starting: bool,
    pub started_at: Option<i64>,
    pub last_error: Option<String>,
}

pub fn status_path(dir: &Path) -> PathBuf {
    dir.join("daemon.json")
}

pub fn read_status(dir: &Path) -> Option<DaemonStatusFile> {
    let path = status_path(dir);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_status(dir: &Path, s: &DaemonStatusFile) -> std::io::Result<()> {
    let path = status_path(dir);
    let json = serde_json::to_string(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

pub fn clear_status(dir: &Path) {
    let path = status_path(dir);
    let _ = std::fs::remove_file(&path);
}

pub fn is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        pid > 0 && unsafe { libc::kill(pid, 0) } == 0
    }
    #[cfg(windows)]
    {
        // GetExitCodeProcess reports STILL_ACTIVE (259) for live processes.
        // A process that exited with code 259 reads as alive — the standard
        // Win32 caveat, harmless here (worst case: one takeover cycle).
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if pid <= 0 {
            return false;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if handle.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

pub fn send_sigterm(pid: i32) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        // No SIGTERM on Windows; TerminateProcess is the takeover/stop path.
        // The daemon keeps all durable state in SQLite and a stale status
        // file is already handled by `derive_state` on a dead pid.
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        if pid <= 0 {
            return;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

pub fn derive_state(s: Option<&DaemonStatusFile>, alive: &dyn Fn(i32) -> bool) -> DaemonState {
    let Some(s) = s else {
        return DaemonState::default();
    };
    if s.state == DaemonFileState::Error {
        return DaemonState {
            last_error: s.last_error.clone(),
            ..DaemonState::default()
        };
    }
    if s.pid > 0 && alive(s.pid) {
        if s.state == DaemonFileState::Connecting {
            return DaemonState {
                starting: true,
                started_at: Some(s.started_at),
                ..DaemonState::default()
            };
        }
        return DaemonState {
            running: true,
            started_at: Some(s.started_at),
            ..DaemonState::default()
        };
    }
    DaemonState::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_compact_camel_case() {
        let dir = tempfile::tempdir().unwrap();
        let s = DaemonStatusFile {
            pid: 42,
            state: DaemonFileState::Running,
            started_at: 1,
            last_error: None,
            version: Some("0.0.0".into()),
            port: None,
            scheme: None,
            host: None,
            fingerprint: None,
        };
        write_status(dir.path(), &s).unwrap();
        let raw = std::fs::read_to_string(status_path(dir.path())).unwrap();
        assert!(raw.contains("\"startedAt\":1"), "camelCase compact: {raw}");
        assert!(!raw.contains('\n'));
        assert!(!raw.contains("lastError")); // skipped when None
        assert!(!raw.contains("scheme")); // skipped when None
        assert!(!raw.contains("host"));
        assert!(!raw.contains("fingerprint"));
        assert_eq!(read_status(dir.path()), Some(s));
        clear_status(dir.path());
        assert_eq!(read_status(dir.path()), None);
    }

    /// The non-loopback (`https`) case: `scheme`/`host`/`fingerprint` are all
    /// `Some` and round-trip through the compact camelCase JSON.
    #[test]
    fn round_trip_carries_scheme_host_fingerprint_when_https() {
        let dir = tempfile::tempdir().unwrap();
        let s = DaemonStatusFile {
            pid: 42,
            state: DaemonFileState::Running,
            started_at: 1,
            last_error: None,
            version: Some("0.0.0".into()),
            port: Some(4483),
            scheme: Some("https".into()),
            host: Some("0.0.0.0".into()),
            fingerprint: Some("deadbeef==".into()),
        };
        write_status(dir.path(), &s).unwrap();
        let raw = std::fs::read_to_string(status_path(dir.path())).unwrap();
        assert!(raw.contains("\"scheme\":\"https\""), "raw: {raw}");
        assert!(raw.contains("\"host\":\"0.0.0.0\""), "raw: {raw}");
        assert!(raw.contains("\"fingerprint\":\"deadbeef==\""), "raw: {raw}");
        assert_eq!(read_status(dir.path()), Some(s));
    }

    #[test]
    fn old_status_files_without_port_still_parse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            status_path(dir.path()),
            r#"{"pid":42,"state":"running","startedAt":1}"#,
        )
        .unwrap();
        let s = read_status(dir.path()).unwrap();
        assert_eq!(s.port, None);
        assert_eq!(s.pid, 42);
        assert_eq!(s.scheme, None);
        assert_eq!(s.host, None);
        assert_eq!(s.fingerprint, None);
    }

    #[test]
    fn read_status_none_on_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(status_path(dir.path()), "not json").unwrap();
        assert_eq!(read_status(dir.path()), None);
    }

    #[test]
    fn is_alive_self_and_dead() {
        assert!(is_alive(std::process::id() as i32));
        assert!(!is_alive(-999999));
    }

    #[test]
    fn derive_state_table() {
        let alive = |_: i32| true;
        let dead = |_: i32| false;
        let f = |state, pid, last_error: Option<&str>| DaemonStatusFile {
            pid,
            state,
            started_at: 7,
            last_error: last_error.map(String::from),
            version: None,
            port: None,
            scheme: None,
            host: None,
            fingerprint: None,
        };
        assert_eq!(derive_state(None, &alive), DaemonState::default());
        let e = derive_state(Some(&f(DaemonFileState::Error, -1, Some("boom"))), &alive);
        assert!(!e.running && e.last_error.as_deref() == Some("boom"));
        let c = derive_state(Some(&f(DaemonFileState::Connecting, 1, None)), &alive);
        assert!(!c.running && c.starting && c.started_at == Some(7));
        let r = derive_state(Some(&f(DaemonFileState::Running, 1, None)), &alive);
        assert!(r.running && r.started_at == Some(7));
        let d = derive_state(Some(&f(DaemonFileState::Running, 1, None)), &dead);
        assert_eq!(d, DaemonState::default());
    }
}
