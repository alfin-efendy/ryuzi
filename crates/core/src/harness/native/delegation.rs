//! The Hermes-verbatim async-delegation completion block (spec §6.2). Ported
//! byte-for-byte from hermes-agent/tools/process_registry.py:2040-2076
//! (`_format_async_delegation`) — the header line, the intro sentence, and
//! the `--- RESULT ---` separator are load-bearing prompt copy and must NOT
//! be reworded. Fields Ryuzi does not track (dispatched_at, context,
//! toolsets, api_calls, duration) are omitted; the verbatim spine (header +
//! intro + goal + role/model + status + result) is preserved.

/// A completed (or interrupted/errored) background delegation, as reported by
/// the async-delegation worker (Task 7). Formatted by
/// [`format_delegation_block`] into the text re-injected into the parent
/// chat's context (Task 9 drainer).
pub struct DelegationResult {
    pub id: String,
    pub goal: String,
    pub agent_type: String,
    pub model: String,
    /// "completed" | "interrupted" | "error" (any other value formats like
    /// "error" — mirrors Hermes' `else` fallthrough).
    pub status: String,
    pub summary: String,
    pub error: Option<String>,
}

/// Format the `[ASYNC DELEGATION COMPLETE — {id}]` block a completed
/// background worker's result re-enters the parent chat as. See the module
/// doc for the Hermes source this is ported from.
pub fn format_delegation_block(r: &DelegationResult) -> String {
    let mut lines: Vec<String> = vec![
        format!("[ASYNC DELEGATION COMPLETE — {}]", r.id),
        "A background subagent you dispatched earlier has finished. You may \
         have moved on since dispatching it; the full task source is below so \
         you can act on the result or re-dispatch if things have changed."
            .to_string(),
        String::new(),
        format!("Original goal: {}", r.goal),
        format!("Role: {}   Model: {}", r.agent_type, r.model),
        format!("Status: {}", r.status),
        "--- RESULT ---".to_string(),
    ];
    match r.status.as_str() {
        "completed" | "success" if !r.summary.is_empty() => lines.push(r.summary.clone()),
        "interrupted" => {
            lines.push(format!(
                "The subagent was interrupted before completing{}",
                r.error
                    .as_ref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_else(|| ".".into())
            ));
            if !r.summary.is_empty() {
                lines.push("Partial output:".to_string());
                lines.push(r.summary.clone());
            }
        }
        _ => {
            lines.push(format!(
                "The subagent did not complete successfully (status={}).{}",
                r.status,
                r.error
                    .as_ref()
                    .map(|e| format!("\n{e}"))
                    .unwrap_or_default()
            ));
            if !r.summary.is_empty() {
                lines.push("Partial output:".to_string());
                lines.push(r.summary.clone());
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_block_is_hermes_verbatim() {
        let block = format_delegation_block(&DelegationResult {
            id: "d-7".into(),
            goal: "audit the auth module".into(),
            agent_type: "general".into(),
            model: "anthropic/claude".into(),
            status: "completed".into(),
            summary: "Found two issues.".into(),
            error: None,
        });
        assert!(block.starts_with("[ASYNC DELEGATION COMPLETE — d-7]\n"));
        assert!(
            block.contains('\u{2014}'),
            "header must use an em-dash (U+2014), not a hyphen"
        );
        assert!(block.contains(
            "A background subagent you dispatched earlier has finished. You may \
             have moved on since dispatching it; the full task source is below so \
             you can act on the result or re-dispatch if things have changed."
        ));
        assert!(block.contains("Original goal: audit the auth module"));
        assert!(block.contains("Role: general   Model: anthropic/claude"));
        assert!(block.contains("--- RESULT ---"));
        assert!(block.trim_end().ends_with("Found two issues."));
        // Full-string assertion: catches any future drift in the verbatim
        // spine (header + intro + separator), byte for byte.
        assert_eq!(
            block,
            "[ASYNC DELEGATION COMPLETE — d-7]\n\
             A background subagent you dispatched earlier has finished. You may \
             have moved on since dispatching it; the full task source is below so \
             you can act on the result or re-dispatch if things have changed.\n\
             \n\
             Original goal: audit the auth module\n\
             Role: general   Model: anthropic/claude\n\
             Status: completed\n\
             --- RESULT ---\n\
             Found two issues."
        );
    }

    #[test]
    fn error_block_names_the_failure() {
        let block = format_delegation_block(&DelegationResult {
            id: "d-8".into(),
            goal: "g".into(),
            agent_type: "general".into(),
            model: "m".into(),
            status: "error".into(),
            summary: String::new(),
            error: Some("boom".into()),
        });
        assert!(block.contains("did not complete successfully (status=error)"));
        assert!(block.contains("boom"));
    }
}
