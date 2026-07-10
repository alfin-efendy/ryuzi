use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// One parked approval: the reply channel plus (for native-runtime prompts)
/// the owning session, so a session-wide stop can deny everything it parked.
struct Pending {
    session_pk: Option<String>,
    tx: oneshot::Sender<bool>,
}

/// Shared registry of pending tool-permission requests. The ACP permission
/// bridge registers a request id when it prompts the user; the UI resolves it
/// (allow/deny) via [`ApprovalHub::resolve`].
pub struct ApprovalHub {
    pending: Mutex<HashMap<String, Pending>>,
}

impl ApprovalHub {
    pub fn new() -> ApprovalHub {
        ApprovalHub {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, request_id: String) -> oneshot::Receiver<bool> {
        self.register_inner(None, request_id)
    }

    /// Register a pending approval tagged with its owning session, so
    /// [`ApprovalHub::resolve_session`] can deny it on a session-wide stop.
    pub fn register_for_session(
        &self,
        session_pk: &str,
        request_id: String,
    ) -> oneshot::Receiver<bool> {
        self.register_inner(Some(session_pk.to_string()), request_id)
    }

    fn register_inner(
        &self,
        session_pk: Option<String>,
        request_id: String,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id, Pending { session_pk, tx });
        rx
    }

    /// Returns true if a pending request with this id existed.
    pub fn resolve(&self, request_id: &str, allow: bool) -> bool {
        if let Some(p) = self.pending.lock().unwrap().remove(request_id) {
            let _ = p.tx.send(allow);
            true
        } else {
            false
        }
    }

    /// Resolve every pending approval registered for `session_pk` (see
    /// [`ApprovalHub::register_for_session`]); unscoped registrations are
    /// never touched. Returns how many were resolved.
    pub fn resolve_session(&self, session_pk: &str, allow: bool) -> usize {
        let mut pending = self.pending.lock().unwrap();
        let ids: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.session_pk.as_deref() == Some(session_pk))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            if let Some(p) = pending.remove(id) {
                let _ = p.tx.send(allow);
            }
        }
        ids.len()
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

    #[tokio::test]
    async fn resolve_session_denies_only_that_sessions_pending_requests() {
        let hub = ApprovalHub::new();
        let rx_a = hub.register_for_session("sess-a", "req-1".into());
        let rx_b = hub.register_for_session("sess-a", "req-2".into());
        let rx_c = hub.register_for_session("sess-b", "req-3".into());
        let rx_plain = hub.register("req-4".into());

        assert_eq!(hub.resolve_session("sess-a", false), 2);
        assert!(!rx_a.await.unwrap());
        assert!(!rx_b.await.unwrap());

        // sess-b and the unscoped (ACP-path) registration are untouched.
        assert!(hub.resolve("req-3", true));
        assert!(rx_c.await.unwrap());
        assert!(hub.resolve("req-4", true));
        assert!(rx_plain.await.unwrap());

        // Nothing left for a second sweep.
        assert_eq!(hub.resolve_session("sess-a", false), 0);
    }
}
