use std::path::{Path, PathBuf};

use ryuzi_core::daemon_status::{is_alive, read_status, DaemonFileState};

use crate::dispatch::Deps;

pub fn cmd_status(deps: &mut Deps) -> u8 {
    let dir: PathBuf = deps
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    render_status(&dir, &mut deps.out)
}

/// Split from `cmd_status` so tests can point at a temp dir directly.
fn render_status(dir: &Path, out: &mut Box<dyn FnMut(&str)>) -> u8 {
    let Some(s) = read_status(dir) else {
        (out)("daemon: stopped (no status file)");
        return 1;
    };
    let alive = is_alive(s.pid);
    let state = match (&s.state, alive) {
        (DaemonFileState::Running, true) => "running",
        (DaemonFileState::Running, false) => "stopped (stale status file — pid is dead)",
        (DaemonFileState::Connecting, true) => "connecting",
        (DaemonFileState::Connecting, false) => "stopped (died while connecting)",
        (DaemonFileState::Error, _) => "error",
    };
    (out)(&format!("daemon: {state}"));
    (out)(&format!("  pid:     {}", s.pid));
    if let Some(port) = s.port {
        (out)(&format!("  port:    {port}"));
    }
    if let Some(v) = &s.version {
        (out)(&format!("  version: {v}"));
    }
    if let Some(e) = &s.last_error {
        (out)(&format!("  error:   {e}"));
    }
    if matches!(s.state, DaemonFileState::Running) && alive {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::daemon_status::{write_status, DaemonStatusFile};

    #[allow(clippy::type_complexity)]
    fn capture() -> (
        Box<dyn FnMut(&str)>,
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    ) {
        let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = out.clone();
        (
            Box::new(move |s: &str| sink.borrow_mut().push(s.to_string())),
            out,
        )
    }

    #[test]
    fn status_reports_stopped_without_a_status_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut out, lines) = capture();
        assert_eq!(render_status(tmp.path(), &mut out), 1);
        assert_eq!(lines.borrow()[0], "daemon: stopped (no status file)");
    }

    #[test]
    fn status_reports_running_for_a_live_pid() {
        let tmp = tempfile::tempdir().unwrap();
        write_status(
            tmp.path(),
            &DaemonStatusFile {
                pid: std::process::id() as i32, // this test process: definitely alive
                state: DaemonFileState::Running,
                started_at: 1,
                last_error: None,
                version: Some("0.6.0".into()),
                port: Some(4483),
                scheme: None,
                host: None,
                fingerprint: None,
            },
        )
        .unwrap();
        let (mut out, lines) = capture();
        assert_eq!(render_status(tmp.path(), &mut out), 0);
        let l = lines.borrow();
        assert_eq!(l[0], "daemon: running");
        assert!(l.iter().any(|s| s.contains("port:    4483")));
        assert!(l.iter().any(|s| s.contains("version: 0.6.0")));
    }

    #[test]
    fn status_flags_a_stale_running_file_with_a_dead_pid() {
        let tmp = tempfile::tempdir().unwrap();
        write_status(
            tmp.path(),
            &DaemonStatusFile {
                pid: -1, // is_alive(-1) is false on every platform
                state: DaemonFileState::Running,
                started_at: 1,
                last_error: None,
                version: None,
                port: None,
                scheme: None,
                host: None,
                fingerprint: None,
            },
        )
        .unwrap();
        let (mut out, lines) = capture();
        assert_eq!(render_status(tmp.path(), &mut out), 1);
        assert!(lines.borrow()[0].contains("stale status file"));
    }
}
