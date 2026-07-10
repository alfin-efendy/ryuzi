//! Compaction: summarize older history and install a compacted replacement
//! (spec §7). Triggered pre-turn/mid-turn by the runner and manually via
//! /compact.

use super::{estimate_tokens, is_context_overflow, ContextManager};
use crate::harness::native::llm::{collect_text, LlmStream};
use crate::llm_router::model_effort::TurnEffortPolicy;
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
    ///
    /// All pre-trim and retry-drop work happens on a local `working` copy of
    /// the messages, never on `self.ledger` directly — the ledger is mutated
    /// exactly once, via `replace_all`, and only after the summarize call has
    /// actually succeeded. This guarantees a failed compaction (overflow
    /// persisting down to one message, or any other error) leaves history
    /// byte-for-byte unchanged.
    pub async fn compact(
        &mut self,
        llm: &Arc<dyn LlmStream>,
        model: &str,
        _trigger: &str,
        effort_policy: Arc<TurnEffortPolicy>,
    ) -> anyhow::Result<CompactionOutcome> {
        anyhow::ensure!(!model.is_empty(), "compaction: no model configured");
        anyhow::ensure!(!self.ledger.is_empty(), "compaction: empty history");
        let before_tokens = self.status().active_tokens;
        let prompt = self
            .cfg()
            .compact_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

        let mut working: Vec<Value> = self.ledger.messages();

        // Pre-trim: the summarize request itself must fit the usable window
        // (spec §7.2 step 1) — rewrite oversized old tool_results first.
        pre_trim_tool_results(&mut working, self.cfg().meta.usable_window());

        let summary = loop {
            let mut messages = working.clone();
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
            match collect_text(llm, body, effort_policy.clone()).await {
                Ok(s) if !s.trim().is_empty() => break s.trim().to_string(),
                Ok(_) => anyhow::bail!("compaction: model returned an empty summary"),
                Err(e) if is_context_overflow(&e.to_string()) && working.len() > 1 => {
                    drop_oldest_message(&mut working);
                }
                Err(e) => return Err(e),
            }
        };

        let replacement = build_replacement(&working, &summary);
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
}

/// Rewrite tool_result bodies to a one-line placeholder, oldest first, until
/// the estimated size fits `usable`. Operates on a working copy so a
/// pre-trim that doesn't get far enough never leaks into the ledger.
fn pre_trim_tool_results(msgs: &mut [Value], usable: u64) {
    let mut total: u64 = msgs.iter().map(estimate_tokens).sum();
    if total <= usable {
        return;
    }
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
                total = total.saturating_sub(before.saturating_sub(estimate_tokens(b)));
                if total <= usable {
                    break 'outer;
                }
            }
        }
    }
}

/// Drop the oldest message, then keep dropping until the front is a valid
/// history start: a user turn whose first block is not a tool_result.
/// (Compaction overflow-recovery — spec §7.2.) Operates on a working copy,
/// never the ledger directly — see `ContextManager::compact`.
fn drop_oldest_message(msgs: &mut Vec<Value>) {
    if msgs.is_empty() {
        return;
    }
    msgs.remove(0);
    while let Some(front) = msgs.first() {
        let role_ok = front["role"] == "user";
        let first_block_type = front["content"][0]["type"].as_str().unwrap_or("");
        if role_ok && first_block_type != "tool_result" {
            break;
        }
        msgs.remove(0);
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
        // User turns with attachments carry image blocks before the text
        // block (see `runner::user_content_blocks`), so the text can't be
        // assumed to live at `content[0]` — scan for the first text block.
        let Some(text) = first_text_block(&m["content"]) else {
            continue; // tool_result turns don't survive compaction
        };
        if text.starts_with(SUMMARY_PREFIX) {
            continue;
        }
        let cost = estimate_tokens(m);
        if cost <= budget {
            budget -= cost;
            retained.push(m.clone()); // full clone: attachments retained
        } else if budget > 100 {
            let max_bytes = (budget as usize) * 4;
            let truncated = super::truncate_for_context(text, max_bytes);
            // Middle-truncation rebuilds a text-only message — any image or
            // other non-text blocks on this one oldest overflowing message
            // are dropped here (acceptable: only ever affects one message).
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

/// The text of the first `"type": "text"` block in a content array, if any.
fn first_text_block(content: &Value) -> Option<&str> {
    content
        .as_array()?
        .iter()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
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

    fn policy() -> Arc<TurnEffortPolicy> {
        Arc::new(TurnEffortPolicy {
            requested_model: "test/model".into(),
            project_override: None,
            route_compatibility: Default::default(),
            configured: Default::default(),
            surfaces: Default::default(),
        })
    }

    fn meta() -> ModelMeta {
        ModelMeta {
            context_window: 100_000,
            max_output_tokens: 8_192,
            supports_prompt_cache: false,
            supports_reasoning: false,
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
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
        let outcome = cm
            .compact(&llm, "test/model", "manual", policy())
            .await
            .unwrap();
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
        cm.compact(&llm, "test/model", "manual", policy())
            .await
            .unwrap();
        cm.append_user(json!([{"type":"text","text":"another question"}]))
            .await
            .unwrap();
        cm.compact(&llm, "test/model", "manual", policy())
            .await
            .unwrap();
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
        cm.compact(&llm, "test/model", "manual", policy())
            .await
            .unwrap();
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
        assert!(cm
            .compact(&llm, "test/model", "manual", policy())
            .await
            .is_err());
        assert_eq!(cm.messages_for_request().len(), before.len());
    }

    #[tokio::test]
    async fn retains_attachment_bearing_user_turns() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        // Image blocks precede the text block, matching
        // `runner::user_content_blocks`.
        cm.append_user(json!([
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}},
            {"type": "text", "text": "look at this screenshot"}
        ]))
        .await
        .unwrap();
        cm.append_assistant(json!([{"type":"text","text":"got it, I see the screenshot"}]))
            .await
            .unwrap();
        for i in 0..3 {
            cm.append_user(json!([{"type":"text","text": format!("follow up {i}")}]))
                .await
                .unwrap();
            cm.append_assistant(json!([{"type":"text","text": format!("reply {i}")}]))
                .await
                .unwrap();
        }
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![vec![
            text_delta("summary of the conversation"),
            message_stop(),
        ]]));
        cm.compact(&llm, "test/model", "manual", policy())
            .await
            .unwrap();
        let text = serde_json::to_string(&cm.messages_for_request()).unwrap();
        assert!(
            text.contains("look at this screenshot"),
            "attachment-bearing user turn's text must survive compaction"
        );
        assert!(
            text.contains("\"image\""),
            "the image block itself must be retained, not just the text"
        );
    }

    #[tokio::test]
    async fn exhausted_overflow_leaves_history_unchanged() {
        // A tiny context window so pre-trim fires against the oversized
        // tool_result below.
        let tiny_meta = ModelMeta {
            context_window: 40,
            max_output_tokens: 8_192,
            supports_prompt_cache: false,
            supports_reasoning: false,
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
        };
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(tiny_meta));
        cm.append_user(json!([{"type":"text","text":"start task"}]))
            .await
            .unwrap();
        cm.append_assistant(json!([{"type":"tool_use","id":"t1","name":"bash","input":{}}]))
            .await
            .unwrap();
        cm.append_user(json!([
            {"type":"tool_result","tool_use_id":"t1","content":"x".repeat(300),"is_error":false}
        ]))
        .await
        .unwrap();
        cm.append_assistant(json!([{"type":"text","text":"done"}]))
            .await
            .unwrap();
        cm.append_user(json!([{"type":"text","text":"continue"}]))
            .await
            .unwrap();

        // 5 messages in the ledger: at most 4 retries plus the initial
        // attempt, so 5 scripted overflow turns exhausts every retry.
        let overflow_turn = || {
            vec![(
                "error".to_string(),
                json!({"type":"error","error":{"message":"prompt is too long: 999999 tokens"}}),
            )]
        };
        let scripts: Vec<Vec<_>> = (0..5).map(|_| overflow_turn()).collect();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(scripts));

        let before = cm.messages_for_request();
        let before_json = serde_json::to_string(&before).unwrap();
        let result = cm.compact(&llm, "test/model", "manual", policy()).await;
        match result {
            Err(e) => assert!(is_context_overflow(&e.to_string())),
            Ok(_) => panic!("expected compaction to fail on persistent overflow"),
        }

        let after = cm.messages_for_request();
        assert_eq!(after.len(), before.len(), "pre-trim must not leak either");
        assert_eq!(serde_json::to_string(&after).unwrap(), before_json);
    }

    #[test]
    fn drop_oldest_message_preserves_a_valid_history_start() {
        let mut msgs = vec![
            json!({"role":"user","content":[{"type":"text","text":"u0"}]}),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"bash","input":{}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"out","is_error":false}]}),
            json!({"role":"assistant","content":[{"type":"text","text":"a1"}]}),
            json!({"role":"user","content":[{"type":"text","text":"u1"}]}),
        ];
        // Dropping u0 must also drop the now-orphaned assistant tool_use AND
        // its tool_result AND the trailing assistant, landing on the next
        // real user turn.
        drop_oldest_message(&mut msgs);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["type"], "text");
        assert_eq!(msgs[0]["content"][0]["text"], "u1");
    }
}
