//! Permission evaluation for native tool calls.
//!
//! Native tools declare a semantic permission `key` (`read`, `edit`, `bash`,
//! `webfetch`, `todowrite`, `todoread`). This module maps that key onto the
//! canonical tool name understood by [`crate::policy`] and reuses the existing,
//! tested decision engine (`PermMode` + project `tool_policies` row). When a
//! call needs a prompt, it registers with the [`ApprovalHub`] and emits a
//! [`CoreEvent::ApprovalRequested`] — the same allow/deny bridge Cockpit and
//! the Discord gateway already resolve via `resolveApproval`. `*Always`
//! replies are then recorded by [`apply_response`]: session-scoped ones fill
//! [`SessionPermOverrides`] (dropped with the session), project-scoped ones
//! persist a `tool_policies` row via `Store::set_tool_policy`.

use crate::approval::ApprovalHub;
use crate::domain::{ApprovalKind, ApprovalResponse, ApprovalScope, CoreEvent, PermMode};
use crate::harness::native::tools::PermissionSpec;
use crate::policy::{decide_tool_permission, is_safe_tool, PolicyOutcome};
use crate::store::Store;
use std::collections::HashSet;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermDecision {
    Allow,
    Deny,
}

/// Per-session "don't ask again" state, keyed by CANONICAL policy tool name
/// (the `key_to_policy_tool` output, same vocabulary as `tool_policies.tool`).
/// Dropped with the session — never persisted.
#[derive(Debug, Default)]
pub struct SessionPermOverrides {
    allow: HashSet<String>,
    deny: HashSet<String>,
}

impl SessionPermOverrides {
    pub fn set(&mut self, tool: &str, allow: bool) {
        if allow {
            self.deny.remove(tool);
            self.allow.insert(tool.to_string());
        } else {
            self.allow.remove(tool);
            self.deny.insert(tool.to_string());
        }
    }

    /// `Some(true)` = session-allowed, `Some(false)` = session-denied.
    pub fn decision_for(&self, tool: &str) -> Option<bool> {
        if self.deny.contains(tool) {
            Some(false)
        } else if self.allow.contains(tool) {
            Some(true)
        } else {
            None
        }
    }
}

/// Everything one permission check needs. Borrowed from `RunnerDeps` at the
/// dispatch site so the check itself stays a pure function of its inputs.
pub struct PermGate<'a> {
    pub perm_mode: PermMode,
    pub project_id: Option<&'a str>,
    pub store: &'a Store,
    pub overrides: &'a std::sync::Mutex<SessionPermOverrides>,
    pub session_pk: &'a str,
    pub tool_call_id: &'a str,
    pub approvals: &'a ApprovalHub,
    pub events: &'a broadcast::Sender<CoreEvent>,
    pub cancel: &'a CancellationToken,
}

/// Map a native permission key to the canonical tool name `policy` recognizes,
/// so native tools share the existing SAFE_TOOLS/EDIT_TOOLS classification.
fn key_to_policy_tool(key: &str) -> &str {
    match key {
        "read" | "todoread" => "Read",
        "todowrite" => "TodoWrite",
        "edit" => "Edit",
        "bash" => "Bash",
        "webfetch" => "WebFetch",
        "exitplanmode" => "ExitPlanMode",
        "askuserquestion" => "AskUserQuestion",
        other => other,
    }
}

/// Decide whether a native tool call may proceed.
///
/// Order (top wins): Plan hard-deny → session overrides → project
/// `tool_policies` (allowAlways AND rejectAlways) → mode auto-allow → prompt.
/// Plan sits above the session sets so "allow for this session" can never
/// punch through Plan's read-only guarantee.
pub async fn evaluate(
    spec: &PermissionSpec,
    input: &serde_json::Value,
    gate: &PermGate<'_>,
) -> PermDecision {
    let tool = key_to_policy_tool(&spec.key);
    if gate.perm_mode == PermMode::Plan && !is_safe_tool(tool) {
        return PermDecision::Deny;
    }
    match gate.overrides.lock().unwrap().decision_for(tool) {
        Some(true) => return PermDecision::Allow,
        Some(false) => return PermDecision::Deny,
        None => {}
    }
    let project_policy = match gate.project_id {
        Some(pid) => gate.store.get_tool_policy(pid, tool).await.unwrap_or(None),
        None => None,
    };
    match decide_tool_permission(gate.perm_mode, project_policy.as_deref(), tool) {
        PolicyOutcome::AutoAllow => return PermDecision::Allow,
        PolicyOutcome::Deny => return PermDecision::Deny,
        PolicyOutcome::Prompt => {}
    }
    // Prompt: register a pending approval (scoped to the session so a
    // session-wide stop can deny it), surface it, and await the reply.
    let rx = gate
        .approvals
        .register_for_session(gate.session_pk, gate.tool_call_id.to_string());
    let _ = gate.events.send(CoreEvent::ApprovalRequested {
        session_pk: gate.session_pk.to_string(),
        request_id: gate.tool_call_id.to_string(),
        tool: spec.key.clone(),
        summary: spec.summary.clone(),
        approval_kind: ApprovalKind::Tool,
        input: input.clone(),
    });
    tokio::select! {
        biased;
        // Turn stopped while parked: deny, and deregister the abandoned
        // prompt so a later resolve() can't hit a stale entry.
        _ = gate.cancel.cancelled() => {
            gate.approvals.resolve_bool(gate.tool_call_id, false);
            PermDecision::Deny
        }
        res = rx => match res {
            Ok(resp) => apply_response(gate, tool, resp).await,
            Err(_) => PermDecision::Deny,
        },
    }
}

/// Record a `*Always` reply at its scope, then return the call's verdict.
/// Scope-less `*Always` degrades to its `*Once` twin (defensive default).
async fn apply_response(gate: &PermGate<'_>, tool: &str, resp: ApprovalResponse) -> PermDecision {
    use crate::domain::ApprovalDecision as D;
    match (resp.decision, resp.scope) {
        (D::AllowAlways, Some(ApprovalScope::Session)) => {
            gate.overrides.lock().unwrap().set(tool, true);
        }
        (D::RejectAlways, Some(ApprovalScope::Session)) => {
            gate.overrides.lock().unwrap().set(tool, false);
        }
        (D::AllowAlways, Some(ApprovalScope::Project)) => {
            persist_rule(gate, tool, "allowAlways").await;
        }
        (D::RejectAlways, Some(ApprovalScope::Project)) => {
            persist_rule(gate, tool, "rejectAlways").await;
        }
        _ => {}
    }
    if resp.allowed() {
        PermDecision::Allow
    } else {
        PermDecision::Deny
    }
}

async fn persist_rule(gate: &PermGate<'_>, tool: &str, decision: &str) {
    if let Some(pid) = gate.project_id {
        // Best-effort: a failed write must not flip the user's verdict.
        let _ = gate.store.set_tool_policy(pid, tool, decision).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApprovalDecision, ApprovalResponse, ApprovalScope};
    use crate::store::Store;
    use std::sync::Arc;

    fn spec(key: &str) -> PermissionSpec {
        PermissionSpec::new(key, format!("do {key}"))
    }

    struct Fixture {
        store: Arc<Store>,
        overrides: std::sync::Mutex<SessionPermOverrides>,
        approvals: Arc<ApprovalHub>,
        events: broadcast::Sender<CoreEvent>,
        cancel: CancellationToken,
    }

    impl Fixture {
        async fn new() -> Self {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let store = Arc::new(Store::open(tmp.path()).await.unwrap());
            let (events, _rx) = broadcast::channel(16);
            Fixture {
                store,
                overrides: std::sync::Mutex::new(SessionPermOverrides::default()),
                approvals: Arc::new(ApprovalHub::new()),
                events,
                cancel: CancellationToken::new(),
            }
        }

        fn gate(&self, perm_mode: PermMode, project_id: Option<&'static str>) -> PermGate<'_> {
            PermGate {
                perm_mode,
                project_id,
                store: &self.store,
                overrides: &self.overrides,
                session_pk: "s",
                tool_call_id: "call-1",
                approvals: &self.approvals,
                events: &self.events,
                cancel: &self.cancel,
            }
        }
    }

    #[tokio::test]
    async fn safe_keys_auto_allow_without_prompt() {
        let f = Fixture::new().await;
        for key in ["read", "todoread", "todowrite"] {
            let d = evaluate(
                &spec(key),
                &serde_json::json!({}),
                &f.gate(PermMode::Default, None),
            )
            .await;
            assert_eq!(d, PermDecision::Allow, "key {key}");
        }
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn session_allow_set_skips_the_prompt() {
        let f = Fixture::new().await;
        f.overrides.lock().unwrap().set("Bash", true);
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        assert_eq!(d, PermDecision::Allow);
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn session_deny_set_hard_denies() {
        let f = Fixture::new().await;
        f.overrides.lock().unwrap().set("Bash", false);
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn plan_mode_beats_session_allow() {
        let f = Fixture::new().await;
        f.overrides.lock().unwrap().set("Bash", true);
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Plan, None),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn project_reject_always_row_denies_without_prompt() {
        let f = Fixture::new().await;
        f.store
            .set_tool_policy("p1", "Bash", "rejectAlways")
            .await
            .unwrap();
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, Some("p1")),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn allow_always_project_reply_persists_a_rule() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        let waiter = tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::AllowAlways,
                        scope: Some(ApprovalScope::Project),
                        payload: None,
                    },
                );
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, Some("p1")),
        )
        .await;
        waiter.await.unwrap();
        assert_eq!(d, PermDecision::Allow);
        assert_eq!(
            f.store
                .get_tool_policy("p1", "Bash")
                .await
                .unwrap()
                .as_deref(),
            Some("allowAlways")
        );
    }

    #[tokio::test]
    async fn allow_always_session_reply_fills_the_override_set() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::AllowAlways,
                        scope: Some(ApprovalScope::Session),
                        payload: None,
                    },
                );
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        assert_eq!(d, PermDecision::Allow);
        assert_eq!(f.overrides.lock().unwrap().decision_for("Bash"), Some(true));
    }

    #[tokio::test]
    async fn reject_always_project_reply_persists_a_rule() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        let waiter = tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::RejectAlways,
                        scope: Some(ApprovalScope::Project),
                        payload: None,
                    },
                );
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, Some("p1")),
        )
        .await;
        waiter.await.unwrap();
        assert_eq!(d, PermDecision::Deny);
        assert_eq!(
            f.store
                .get_tool_policy("p1", "Bash")
                .await
                .unwrap()
                .as_deref(),
            Some("rejectAlways")
        );
    }

    #[tokio::test]
    async fn reject_always_session_reply_fills_the_deny_set() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::RejectAlways,
                        scope: Some(ApprovalScope::Session),
                        payload: None,
                    },
                );
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
        assert_eq!(
            f.overrides.lock().unwrap().decision_for("Bash"),
            Some(false)
        );
    }

    #[tokio::test]
    async fn scope_less_always_degrades_to_once() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve(
                    &request_id,
                    ApprovalResponse {
                        decision: ApprovalDecision::AllowAlways,
                        scope: None,
                        payload: None,
                    },
                );
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, Some("p1")),
        )
        .await;
        assert_eq!(d, PermDecision::Allow);
        assert_eq!(
            f.store.get_tool_policy("p1", "Bash").await.unwrap(),
            None,
            "a scope-less AllowAlways must not persist a project rule"
        );
        assert_eq!(
            f.overrides.lock().unwrap().decision_for("Bash"),
            None,
            "a scope-less AllowAlways must not fill the session override set either"
        );
    }

    #[tokio::test]
    async fn deny_reply_denies() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                approvals.resolve_bool(&request_id, false);
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
    }

    #[tokio::test]
    async fn cancellation_denies_and_deregisters() {
        let f = Fixture::new().await;
        let input = serde_json::json!({});
        let gate = f.gate(PermMode::Default, None);
        let bash = spec("bash");
        let fut = evaluate(&bash, &input, &gate);
        tokio::pin!(fut);
        assert!(futures::poll!(fut.as_mut()).is_pending());
        assert!(f.approvals.has_pending());
        f.cancel.cancel();
        assert_eq!(fut.await, PermDecision::Deny);
        assert!(!f.approvals.has_pending());
    }

    #[tokio::test]
    async fn prompt_event_carries_kind_and_input() {
        let f = Fixture::new().await;
        let approvals = f.approvals.clone();
        let mut rx = f.events.subscribe();
        let waiter = tokio::spawn(async move {
            match rx.recv().await.unwrap() {
                CoreEvent::ApprovalRequested {
                    approval_kind,
                    input,
                    ..
                } => {
                    assert_eq!(approval_kind, ApprovalKind::Tool);
                    assert_eq!(input["command"], "rm -rf ./x");
                    approvals.resolve_bool("call-1", true);
                }
                other => panic!("unexpected event {other:?}"),
            }
        });
        let d = evaluate(
            &spec("bash"),
            &serde_json::json!({"command": "rm -rf ./x"}),
            &f.gate(PermMode::Default, None),
        )
        .await;
        waiter.await.unwrap();
        assert_eq!(d, PermDecision::Allow);
    }
}
