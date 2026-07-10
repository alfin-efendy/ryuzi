//! Context compaction for the native runtime.
//!
//! When a session's history grows past a token budget, the older turns are
//! summarized by the model and folded into a single message, keeping the most
//! recent user turns verbatim. This bounds the request size for long sessions.
//! The durable `provider_turns` ledger is not rewritten — compaction operates
//! on the in-memory projection, so a resume reloads full history and
//! re-compacts as needed.

use super::ledger::Ledger;
use super::llm::LlmStream;
use crate::llm_router::client::MessageStreamEvent;
use serde_json::{json, Value};
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

/// If the ledger exceeds `max_tokens`, summarize the older turns and compact.
/// Best-effort: a summary failure leaves the ledger unchanged.
pub async fn maybe_compact(
    llm: &Arc<dyn LlmStream>,
    model: &str,
    ledger: &mut Ledger,
    max_tokens: usize,
    keep_recent: usize,
) {
    if estimate_tokens(&ledger.messages()) < max_tokens {
        return;
    }
    // Boundary = the `keep_recent`-th-from-last real user-text turn. Only
    // compact when there is history before it to summarize.
    let user_turns: Vec<usize> = ledger
        .messages()
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m["role"] == "user"
                && m["content"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("text")
        })
        .map(|(i, _)| i)
        .collect();
    if user_turns.len() <= keep_recent {
        return;
    }
    let boundary = user_turns[user_turns.len() - keep_recent];
    if boundary == 0 {
        return;
    }
    let transcript = render_transcript(&ledger.messages()[..boundary]);
    if let Some(_summary) = summarize(llm, model, &transcript).await {
        // replaced by context_manager::compaction in Task 9
    }
}

/// Render messages to a compact plain-text transcript for summarization.
fn render_transcript(messages: &[Value]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = m["role"].as_str().unwrap_or("?");
            let body = m["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                b.get("text").and_then(|t| t.as_str()).map(str::to_string)
                            }
                            Some("tool_use") => Some(format!(
                                "[tool {}({})]",
                                b.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                                b.get("input").map(|i| i.to_string()).unwrap_or_default()
                            )),
                            Some("tool_result") => Some(format!(
                                "[result: {}]",
                                b.get("content").and_then(|c| c.as_str()).unwrap_or("")
                            )),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            format!("{role}: {body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn summarize(llm: &Arc<dyn LlmStream>, model: &str, transcript: &str) -> Option<String> {
    if model.is_empty() {
        return None;
    }
    let body = json!({
        "model": model,
        "max_tokens": 1024,
        "system": "You are compacting a coding session's history. Summarize the \
                   conversation below so an agent can continue with full context: \
                   preserve decisions made, files touched, commands run, and any \
                   open tasks. Be concise but complete. Reply with ONLY the summary.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": transcript}]}],
        "stream": true,
    });
    let mut rx = llm.stream(body).await.ok()?;
    let mut summary = String::new();
    while let Some(item) = rx.recv().await {
        if let Ok(ev) = item {
            if let Some(MessageStreamEvent::TextDelta { text, .. }) =
                MessageStreamEvent::from_event(&ev)
            {
                summary.push_str(&text);
            }
        }
    }
    let summary = summary.trim().to_string();
    (!summary.is_empty()).then_some(summary)
}

#[cfg(test)]
mod tests {
    use super::super::runner::testutil::ScriptedLlm;
    use super::*;

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": [{"type": "text", "text": text}] })
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
            .append_user(json!([{"type": "text", "text": "hi"}]))
            .await
            .unwrap();
        let before = ledger.len();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![]));
        maybe_compact(&llm, "test/model", &mut ledger, MAX_CONTEXT_TOKENS, 3).await;
        assert_eq!(ledger.len(), before);
    }
}
