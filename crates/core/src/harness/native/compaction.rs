//! Context compaction for the native runtime.
//!
//! **This module is a temporary stub.** Compaction logic is being rebuilt in
//! `context_manager`. The module will be deleted once that work lands.

use super::ledger::Ledger;
use super::llm::LlmStream;
use serde_json::Value;
use std::sync::Arc;

/// Approximate token budget before compaction kicks in.
pub const MAX_CONTEXT_TOKENS: usize = 120_000;
/// How many recent user turns (and their responses) to keep verbatim.
pub const KEEP_RECENT_USER_TURNS: usize = 3;

/// Rough char/4 token estimate for a set of messages.
pub fn estimate_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|m| serde_json::to_string(m).map(|s| s.len()).unwrap_or(0))
        .sum::<usize>()
        / 4
}

/// No-op placeholder. Compaction is being rebuilt in `context_manager`.
pub async fn maybe_compact(
    _llm: &Arc<dyn LlmStream>,
    _model: &str,
    _ledger: &mut Ledger,
    _max_tokens: usize,
    _keep_recent: usize,
) {
    // No-op: compaction is being rebuilt in context_manager (this module is deleted when that lands).
}

#[cfg(test)]
mod tests {
    use super::super::runner::testutil::ScriptedLlm;
    use super::*;

    fn user(text: &str) -> Value {
        serde_json::json!({ "role": "user", "content": [{"type": "text", "text": text}] })
    }

    #[test]
    fn estimate_scales_with_size() {
        let small = estimate_tokens(&[user("hi")]);
        let big = estimate_tokens(&[user(&"x".repeat(4000))]);
        assert!(big > small + 500);
    }

    #[tokio::test]
    async fn under_budget_is_a_noop() {
        let mut ledger = Ledger::ephemeral("s");
        ledger
            .append_user(serde_json::json!([{"type": "text", "text": "hi"}]))
            .await
            .unwrap();
        let before = ledger.len();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![]));
        maybe_compact(&llm, "test/model", &mut ledger, MAX_CONTEXT_TOKENS, 3).await;
        assert_eq!(ledger.len(), before);
    }
}
