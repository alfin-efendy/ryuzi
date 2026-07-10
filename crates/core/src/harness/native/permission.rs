//! Permission evaluation for native tool calls.
//!
//! Native tools declare a semantic permission `key` (`read`, `edit`, `bash`,
//! `webfetch`, `todowrite`, `todoread`). This module maps that key onto the
//! canonical tool name understood by [`crate::policy`] and reuses the existing,
//! tested decision engine (`PermMode` + project `allowAlways` policy). When a
//! call needs a prompt, it registers with the [`ApprovalHub`] and emits a
//! [`CoreEvent::ApprovalRequested`] — the same allow/deny bridge Cockpit and
//! the Discord gateway already resolve via `resolveApproval`.

use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, PermMode};
use crate::harness::native::tools::PermissionSpec;
use crate::policy::{decide_tool_permission, PolicyOutcome};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// The outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermDecision {
    Allow,
    Deny,
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
        other => other,
    }
}

/// Decide whether a native tool call may proceed. Auto-allows via
/// `PermMode`/project policy where possible; otherwise prompts the user and
/// blocks on their reply — or on the turn's cancellation token, so a stopped
/// turn is denied ("interrupted") instead of parking forever with a dangling
/// `tool_use` in the ledger.
#[allow(clippy::too_many_arguments)]
pub async fn evaluate(
    spec: &PermissionSpec,
    perm_mode: PermMode,
    project_policy: Option<&str>,
    session_pk: &str,
    tool_call_id: &str,
    approvals: &ApprovalHub,
    events: &broadcast::Sender<CoreEvent>,
    cancel: &CancellationToken,
) -> PermDecision {
    let tool = key_to_policy_tool(&spec.key);
    match decide_tool_permission(perm_mode, project_policy, tool) {
        PolicyOutcome::AutoAllow => return PermDecision::Allow,
        // Plan mode hard-denies mutations without a prompt.
        PolicyOutcome::Deny => return PermDecision::Deny,
        PolicyOutcome::Prompt => {}
    }
    // Prompt: register a pending approval (scoped to the session so a
    // session-wide stop can deny it), surface it, and await the reply.
    let rx = approvals.register_for_session(session_pk, tool_call_id.to_string());
    let _ = events.send(CoreEvent::ApprovalRequested {
        session_pk: session_pk.to_string(),
        request_id: tool_call_id.to_string(),
        tool: spec.key.clone(),
        summary: spec.summary.clone(),
    });
    tokio::select! {
        biased;
        // Turn stopped while parked: deny, and deregister the abandoned
        // prompt so a later resolve() can't hit a stale entry.
        _ = cancel.cancelled() => {
            approvals.resolve(tool_call_id, false);
            PermDecision::Deny
        }
        res = rx => match res {
            Ok(true) => PermDecision::Allow,
            // Explicit deny, or the sender was dropped (session ended) — deny.
            Ok(false) | Err(_) => PermDecision::Deny,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str) -> PermissionSpec {
        PermissionSpec::new(key, format!("do {key}"))
    }

    #[tokio::test]
    async fn safe_keys_auto_allow_in_default_mode() {
        let hub = ApprovalHub::new();
        let (tx, _rx) = broadcast::channel(4);
        for key in ["read", "todoread", "todowrite"] {
            let d = evaluate(
                &spec(key),
                PermMode::Default,
                None,
                "s",
                "t1",
                &hub,
                &tx,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(d, PermDecision::Allow, "key {key} should auto-allow");
        }
        assert!(!hub.has_pending(), "no prompt should have been registered");
    }

    #[tokio::test]
    async fn plan_mode_denies_mutations_without_prompting_but_allows_reads() {
        let hub = ApprovalHub::new();
        let (tx, _rx) = broadcast::channel(4);
        // Read-class tools still auto-allow under Plan.
        let read = evaluate(
            &spec("read"),
            PermMode::Plan,
            None,
            "s",
            "t1",
            &hub,
            &tx,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(read, PermDecision::Allow);
        // Edit + bash are hard-denied WITHOUT registering a prompt.
        for key in ["edit", "bash"] {
            let d = evaluate(
                &spec(key),
                PermMode::Plan,
                None,
                "s",
                "t2",
                &hub,
                &tx,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(d, PermDecision::Deny, "{key} must be denied in Plan mode");
        }
        assert!(!hub.has_pending(), "Plan denial must not prompt the user");
    }

    #[tokio::test]
    async fn edit_prompts_in_default_but_allows_under_accept_edits() {
        let hub = ApprovalHub::new();
        let (tx, _rx) = broadcast::channel(4);
        let d = evaluate(
            &spec("edit"),
            PermMode::AcceptEdits,
            None,
            "s",
            "t1",
            &hub,
            &tx,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(d, PermDecision::Allow);
    }

    #[tokio::test]
    async fn allow_always_project_policy_allows_bash() {
        let hub = ApprovalHub::new();
        let (tx, _rx) = broadcast::channel(4);
        let d = evaluate(
            &spec("bash"),
            PermMode::Default,
            Some("allowAlways"),
            "s",
            "t1",
            &hub,
            &tx,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(d, PermDecision::Allow);
    }

    #[tokio::test]
    async fn prompt_path_registers_emits_and_resolves() {
        let hub = std::sync::Arc::new(ApprovalHub::new());
        let (tx, mut rx) = broadcast::channel(4);
        let hub2 = hub.clone();
        // Resolve the approval once the event is observed.
        let waiter = tokio::spawn(async move {
            let ev = rx.recv().await.unwrap();
            match ev {
                CoreEvent::ApprovalRequested {
                    request_id, tool, ..
                } => {
                    assert_eq!(tool, "bash");
                    hub2.resolve(&request_id, true);
                }
                other => panic!("unexpected event {other:?}"),
            }
        });
        let d = evaluate(
            &spec("bash"),
            PermMode::Default,
            None,
            "s",
            "call-1",
            &hub,
            &tx,
            &CancellationToken::new(),
        )
        .await;
        waiter.await.unwrap();
        assert_eq!(d, PermDecision::Allow);
    }

    #[tokio::test]
    async fn deny_reply_denies() {
        let hub = std::sync::Arc::new(ApprovalHub::new());
        let (tx, mut rx) = broadcast::channel(4);
        let hub2 = hub.clone();
        tokio::spawn(async move {
            if let Ok(CoreEvent::ApprovalRequested { request_id, .. }) = rx.recv().await {
                hub2.resolve(&request_id, false);
            }
        });
        let d = evaluate(
            &spec("bash"),
            PermMode::Default,
            None,
            "s",
            "call-2",
            &hub,
            &tx,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(d, PermDecision::Deny);
    }

    #[tokio::test]
    async fn cancellation_unblocks_a_parked_prompt_as_deny_and_deregisters() {
        let hub = ApprovalHub::new();
        let (tx, _rx) = broadcast::channel(4);
        let cancel = CancellationToken::new();
        let bash_spec = spec("bash");
        let fut = evaluate(
            &bash_spec,
            PermMode::Default,
            None,
            "s",
            "call-3",
            &hub,
            &tx,
            &cancel,
        );
        tokio::pin!(fut);
        // First poll drives evaluate up to the parked prompt.
        assert!(futures::poll!(fut.as_mut()).is_pending());
        assert!(hub.has_pending(), "the prompt was registered");
        cancel.cancel();
        assert_eq!(fut.await, PermDecision::Deny);
        assert!(
            !hub.has_pending(),
            "a cancelled prompt must be deregistered so it can't be resolved later"
        );
    }
}
