use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Shared registry of pending tool-permission requests. The ACP permission
/// bridge registers a request id when it prompts the user; the UI resolves it
/// (allow/deny) via [`ApprovalHub::resolve`].
pub struct ApprovalHub {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalHub {
    pub fn new() -> ApprovalHub {
        ApprovalHub {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, request_id: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id, tx);
        rx
    }

    /// Returns true if a pending request with this id existed.
    pub fn resolve(&self, request_id: &str, allow: bool) -> bool {
        if let Some(tx) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(allow);
            true
        } else {
            false
        }
    }

    /// Returns `true` if the hub currently has any unresolved registrations.
    /// Useful in tests to assert that the bridge never registered a request
    /// (i.e. auto-allow short-circuited before the hub).
    pub fn has_pending(&self) -> bool {
        !self.pending.lock().unwrap().is_empty()
    }
}

impl Default for ApprovalHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_then_resolve_completes_the_receiver() {
        let hub = ApprovalHub::new();
        let rx = hub.register("req-1".into());
        assert!(hub.resolve("req-1", true));
        assert!(rx.await.unwrap());
        // resolving an unknown id returns false
        assert!(!hub.resolve("nope", true));
    }
}
