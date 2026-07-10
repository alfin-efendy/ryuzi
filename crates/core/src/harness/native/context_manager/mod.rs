//! ContextManager: single owner of a native session's in-memory history and
//! token state (spec §6). The Ledger underneath persists provider turns and
//! compaction checkpoints; the runner talks only to this type.

pub mod accounting;
pub mod compaction;
pub mod truncation;

pub use accounting::estimate_tokens;
pub use truncation::truncate_for_context;

use super::ledger::Ledger;
use crate::llm_router::model_meta::ModelMeta;
use crate::store::Store;
use accounting::TokenState;
use serde_json::Value;
use std::sync::Arc;

pub struct ContextConfig {
    pub meta: ModelMeta,
    pub auto_compact_percent: u64,
    pub tool_output_max_bytes: usize,
    pub compact_prompt: Option<String>,
}

impl ContextConfig {
    /// Defaults from spec §14 with the given model metadata.
    pub fn with_meta(meta: ModelMeta) -> ContextConfig {
        ContextConfig {
            meta,
            auto_compact_percent: 90,
            tool_output_max_bytes: 10_000,
            compact_prompt: None,
        }
    }

    pub async fn load(store: &Store, meta: ModelMeta) -> ContextConfig {
        let percent =
            crate::settings::usize_setting(store, "context.auto_compact_percent", 90).await as u64;
        let budget =
            crate::settings::usize_setting(store, "context.tool_output_max_bytes", 10_000).await;
        let prompt = store
            .get_setting("context.compact_prompt")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        ContextConfig {
            meta,
            auto_compact_percent: percent.clamp(50, 95),
            tool_output_max_bytes: budget,
            compact_prompt: prompt,
        }
    }
}

pub struct ContextStatus {
    pub active_tokens: u64,
    pub context_window: u64,
    pub usable_window: u64,
    pub percent_left: u8,
    pub needs_compaction: bool,
}

pub struct ContextManager {
    pub(super) ledger: Ledger,
    cfg: ContextConfig,
    tokens: TokenState,
}

impl ContextManager {
    pub async fn load(
        store: Arc<Store>,
        session_pk: &str,
        cfg: ContextConfig,
    ) -> anyhow::Result<ContextManager> {
        let ledger = Ledger::load(store, session_pk).await?;
        let mut cm = ContextManager {
            ledger,
            cfg,
            tokens: TokenState::default(),
        };
        // Resume: the reloaded history is "local" until the next server truth.
        cm.tokens.local_appended = cm.ledger.messages().iter().map(estimate_tokens).sum();
        Ok(cm)
    }

    pub fn ephemeral(session_pk: &str, cfg: ContextConfig) -> ContextManager {
        ContextManager {
            ledger: Ledger::ephemeral(session_pk),
            cfg,
            tokens: TokenState::default(),
        }
    }

    pub fn cfg(&self) -> &ContextConfig {
        &self.cfg
    }

    pub fn is_empty(&self) -> bool {
        self.ledger.is_empty()
    }

    pub fn window_number(&self) -> u32 {
        self.ledger.window_number()
    }

    pub async fn append_user(&mut self, content: Value) -> anyhow::Result<()> {
        self.tokens.local_appended += estimate_tokens(&content);
        self.ledger.append_user(content).await
    }

    pub async fn append_assistant(&mut self, content: Value) -> anyhow::Result<()> {
        self.tokens.local_appended += estimate_tokens(&content);
        self.ledger.append_assistant(content).await
    }

    /// Append tool_result blocks as one user turn, truncating each result's
    /// string content to the ingestion budget first (spec §6.2). The ledger
    /// stores the truncated form — exactly what the model sees.
    pub async fn append_tool_results(&mut self, results: Vec<Value>) -> anyhow::Result<()> {
        let budget = self.cfg.tool_output_max_bytes;
        let truncated: Vec<Value> = results
            .into_iter()
            .map(|mut r| {
                if let Some(s) = r.get("content").and_then(|c| c.as_str()) {
                    if s.len() > budget {
                        r["content"] = Value::String(truncate_for_context(s, budget));
                    }
                }
                r
            })
            .collect();
        self.append_user(Value::Array(truncated)).await
    }

    /// Request messages: the history — first sanitized so any dangling
    /// `tool_use` from an interrupted prior turn is repaired (Anthropic 400s
    /// an unpaired `tool_use`; see [`Ledger::messages_for_request`]) — then
    /// with a moving `cache_control` breakpoint on the final message's last
    /// block when the model supports caching. The sanitize runs first so the
    /// cache breakpoint lands on the actual last block of the sent body.
    pub fn messages_for_request(&self) -> Vec<Value> {
        let mut msgs = self.ledger.messages_for_request();
        if self.cfg.meta.supports_prompt_cache {
            if let Some(last) = msgs.last_mut() {
                if let Some(blocks) = last["content"].as_array_mut() {
                    if let Some(block) = blocks.last_mut() {
                        block["cache_control"] = serde_json::json!({"type": "ephemeral"});
                    }
                }
            }
        }
        msgs
    }

    pub fn set_baseline(&mut self, system: &str, tools: &[Value]) {
        let tools_len: usize = tools
            .iter()
            .map(|t| serde_json::to_string(t).map(|s| s.len()).unwrap_or(0))
            .sum();
        self.tokens.baseline = ((system.len() + tools_len) / 4) as u64;
    }

    pub fn observe_message_start(&mut self, message: &Value) {
        if let Some(usage) = message.get("usage") {
            self.tokens.observe_start_usage(usage);
        }
    }

    pub fn observe_message_delta(
        &mut self,
        output: i64,
        input: Option<i64>,
        cache_read: Option<i64>,
        cache_creation: Option<i64>,
    ) {
        self.tokens
            .observe_delta_usage(output, input, cache_read, cache_creation);
    }

    pub fn commit_response(&mut self) {
        self.tokens.commit();
    }

    /// The last committed response's cache-read token count (server truth) —
    /// for the `ContextUsage` event; not tracked between commits.
    pub fn last_cache_read(&self) -> u64 {
        self.tokens.cache_read
    }

    /// The last committed response's output token count (server truth).
    pub fn last_output(&self) -> u64 {
        self.tokens.last_output
    }

    /// On a context-overflow error: pin the indicator to 0% so the next
    /// turn's pre-check deterministically compacts (spec §12).
    pub fn mark_full(&mut self) {
        self.tokens.last_server_total = Some(self.cfg.meta.usable_window());
        self.tokens.local_appended = 0;
    }

    /// Resume-seed the token state from a persisted `active_tokens` value
    /// (spec §12 continued): each turn builds a fresh `ContextManager` whose
    /// estimate comes only from re-summing the reloaded history, which can
    /// badly undercount the true active total right after `mark_full` pinned
    /// it to the full window — the in-memory pin never survives past that
    /// turn. Applied only when `active` exceeds the current estimate, so a
    /// normal resume (where the reload estimate is already accurate) is a
    /// no-op; called before the next turn's `needs_compaction` check so an
    /// overflow reliably drives a pre-turn compaction instead of looping
    /// forever on an undercounted estimate.
    pub fn seed_active_tokens(&mut self, active: u64) {
        if active > self.tokens.active() {
            self.tokens.last_server_total = Some(active);
            self.tokens.local_appended = 0;
        }
    }

    pub fn status(&self) -> ContextStatus {
        let meta = &self.cfg.meta;
        let active = self.tokens.active();
        let usable = meta.usable_window();
        let limit = meta.auto_compact_limit(self.cfg.auto_compact_percent);
        let baseline = self.tokens.baseline.min(usable.saturating_sub(1));
        // Baseline subtracted from both sides: numerator simplifies to
        // usable − active; a fresh session (active ≈ baseline) reads ~100%.
        let percent_left = if usable <= baseline {
            0
        } else {
            (usable.saturating_sub(active) * 100 / (usable - baseline)).min(100) as u8
        };
        ContextStatus {
            active_tokens: active,
            context_window: meta.context_window,
            usable_window: usable,
            percent_left,
            needs_compaction: !self.ledger.is_empty() && (active >= limit || active >= usable),
        }
    }
}

/// Classify provider error text as context-window overflow. Applied to both
/// pre-stream anyhow errors and mid-stream Error events (spec §12).
pub fn is_context_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("prompt is too long")
        || m.contains("context_length_exceeded")
        || m.contains("maximum context length")
        || m.contains("exceed context limit")
        || m.contains("exceeds the context window")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::model_meta::ModelMeta;
    use serde_json::json;

    fn meta() -> ModelMeta {
        ModelMeta {
            context_window: 100_000,
            max_output_tokens: 8_192,
            supports_prompt_cache: true,
            supports_reasoning: false,
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
        }
    }

    #[test]
    fn fresh_session_reads_full_and_grows_with_appends() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        cm.set_baseline("system prompt ".repeat(100).as_str(), &[]);
        let st = cm.status();
        assert!(
            st.percent_left >= 99,
            "baseline-only reads ~100%, got {}",
            st.percent_left
        );
        assert!(!st.needs_compaction);
    }

    #[tokio::test]
    async fn server_usage_overrides_local_estimate_and_resets_local() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        cm.append_user(json!([{"type":"text","text":"hi"}]))
            .await
            .unwrap();
        // Anthropic shape: input on message_start, output on message_delta.
        cm.observe_message_start(
            &json!({"usage":{"input_tokens":40_000,"cache_read_input_tokens":10_000}}),
        );
        cm.observe_message_delta(2_000, None, None, None);
        cm.commit_response();
        let st = cm.status();
        assert_eq!(st.active_tokens, 52_000); // 40k + 10k cache + 2k out
                                              // OpenAI shape: input arrives on the terminal delta and wins.
        cm.observe_message_delta(1_000, Some(60_000), None, None);
        cm.commit_response();
        assert_eq!(cm.status().active_tokens, 61_000);
    }

    #[tokio::test]
    async fn needs_compaction_at_threshold_and_mark_full() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        // needs_compaction requires a non-empty history.
        cm.append_user(json!([{"type":"text","text":"hi"}]))
            .await
            .unwrap();
        cm.observe_message_start(&json!({"usage":{"input_tokens":89_000}}));
        cm.observe_message_delta(2_000, None, None, None);
        cm.commit_response();
        assert!(cm.status().needs_compaction, "91k >= 90% of 100k");
        cm.mark_full();
        assert_eq!(cm.status().percent_left, 0);
    }

    #[tokio::test]
    async fn seed_active_tokens_only_applies_when_it_exceeds_the_estimate() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        cm.set_baseline("short system", &[]);
        // needs_compaction requires a non-empty history.
        cm.append_user(json!([{"type":"text","text":"hi"}]))
            .await
            .unwrap();
        let before = cm.status().active_tokens;
        // A seed below the current estimate must not regress the state.
        cm.seed_active_tokens(before.saturating_sub(1));
        assert_eq!(cm.status().active_tokens, before);
        assert!(!cm.status().needs_compaction);
        // A seed above the current estimate (the overflow-resume case) wins.
        cm.seed_active_tokens(90_000);
        assert_eq!(cm.status().active_tokens, 90_000);
        assert!(cm.status().needs_compaction, "90k >= 90% of 100k");
    }

    #[tokio::test]
    async fn tool_results_are_truncated_at_ingestion() {
        let cfg = ContextConfig {
            tool_output_max_bytes: 200,
            ..ContextConfig::with_meta(meta())
        };
        let mut cm = ContextManager::ephemeral("s", cfg);
        let big = "x".repeat(5_000);
        cm.append_tool_results(vec![json!({
            "type":"tool_result","tool_use_id":"t1","content": big,"is_error":false
        })])
        .await
        .unwrap();
        let msgs = cm.messages_for_request();
        let content = msgs[0]["content"][0]["content"].as_str().unwrap();
        assert!(content.len() < 1_000);
        assert!(content.starts_with("Warning: truncated output"));
    }

    #[test]
    fn messages_for_request_injects_cache_control_on_last_block() {
        let mut cm = ContextManager::ephemeral("s", ContextConfig::with_meta(meta()));
        futures::executor::block_on(async {
            cm.append_user(json!([{"type":"text","text":"one"}]))
                .await
                .unwrap();
            cm.append_assistant(json!([{"type":"text","text":"two"}]))
                .await
                .unwrap();
        });
        let msgs = cm.messages_for_request();
        assert_eq!(msgs[1]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(
            msgs[0]["content"][0].get("cache_control").is_none(),
            "only the last block"
        );
    }

    #[test]
    fn overflow_classifier_matches_known_provider_messages() {
        assert!(is_context_overflow(
            "prompt is too long: 210000 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow(
            "This model's maximum context length is 128000 tokens"
        ));
        assert!(is_context_overflow("error code: context_length_exceeded"));
        assert!(is_context_overflow(
            "input length and `max_tokens` exceed context limit"
        ));
        assert!(!is_context_overflow("rate limit exceeded"));
    }
}
