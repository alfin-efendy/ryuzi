use crate::domain::ApprovalResponse;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Identifies a pending approval within its owning durable agent run. Tool call
/// identifiers are only unique inside a provider turn, so `request_id` alone
/// is insufficient once delegated runs execute concurrently.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalKey {
    pub run_id: String,
    pub request_id: String,
}

impl ApprovalKey {
    pub fn new(run_id: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            request_id: request_id.into(),
        }
    }
}

/// One parked approval: the reply channel plus (for native-runtime prompts)
/// the owning session, so a session-wide stop can deny everything it parked.
struct Pending {
    session_pk: Option<String>,
    tx: oneshot::Sender<ApprovalResponse>,
}

/// Shared registry of pending tool-permission requests. The native runtime's
/// permission gate (see `harness::native::permission`) registers a run-scoped
/// request key when it prompts the user; the UI resolves it via
/// [`ApprovalHub::resolve`].
pub struct ApprovalHub {
    pending: Mutex<HashMap<ApprovalKey, Pending>>,
}

impl ApprovalHub {
    pub fn new() -> ApprovalHub {
        ApprovalHub {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, key: ApprovalKey) -> oneshot::Receiver<ApprovalResponse> {
        self.register_inner(None, key)
    }

    /// Register a pending approval tagged with its owning session, so
    /// [`ApprovalHub::resolve_session`] can deny it on a session-wide stop.
    pub fn register_for_session(
        &self,
        session_pk: &str,
        key: ApprovalKey,
    ) -> oneshot::Receiver<ApprovalResponse> {
        self.register_inner(Some(session_pk.to_string()), key)
    }

    fn register_inner(
        &self,
        session_pk: Option<String>,
        key: ApprovalKey,
    ) -> oneshot::Receiver<ApprovalResponse> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(key, Pending { session_pk, tx });
        rx
    }

    /// Returns true if a pending request with this run-scoped key existed.
    pub fn resolve(&self, key: &ApprovalKey, response: ApprovalResponse) -> bool {
        if let Some(p) = self.pending.lock().unwrap().remove(key) {
            let _ = p.tx.send(response);
            true
        } else {
            false
        }
    }

    /// Binary convenience for callers that only know allow/deny (CLI y/N,
    /// gateway fan-out, cancellation cleanup).
    pub fn resolve_bool(&self, key: &ApprovalKey, allow: bool) -> bool {
        self.resolve(key, ApprovalResponse::once(allow))
    }

    /// Resolve every pending approval registered for `session_pk` (see
    /// [`ApprovalHub::register_for_session`]); unscoped registrations are
    /// never touched. Returns how many were resolved.
    pub fn resolve_session(&self, session_pk: &str, allow: bool) -> usize {
        let mut pending = self.pending.lock().unwrap();
        let keys: Vec<ApprovalKey> = pending
            .iter()
            .filter(|(_, p)| p.session_pk.as_deref() == Some(session_pk))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &keys {
            if let Some(p) = pending.remove(key) {
                let _ = p.tx.send(ApprovalResponse::once(allow));
            }
        }
        keys.len()
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
        let key = ApprovalKey::new("run-1", "req-1");
        let rx = hub.register(key.clone());
        assert!(hub.resolve_bool(&key, true));
        assert!(rx.await.unwrap().allowed());
        assert!(!hub.resolve_bool(&ApprovalKey::new("run-1", "nope"), true));
    }

    #[tokio::test]
    async fn resolve_requires_the_owning_run_identity() {
        let hub = ApprovalHub::new();
        let first = ApprovalKey::new("run-a", "request-1");
        let second = ApprovalKey::new("run-b", "request-1");
        let rx_first = hub.register(first.clone());
        let rx_second = hub.register(second.clone());

        assert!(hub.resolve_bool(&first, true));
        assert!(rx_first.await.unwrap().allowed());
        assert!(hub.has_pending());
        assert!(hub.resolve_bool(&second, false));
        assert!(!rx_second.await.unwrap().allowed());
    }

    #[tokio::test]
    async fn resolve_session_denies_only_that_sessions_pending_requests() {
        let hub = ApprovalHub::new();
        let rx_a = hub.register_for_session("sess-a", ApprovalKey::new("run-a", "req-1"));
        let rx_b = hub.register_for_session("sess-a", ApprovalKey::new("run-b", "req-2"));
        let rx_c = hub.register_for_session("sess-b", ApprovalKey::new("run-c", "req-3"));
        let plain = ApprovalKey::new("run-d", "req-4");
        let rx_plain = hub.register(plain.clone());

        assert_eq!(hub.resolve_session("sess-a", false), 2);
        assert!(!rx_a.await.unwrap().allowed());
        assert!(!rx_b.await.unwrap().allowed());
        assert!(hub.resolve_bool(&ApprovalKey::new("run-c", "req-3"), true));
        assert!(rx_c.await.unwrap().allowed());
        assert!(hub.resolve_bool(&plain, true));
        assert!(rx_plain.await.unwrap().allowed());
        assert_eq!(hub.resolve_session("sess-a", false), 0);
    }

    #[tokio::test]
    async fn resolve_carries_a_structured_response() {
        use crate::domain::{ApprovalDecision, ApprovalResponse, ApprovalScope};
        let hub = ApprovalHub::new();
        let key = ApprovalKey::new("run-s", "req-s");
        let rx = hub.register(key.clone());
        assert!(hub.resolve(
            &key,
            ApprovalResponse {
                decision: ApprovalDecision::AllowAlways,
                scope: Some(ApprovalScope::Project),
                payload: Some(serde_json::json!({"mode": "acceptEdits"})),
            },
        ));
        let got = rx.await.unwrap();
        assert_eq!(got.decision, ApprovalDecision::AllowAlways);
        assert_eq!(got.scope, Some(ApprovalScope::Project));
        assert!(got.allowed());
    }

    #[tokio::test]
    async fn resolve_bool_maps_to_once_decisions() {
        use crate::domain::ApprovalDecision;
        let hub = ApprovalHub::new();
        let key = ApprovalKey::new("run-b", "req-b");
        let rx = hub.register(key.clone());
        assert!(hub.resolve_bool(&key, false));
        let got = rx.await.unwrap();
        assert_eq!(got.decision, ApprovalDecision::RejectOnce);
        assert!(!got.allowed());
    }
}
