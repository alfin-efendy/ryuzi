//! Compaction: summarize older history and install a compacted replacement
//! (spec §7). Triggered pre-turn/mid-turn by the runner and manually via
//! /compact.

use super::{estimate_tokens, is_context_overflow, ContextManager};
use crate::harness::native::llm::{collect_text, LlmStream};
use serde_json::{json, Value};
use std::sync::Arc;

/// Marks compaction summaries so repeated compactions replace (not stack).
pub const SUMMARY_PREFIX: &str = "[Ryuzi context summary]";
/// Token budget for raw user messages retained beside the summary (spec §14).
const RETAINED_USER_TOKENS: u64 = 20_000;
const SUMMARIZE_MAX_TOKENS: i64 = 2_048;

const DEFAULT_COMPACT_PROMPT: &str = "\
You are performing a context checkpoint compaction. Write a handoff summary \
for another LLM that will continue this session: progress so far, decisions \
made and their reasons, files touched, commands run, constraints discovered, \
open tasks and next steps, and critical data (paths, ids, snippets). Be \
concise and structured. Reply with ONLY the summary.";

pub struct CompactionOutcome {
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub window_number: u32,
}

impl ContextManager {
    /// Summarize and install a compacted history. On a summarize-call
    /// overflow, drops the oldest item and retries; other errors leave the
    /// history unchanged and propagate.
    pub async fn compact(
        &mut self,
        llm: &Arc<dyn LlmStream>,
        model: &str,
        _trigger: &str,
    ) -> anyhow::Result<CompactionOutcome> {
        anyhow::ensure!(!model.is_empty(), "compaction: no model configured");
        anyhow::ensure!(!self.ledger.is_empty(), "compaction: empty history");
        let before_tokens = self.status().active_tokens;
        let prompt = self
            .cfg()
            .compact_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

        // Pre-trim: the summarize request itself must fit the usable window
        // (spec §7.2 step 1) — rewrite oversized old tool_results first.
        self.pre_trim_tool_results();

        let summary = loop {
            let mut messages = self.ledger.messages();
            messages.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": prompt}]
            }));
            let body = json!({
                "model": model,
                "max_tokens": SUMMARIZE_MAX_TOKENS,
                "messages": messages,
                "stream": true,
            });
            match collect_text(llm, body).await {
                Ok(s) if !s.trim().is_empty() => break s.trim().to_string(),
                Ok(_) => anyhow::bail!("compaction: model returned an empty summary"),
                Err(e) if is_context_overflow(&e.to_string()) && self.ledger.len() > 1 => {
                    self.ledger.drop_oldest();
                }
                Err(e) => return Err(e),
            }
        };

        let replacement = build_replacement(&self.ledger.messages(), &summary);
        let window_number = self.ledger.replace_all(replacement).await?;
        // The indicator drops immediately: estimate the new history. Sum
        // over each message's `content` (not the `{role, content}` wrapper)
        // to match the units `append_user`/`append_assistant` accumulate
        // into `local_appended`, so this one-time re-estimate doesn't inject
        // a step-function jump in the tracked count.
        self.tokens.last_server_total = None;
        self.tokens.local_appended = self
            .ledger
            .messages()
            .iter()
            .map(|m| estimate_tokens(&m["content"]))
            .sum();
        Ok(CompactionOutcome {
            before_tokens,
            after_tokens: self.status().active_tokens,
            window_number,
        })
    }

    /// Rewrite tool_result bodies to a one-line placeholder, oldest first,
    /// until the estimated history fits the usable window.
    fn pre_trim_tool_results(&mut self) {
        let usable = self.cfg().meta.usable_window();
        let mut msgs = self.ledger.messages();
        let mut total: u64 = msgs.iter().map(estimate_tokens).sum();
        if total <= usable {
            return;
        }
        let mut changed = false;
        'outer: for m in msgs.iter_mut() {
            let Some(blocks) = m["content"].as_array_mut() else {
                continue;
            };
            for b in blocks.iter_mut() {
                if b["type"] == "tool_result"
                    && b["content"]
                        .as_str()
                        .map(|s| s.len() > 200)
                        .unwrap_or(false)
                {
                    let before = estimate_tokens(b);
                    b["content"] = Value::String(
                        "Output exceeded the available model context and was truncated".into(),
                    );
                    total = total.saturating_sub(before - estimate_tokens(b));
                    changed = true;
                    if total <= usable {
                        break 'outer;
                    }
                }
            }
        }
        if changed {
            // Install the trimmed projection in place (no checkpoint: this is
            // a pre-summarize working copy; replace_all follows right after).
            self.ledger.overwrite_in_memory(msgs);
        }
    }
}

/// Replacement history: real user messages newest→oldest under the retained
/// budget (prior summaries filtered by prefix; the oldest overflowing one
/// middle-truncated), re-ordered oldest→newest, then the summary last.
fn build_replacement(messages: &[Value], summary: &str) -> Vec<Value> {
    let mut retained: Vec<Value> = Vec::new();
    let mut budget = RETAINED_USER_TOKENS;
    for m in messages.iter().rev() {
        if m["role"] != "user" {
            continue;
        }
        let Some(text) = m["content"][0]["text"].as_str() else {
            continue; // tool_result turns don't survive compaction
        };
        if text.starts_with(SUMMARY_PREFIX) {
            continue;
        }
        let cost = estimate_tokens(m);
        if cost <= budget {
            budget -= cost;
            retained.push(m.clone());
        } else if budget > 100 {
            let max_bytes = (budget as usize) * 4;
            let truncated = super::truncate_for_context(text, max_bytes);
            retained.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": truncated}]
            }));
            break;
        } else {
            break;
        }
    }
    retained.reverse();
    retained.push(json!({
        "role": "user",
        "content": [{"type": "text", "text": format!("{SUMMARY_PREFIX}\n{summary}")}]
    }));
    retained
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::context_manager::{ContextConfig, ContextManager};
    use crate::harness::native::llm::LlmStream;
    use crate::harness::native::runner::testutil::{message_stop, text_delta, ScriptedLlm};
    use crate::llm_router::model_meta::ModelMeta;
    use serde_json::json;
    use std::sync::Arc;

    fn meta() -> ModelMeta {
        ModelMeta {
            context_window: 100_000,
            max_output_tokens: 8_192,
            supports_prompt_cache: false,
            supports_reasoning: false,
        }
    }

    async fn seeded_cm() -> ContextManager {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        for i in 0..5 {
            cm.append_user(
                json!([{"type":"text","text": format!("user turn {i} {}", "x".repeat(400))}]),
            )
            .await
            .unwrap();
            cm.append_assistant(json!([{"type":"text","text": format!("reply {i}")}]))
                .await
                .unwrap();
        }
        cm
    }

    #[tokio::test]
    async fn compact_replaces_history_with_retained_users_plus_summary() {
        let mut cm = seeded_cm().await;
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![vec![
            text_delta("Did turns 0-4; files touched: a.rs."),
            message_stop(),
        ]]));
        let before_len = cm.messages_for_request().len();
        let outcome = cm.compact(&llm, "test/model", "manual").await.unwrap();
        assert_eq!(outcome.window_number, 1);
        assert!(outcome.after_tokens < outcome.before_tokens);
        let msgs = cm.messages_for_request();
        assert!(msgs.len() < before_len);
        // Every retained message is a user message; the last is the summary.
        assert!(msgs.iter().all(|m| m["role"] == "user"));
        let last = msgs.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(last.starts_with(SUMMARY_PREFIX));
        assert!(last.contains("Did turns 0-4"));
        // Recent raw user text survives verbatim.
        assert!(msgs.iter().any(|m| m["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("user turn 4")));
    }

    #[tokio::test]
    async fn second_compaction_replaces_rather_than_stacks_summaries() {
        let mut cm = seeded_cm().await;
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![
            vec![text_delta("summary one"), message_stop()],
            vec![text_delta("summary two"), message_stop()],
        ]));
        cm.compact(&llm, "test/model", "manual").await.unwrap();
        cm.append_user(json!([{"type":"text","text":"another question"}]))
            .await
            .unwrap();
        cm.compact(&llm, "test/model", "manual").await.unwrap();
        let text = serde_json::to_string(&cm.messages_for_request()).unwrap();
        assert!(
            !text.contains("summary one"),
            "old summary filtered by prefix"
        );
        assert!(text.contains("summary two"));
        assert_eq!(text.matches(SUMMARY_PREFIX).count(), 1);
    }

    #[tokio::test]
    async fn summarize_overflow_drops_oldest_and_retries() {
        let mut cm = seeded_cm().await;
        // First summarize attempt overflows; the retry succeeds.
        let overflow = vec![(
            "error".to_string(),
            json!({"type":"error","error":{"message":"prompt is too long: 999 tokens"}}),
        )];
        let ok = vec![text_delta("recovered summary"), message_stop()];
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![overflow, ok]));
        let len_before = cm.messages_for_request().len();
        cm.compact(&llm, "test/model", "manual").await.unwrap();
        assert!(cm.messages_for_request().len() < len_before);
        let text = serde_json::to_string(&cm.messages_for_request()).unwrap();
        assert!(text.contains("recovered summary"));
    }

    #[tokio::test]
    async fn non_overflow_failure_leaves_history_unchanged() {
        let mut cm = seeded_cm().await;
        let boom = vec![(
            "error".to_string(),
            json!({"type":"error","error":{"message":"upstream 500"}}),
        )];
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![boom]));
        let before = cm.messages_for_request();
        assert!(cm.compact(&llm, "test/model", "manual").await.is_err());
        assert_eq!(cm.messages_for_request().len(), before.len());
    }
}
