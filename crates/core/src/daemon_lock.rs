//! One-daemon-per-state-dir enforcement. `daemon.json` pid-liveness is only
//! advisory (two daemons started by different paths would both run); this OS
//! file lock is the real mutual exclusion. Held for the process lifetime;
//! the OS releases it even on SIGKILL.

use std::fs::File;
use std::path::Path;

#[derive(Debug)]
pub struct DaemonLock {
    _guard: fd_lock::RwLock<File>,
}

impl DaemonLock {
    /// Take the exclusive daemon lock for `dir` (the state dir holding
    /// `ryuzi.sqlite` / `daemon.json`). Non-blocking: if any other process
    /// holds it, fail immediately with an actionable error.
    pub fn acquire(dir: &Path) -> anyhow::Result<DaemonLock> {
        let path = dir.join("daemon.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        let mut lock = fd_lock::RwLock::new(file);
        // Leak the write guard's lifetime by keeping the RwLock and never
        // unlocking: try_write() to probe, then forget the guard — the fd
        // stays flocked until the process exits or DaemonLock drops.
        //
        // The match is a statement (not the function's tail expression) so
        // its scrutinee's borrow of `lock` ends when the match closes,
        // instead of being extended to the end of the function — that
        // extension is what makes constructing `DaemonLock { _guard: lock }`
        // inside the same match arm rejected by the borrow checker.
        match lock.try_write() {
            Ok(guard) => std::mem::forget(guard),
            Err(_) => anyhow::bail!(
                "another ryuzi daemon is already running (lock: {})",
                path.display()
            ),
        }
        Ok(DaemonLock { _guard: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_in_same_dir_fails_with_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let _held = DaemonLock::acquire(dir.path()).expect("first acquire");
        let err = DaemonLock::acquire(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("another ryuzi daemon is already running"),
            "got: {err}"
        );
    }

    #[test]
    fn lock_is_reacquirable_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        drop(DaemonLock::acquire(dir.path()).unwrap());
        DaemonLock::acquire(dir.path()).expect("reacquire after drop");
    }
}
