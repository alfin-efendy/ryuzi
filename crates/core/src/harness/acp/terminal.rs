//! Client-side ACP terminal handler: sandboxed `terminal/*` implementations.
//!
//! The ACP protocol allows the agent to request terminal sessions from the
//! client. These handlers enforce that all working directories are confined to
//! the session's `work_dir` (the session worktree).
//!
//! # Architecture
//! [`TerminalManager`] owns a map of `TerminalId → TerminalHandle`. Each handle
//! holds:
//! - The PTY master reader (wrapped behind a `Mutex<dyn Read>`)
//! - A byte-capped ring buffer accumulating output
//! - A background OS thread reading from the PTY and flushing into the buffer
//! - The child process handle (for `kill`)
//! - A once-cell for the exit status
//!
//! PTY I/O is blocking, so we read on a dedicated OS thread (`std::thread::spawn`)
//! and funnel bytes into the shared buffer under a `Mutex`. `wait_for_exit` blocks
//! an `async fn` via `tokio::sync::Notify` so the async executor is never stalled.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem as _};
use tokio::sync::Notify;

use agent_client_protocol_schema::v1::{TerminalExitStatus, TerminalId};

// ---------------------------------------------------------------------------
// Per-terminal state
// ---------------------------------------------------------------------------

/// Byte-capped ring buffer: keeps only the **last** `cap` bytes.
struct RingBuffer {
    /// The accumulated bytes (valid UTF-8 best-effort).
    buf: Vec<u8>,
    /// Maximum number of bytes to retain.
    cap: usize,
    /// Whether we have ever truncated.
    truncated: bool,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        let cap = cap.max(1); // avoid 0-cap edge cases
        Self {
            buf: Vec::with_capacity(cap.min(64 * 1024)),
            cap,
            truncated: false,
        }
    }

    /// Append bytes, discarding the oldest data when the cap would be exceeded.
    fn push(&mut self, data: &[u8]) {
        let combined_len = self.buf.len() + data.len();
        if combined_len <= self.cap {
            self.buf.extend_from_slice(data);
        } else {
            // We need to keep only the last `cap` bytes.
            self.truncated = true;
            // How many bytes from `data` can we keep?
            let keep_from_data = data.len().min(self.cap);
            let skip_from_data = data.len() - keep_from_data;
            // How many bytes from existing buf can we keep?
            let keep_from_buf = self.cap - keep_from_data;
            let skip_from_buf = self.buf.len().saturating_sub(keep_from_buf);
            self.buf.drain(..skip_from_buf);
            self.buf.extend_from_slice(&data[skip_from_data..]);
        }
        debug_assert!(self.buf.len() <= self.cap, "ring buffer overflow");
    }

    /// Return the current content as a lossy UTF-8 string.
    fn as_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// Result of polling / waiting for a terminal's exit.
#[derive(Clone, Debug)]
pub struct TerminalOutput {
    /// Accumulated output (last `output_byte_limit` bytes).
    pub output: String,
    /// Whether the output was truncated due to the byte limit.
    pub truncated: bool,
    /// Exit status, once the child has exited.
    pub exit_status: Option<TerminalExitStatus>,
}

/// All mutable runtime state for one terminal, protected by a single Mutex.
struct TerminalState {
    ring: RingBuffer,
    /// The exit code once the process has exited.
    exit_code: Option<u32>,
    /// `true` once the reader thread has observed the child exit.
    exited: bool,
}

struct TerminalHandle {
    /// Shared mutable state (output buffer + exit status).
    state: Arc<Mutex<TerminalState>>,
    /// Notified when the process exits (reader thread fires this).
    exit_notify: Arc<Notify>,
    /// PTY master — kept alive so the slave side's fd stays open. `Option` so
    /// `release`/`release_all` can drop (close) it to unblock the drain reader
    /// before joining the drain thread.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    /// Child process handle — used for `kill`.
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    /// Join handle for the background drain thread. `Option` so it can be taken
    /// and `join()`ed on release. Detaching (dropping without joining) would
    /// leak the thread past session end.
    drain_thread: Option<std::thread::JoinHandle<()>>,
}

// TerminalId wraps Arc<str> with no AsRef<str> impl; access the inner field.
fn terminal_key(id: &TerminalId) -> &str {
    id.0.as_ref()
}

// ---------------------------------------------------------------------------
// TerminalManager
// ---------------------------------------------------------------------------

/// Manages multiple PTY-backed terminals for a single ACP session.
///
/// One `TerminalManager` is created per ACP session and dropped (after
/// calling `release_all`) when the session ends.
pub struct TerminalManager {
    terminals: Mutex<HashMap<String, TerminalHandle>>,
}

impl TerminalManager {
    /// Create a new, empty `TerminalManager`.
    pub fn new() -> Self {
        Self {
            terminals: Mutex::new(HashMap::new()),
        }
    }

    /// Open a PTY, spawn `command` (via `sh -c`) in `cwd`, and start draining
    /// output into a byte-capped ring buffer.
    ///
    /// `cwd` must already be sandboxed to the session worktree by the caller
    /// (via [`super::fs::sandbox`]).
    ///
    /// Returns the new `TerminalId`.
    pub fn create(
        &self,
        command: &str,
        cwd: PathBuf,
        output_byte_limit: u64,
    ) -> anyhow::Result<TerminalId> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("portable-pty: openpty failed")?;

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", command]);
        cmd.cwd(&cwd);

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("portable-pty: spawn_command failed")?;
        // slave is no longer needed after spawning; drop it so the master
        // reader sees EOF when the child exits.
        drop(pair.slave);

        let child = Arc::new(Mutex::new(child));

        let state = Arc::new(Mutex::new(TerminalState {
            ring: RingBuffer::new(output_byte_limit as usize),
            exit_code: None,
            exited: false,
        }));
        let exit_notify = Arc::new(Notify::new());

        // Spawn a background OS thread to drain the PTY master reader.
        let reader = pair
            .master
            .try_clone_reader()
            .context("portable-pty: try_clone_reader failed")?;
        let drain_thread = {
            let state_for_thread = state.clone();
            let notify_for_thread = exit_notify.clone();
            let child_for_thread = child.clone();
            std::thread::spawn(move || {
                drain_pty(reader, state_for_thread, notify_for_thread, child_for_thread);
            })
        };

        let id = uuid::Uuid::new_v4().to_string();
        let terminal_id = TerminalId::new(id.clone());

        let handle = TerminalHandle {
            state,
            exit_notify,
            master: Some(pair.master),
            child,
            drain_thread: Some(drain_thread),
        };

        self.terminals.lock().unwrap().insert(id, handle);

        Ok(terminal_id)
    }

    /// Return the current output and exit status for terminal `id`.
    pub fn output(&self, id: &TerminalId) -> anyhow::Result<TerminalOutput> {
        let terminals = self.terminals.lock().unwrap();
        let handle = terminals
            .get(terminal_key(id))
            .ok_or_else(|| anyhow::anyhow!("terminal not found: {}", terminal_key(id)))?;
        let state = handle.state.lock().unwrap();
        Ok(TerminalOutput {
            output: state.ring.as_string_lossy(),
            truncated: state.ring.is_truncated(),
            exit_status: state
                .exit_code
                .map(|code| TerminalExitStatus::new().exit_code(code)),
        })
    }

    /// Wait (asynchronously) until the terminal process exits.
    ///
    /// Race-free against the drain thread's `notify_waiters()`: `tokio::Notify`
    /// stores no permit, so `notify_waiters()` only wakes waiters already
    /// registered. We therefore register (`enable()`) the `notified()` future
    /// BEFORE re-checking `exited`; any `notify_waiters()` firing after that
    /// recheck is guaranteed to be observed. The loop guards against spurious /
    /// unrelated wakeups by re-checking the flag each iteration.
    pub async fn wait_for_exit(&self, id: &TerminalId) -> anyhow::Result<()> {
        // Clone the shared state + notify without holding the manager lock
        // across the await.
        let (state, notify) = {
            let terminals = self.terminals.lock().unwrap();
            let handle = terminals
                .get(terminal_key(id))
                .ok_or_else(|| anyhow::anyhow!("terminal not found: {}", terminal_key(id)))?;
            (handle.state.clone(), handle.exit_notify.clone())
        };
        loop {
            // Create + register the waiter BEFORE checking the flag so a
            // notify_waiters() that fires after the check cannot be missed.
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable(); // registers this waiter now
            if state.lock().unwrap().exited {
                return Ok(());
            }
            notified.await; // any notify_waiters() after enable() is observed
            // Loop re-checks `exited` to guard against spurious wakeups.
        }
    }

    /// Kill the terminal's child process (SIGKILL). The terminal id remains
    /// valid for `output()` afterwards.
    pub fn kill(&self, id: &TerminalId) -> anyhow::Result<()> {
        let terminals = self.terminals.lock().unwrap();
        let handle = terminals
            .get(terminal_key(id))
            .ok_or_else(|| anyhow::anyhow!("terminal not found: {}", terminal_key(id)))?;
        handle
            .child
            .lock()
            .unwrap()
            .kill()
            .context("portable-pty: kill failed")?;
        Ok(())
    }

    /// Release the terminal, freeing all associated resources.
    ///
    /// Ordering matters (see [`shutdown_handle`]): kill the child, close the
    /// PTY master (so the reader gets EOF/EIO), then join the drain thread.
    pub fn release(&self, id: &TerminalId) -> anyhow::Result<()> {
        // Remove the handle under the manager lock, then shut it down WITHOUT
        // holding the lock (joining the thread could otherwise block others).
        let removed = self.terminals.lock().unwrap().remove(terminal_key(id));
        match removed {
            Some(handle) => {
                shutdown_handle(handle);
                Ok(())
            }
            None => anyhow::bail!("terminal not found: {}", terminal_key(id)),
        }
    }

    /// Release all terminals (called at session end / cancel).
    pub fn release_all(&self) {
        // Drain the map under the lock, then shut each handle down outside it.
        let handles: Vec<TerminalHandle> = {
            let mut terminals = self.terminals.lock().unwrap();
            terminals.drain().map(|(_, handle)| handle).collect()
        };
        for handle in handles {
            shutdown_handle(handle);
        }
    }
}

/// Tear down one terminal handle: kill the child (best-effort, so a still-running
/// process is never orphaned), close the PTY master so the drain reader observes
/// EOF/EIO, then join the drain thread. This ordering avoids a join-hang: the
/// drain thread only exits once the child is dead AND the master is closed.
fn shutdown_handle(mut handle: TerminalHandle) {
    // (a) Kill the child (best-effort; it may have already exited).
    let _ = handle.child.lock().unwrap().kill();
    // (b) Close the PTY master so the reader gets EOF/EIO and drain_pty returns.
    drop(handle.master.take());
    // (c) Join the drain thread; after kill+master-close it returns promptly.
    if let Some(join) = handle.drain_thread.take() {
        let _ = join.join();
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PTY drain loop (runs on a dedicated OS thread)
// ---------------------------------------------------------------------------

fn drain_pty(
    mut reader: Box<dyn Read + Send>,
    state: Arc<Mutex<TerminalState>>,
    notify: Arc<Notify>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                state.lock().unwrap().ring.push(&buf[..n]);
            }
        }
    }
    // PTY EOF — the child has exited. Collect exit code.
    // portable_pty::ExitStatus::exit_code() returns u32 (always present).
    let exit_code = child
        .lock()
        .unwrap()
        .wait()
        .ok()
        .map(|status| status.exit_code());
    {
        let mut st = state.lock().unwrap();
        st.exit_code = exit_code;
        st.exited = true;
    }
    notify.notify_waiters();
}

// ---------------------------------------------------------------------------
// Sandbox helper re-export (used by the handler in mod.rs)
// ---------------------------------------------------------------------------

/// Sandbox `cwd` to `work_dir`. Thin wrapper around `fs::sandbox` that
/// falls back to `work_dir` when `cwd` is `None`.
pub fn sandbox_cwd(work_dir: &Path, cwd: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match cwd {
        Some(p) => crate::harness::acp::fs::sandbox(work_dir, &p),
        None => Ok(work_dir.to_path_buf()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_runs_a_command_and_captures_output_within_byte_limit() {
        let root = tempfile::tempdir().unwrap();
        let mgr = TerminalManager::new();
        let id = mgr
            .create("echo hello", root.path().to_path_buf(), 1024)
            .unwrap();
        mgr.wait_for_exit(&id).await.unwrap();
        let out = mgr.output(&id).unwrap();
        assert!(
            out.output.contains("hello"),
            "expected 'hello' in output, got: {:?}",
            out.output
        );
        assert_eq!(
            out.exit_status.and_then(|s| s.exit_code),
            Some(0),
            "expected exit code 0"
        );
        mgr.release(&id).unwrap();
    }

    #[tokio::test]
    async fn terminal_byte_limit_truncates_old_output() {
        let root = tempfile::tempdir().unwrap();
        let mgr = TerminalManager::new();
        // 5-byte limit: 10 A chars will be truncated to 5
        let id = mgr
            .create("printf 'AAAAAAAAAA'", root.path().to_path_buf(), 5)
            .unwrap();
        mgr.wait_for_exit(&id).await.unwrap();
        let out = mgr.output(&id).unwrap();
        assert!(
            out.output.len() <= 5,
            "output should be capped at 5 bytes, got {} bytes: {:?}",
            out.output.len(),
            out.output
        );
        assert!(out.truncated, "truncated flag should be set");
        mgr.release(&id).unwrap();
    }

    #[tokio::test]
    async fn terminal_runs_in_cwd() {
        let root = tempfile::tempdir().unwrap();
        let mgr = TerminalManager::new();
        let id = mgr
            .create("pwd", root.path().to_path_buf(), 4096)
            .unwrap();
        mgr.wait_for_exit(&id).await.unwrap();
        let out = mgr.output(&id).unwrap();
        // The pwd output should contain the temp dir's real path.
        let real_root = root.path().canonicalize().unwrap();
        let real_str = real_root.to_string_lossy();
        assert!(
            out.output.contains(real_str.as_ref()),
            "pwd output {:?} should contain {:?}",
            out.output,
            real_str
        );
        mgr.release(&id).unwrap();
    }

    #[tokio::test]
    async fn kill_makes_wait_for_exit_return_promptly() {
        // A long-running command; without kill it would run for 30s. kill(id)
        // must terminate it so the drain thread reaches EOF, sets `exited`, and
        // fires notify_waiters() — exercising the race-free wait path.
        let root = tempfile::tempdir().unwrap();
        let mgr = TerminalManager::new();
        let id = mgr
            .create("sleep 30", root.path().to_path_buf(), 1024)
            .unwrap();
        mgr.kill(&id).unwrap();
        // Should resolve well within a few seconds; bound it so a regression
        // (lost wakeup / join-hang) surfaces as a failure instead of a hang.
        tokio::time::timeout(std::time::Duration::from_secs(10), mgr.wait_for_exit(&id))
            .await
            .expect("wait_for_exit hung after kill")
            .expect("wait_for_exit returned an error");
        mgr.release(&id).unwrap();
    }

    #[tokio::test]
    async fn wait_for_exit_returns_for_already_exited_terminal() {
        // Run a command that finishes immediately, wait once so `exited` is set,
        // then call wait_for_exit AGAIN — it must take the enable-recheck fast
        // path and return without blocking.
        let root = tempfile::tempdir().unwrap();
        let mgr = TerminalManager::new();
        let id = mgr
            .create("true", root.path().to_path_buf(), 1024)
            .unwrap();
        // First wait: let the process finish and `exited` become true.
        tokio::time::timeout(std::time::Duration::from_secs(10), mgr.wait_for_exit(&id))
            .await
            .expect("initial wait_for_exit hung")
            .unwrap();
        // Second wait on an already-exited terminal must return promptly.
        tokio::time::timeout(std::time::Duration::from_secs(5), mgr.wait_for_exit(&id))
            .await
            .expect("wait_for_exit on already-exited terminal hung")
            .expect("wait_for_exit returned an error");
        mgr.release(&id).unwrap();
    }

    #[test]
    fn ring_buffer_truncates_at_cap() {
        let mut rb = RingBuffer::new(5);
        rb.push(b"hello world"); // 11 bytes > 5
        let s = rb.as_string_lossy();
        assert_eq!(s, "world");
        assert!(rb.is_truncated());
    }

    #[test]
    fn ring_buffer_no_truncation_within_cap() {
        let mut rb = RingBuffer::new(10);
        rb.push(b"hi");
        rb.push(b" there");
        assert_eq!(rb.as_string_lossy(), "hi there");
        assert!(!rb.is_truncated());
    }
}
