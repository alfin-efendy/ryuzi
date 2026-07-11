//! Mid-turn steering (Task B3): a user message sent while a turn is already
//! running is buffered here instead of racing the in-flight turn, then
//! injected into the NEXT tool-result batch `drive()` appends — the turn
//! itself is never bypassed or interrupted, only nudged on its own next
//! iteration.
//!
//! `NativeSession` owns one [`SteerBuffer`] for its whole lifetime and clones
//! it into `RunnerDeps` at session start (see
//! `NativeHarness::start_session`), so a `steer()` call racing an
//! already-running turn still lands on the exact buffer that turn's
//! `drive()` loop drains.

use std::sync::{Arc, Mutex};

/// Hermes' verbatim out-of-band marker — the system prompt
/// ([`super::context`]) references this exact text, so keep it byte-identical
/// wherever it's read.
pub const STEER_MARKER_OPEN: &str = "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered mid-turn; not tool output]";
pub const STEER_MARKER_CLOSE: &str = "[/OUT-OF-BAND USER MESSAGE]";

/// Buffered mid-turn user messages, shared between the session handle
/// (`push`, called by `NativeSession::steer`) and the running turn's
/// `drive()` loop (`take_block`, drained after each tool-result batch).
/// Cheaply `Clone`— every clone shares the same underlying `Vec` via `Arc`.
#[derive(Clone, Default)]
pub struct SteerBuffer(Arc<Mutex<Vec<String>>>);

impl SteerBuffer {
    pub fn new() -> Self {
        SteerBuffer(Arc::new(Mutex::new(Vec::new())))
    }

    /// Queue a message for the next drain.
    pub fn push(&self, text: String) {
        self.0.lock().unwrap().push(text);
    }

    /// Take (and clear) every buffered message, in push order.
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// Render every buffered message as one marker-wrapped block, or `None`
    /// when nothing was pushed since the last drain. This is the ONLY read
    /// path `drive()` should use — it both drains and formats in one step so
    /// no caller can peek without consuming.
    pub fn take_block(&self) -> Option<String> {
        let msgs = self.drain();
        if msgs.is_empty() {
            return None;
        }
        Some(format!(
            "{STEER_MARKER_OPEN}\n{}\n{STEER_MARKER_CLOSE}",
            msgs.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_buffer_push_drain() {
        let b = SteerBuffer::new();
        b.push("do X".into());
        b.push("also Y".into());
        assert_eq!(b.drain(), vec!["do X".to_string(), "also Y".to_string()]);
        assert!(b.drain().is_empty());
    }

    #[test]
    fn take_block_is_none_when_empty() {
        let b = SteerBuffer::new();
        assert!(b.take_block().is_none());
    }

    #[test]
    fn take_block_wraps_buffered_messages_in_the_verbatim_marker_and_drains() {
        let b = SteerBuffer::new();
        b.push("stop and check the tests first".into());
        b.push("also rename the function".into());
        let block = b.take_block().expect("buffered messages produce a block");
        assert!(block.starts_with(STEER_MARKER_OPEN));
        assert!(block.ends_with(STEER_MARKER_CLOSE));
        assert!(block.contains("stop and check the tests first"));
        assert!(block.contains("also rename the function"));
        // Consumed: nothing left to take.
        assert!(b.take_block().is_none());
    }
}
