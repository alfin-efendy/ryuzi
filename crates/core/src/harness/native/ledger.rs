//! The provider-turn ledger: the model-faithful Anthropic `messages` array,
//! persisted to the `provider_turns` table so history survives restarts and
//! can be replayed on resume. Compaction checkpoints (`context_checkpoints`)
//! let a reload skip straight to the latest compacted replacement plus the
//! turns appended after it, instead of replaying the full session history.

use crate::domain::NewProviderTurn;
use crate::store::Store;
use serde_json::{json, Value};
use std::sync::Arc;

struct Turn {
    /// provider_turns seq for persisted appends; the checkpoint boundary_seq
    /// for turns seeded from a checkpoint; a local counter for ephemeral.
    seq: i64,
    msg: Value,
}

/// An in-memory conversation history for one session, optionally persisted to
/// the `provider_turns` table. Sub-agents use an ephemeral (unpersisted)
/// ledger so their internal turns don't pollute the parent session's history.
pub struct Ledger {
    session_pk: String,
    /// `None` for an ephemeral (sub-agent) ledger.
    store: Option<Arc<Store>>,
    turns: Vec<Turn>,
    window_number: u32,
}

impl Ledger {
    /// Load a session's history: the latest compaction checkpoint's
    /// replacement (if any) plus every provider turn after its boundary.
    pub async fn load(store: Arc<Store>, session_pk: &str) -> anyhow::Result<Ledger> {
        let (mut turns, window_number, after) =
            match store.latest_context_checkpoint(session_pk).await? {
                Some(ck) => {
                    let seeded: Vec<Turn> = ck
                        .payload
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|msg| Turn {
                            seq: ck.boundary_seq,
                            msg,
                        })
                        .collect();
                    (seeded, ck.window_number as u32, ck.boundary_seq)
                }
                None => (Vec::new(), 0, 0),
            };
        for t in store.list_provider_turns_after(session_pk, after).await? {
            turns.push(Turn {
                seq: t.seq,
                msg: json!({ "role": t.role, "content": t.payload }),
            });
        }
        Ok(Ledger {
            session_pk: session_pk.to_string(),
            store: Some(store),
            turns,
            window_number,
        })
    }

    /// A fresh, unpersisted ledger (for sub-agent runs).
    pub fn ephemeral(session_pk: &str) -> Ledger {
        Ledger {
            session_pk: session_pk.to_string(),
            store: None,
            turns: Vec::new(),
            window_number: 0,
        }
    }

    /// A fresh, unpersisted ledger pre-loaded with `messages` verbatim as its
    /// starting turns (Task 9's review-fork cache-parity replay): each
    /// message is installed byte-for-byte, so a later `messages_for_request`
    /// call reproduces them unchanged (aside from `sanitize_tool_pairing`'s
    /// dangling-`tool_use` repair, a no-op on an already-valid captured
    /// prefix) — any `cache_control` marker a seeded message already carries
    /// on its own content survives untouched for as long as it stays out of
    /// the LAST position, exactly like a live session's moving breakpoint.
    pub fn seed_projected(session_pk: &str, messages: Vec<Value>) -> Ledger {
        let turns = messages
            .into_iter()
            .enumerate()
            .map(|(i, msg)| Turn {
                seq: i as i64 + 1,
                msg,
            })
            .collect();
        Ledger {
            session_pk: session_pk.to_string(),
            store: None,
            turns,
            window_number: 0,
        }
    }

    /// Append a user turn (content = Anthropic content-block array).
    pub async fn append_user(&mut self, content: Value) -> anyhow::Result<()> {
        self.append("user", content).await
    }

    /// Append an assistant turn.
    pub async fn append_assistant(&mut self, content: Value) -> anyhow::Result<()> {
        self.append("assistant", content).await
    }

    async fn append(&mut self, role: &str, content: Value) -> anyhow::Result<()> {
        let seq = match &self.store {
            Some(store) => {
                store
                    .insert_provider_turn(NewProviderTurn::new(
                        self.session_pk.clone(),
                        role,
                        content.clone(),
                    ))
                    .await?
            }
            None => self.turns.last().map(|t| t.seq).unwrap_or(0) + 1,
        };
        self.turns.push(Turn {
            seq,
            msg: json!({ "role": role, "content": content }),
        });
        Ok(())
    }

    /// The Anthropic `messages` array for a provider request.
    pub fn messages(&self) -> Vec<Value> {
        self.turns.iter().map(|t| t.msg.clone()).collect()
    }

    /// The Anthropic `messages` array for a provider request, with any
    /// dangling `tool_use` repaired — see [`sanitize_tool_pairing`]. Always
    /// use THIS (not [`Ledger::messages`]) when assembling a provider request:
    /// a turn interrupted between the assistant `tool_use` append and the
    /// `tool_result` append (cancel, crash, parked approval) leaves durable
    /// rows that would otherwise 400 every later Anthropic request. The
    /// durable rows are untouched — this is a per-request projection.
    pub fn messages_for_request(&self) -> Vec<Value> {
        sanitize_tool_pairing(&self.messages())
    }

    /// Whether any turns have been recorded.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Number of turns (messages) currently projected.
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// The compaction window this ledger is currently in (0 = never compacted).
    pub fn window_number(&self) -> u32 {
        self.window_number
    }

    /// Install a compacted replacement history. Persistent ledgers write a
    /// durable checkpoint so `load` never replays the replaced turns again;
    /// the provider_turns rows stay untouched (append-only audit record).
    ///
    /// `window_number` only advances once the checkpoint insert actually
    /// succeeds: computing the next number up front (rather than mutating
    /// `self.window_number` before the fallible write) keeps numbering
    /// gap-free if the insert fails — a retried compaction reuses the same
    /// number instead of skipping one.
    pub async fn replace_all(&mut self, replacement: Vec<Value>) -> anyhow::Result<u32> {
        let next = self.window_number + 1;
        let boundary_seq = self.turns.last().map(|t| t.seq).unwrap_or(0);
        if let Some(store) = &self.store {
            store
                .insert_context_checkpoint(
                    &self.session_pk,
                    boundary_seq,
                    next as i64,
                    &Value::Array(replacement.clone()),
                )
                .await?;
        }
        self.window_number = next;
        self.turns = replacement
            .into_iter()
            .map(|msg| Turn {
                seq: boundary_seq,
                msg,
            })
            .collect();
        Ok(self.window_number)
    }
}

/// Repair `tool_use`/`tool_result` pairing: every `tool_use` id in an
/// assistant message must have a matching `tool_result` in the immediately
/// following user message, or Anthropic rejects the request with a 400.
/// Missing results are synthesized as `is_error` "interrupted" blocks —
/// prepended to the next user message when there is one (tool_result blocks
/// must lead its content), or appended as a standalone user turn at the tail.
pub fn sanitize_tool_pairing(messages: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len() + 1);
    let mut i = 0;
    while i < messages.len() {
        let dangling = missing_tool_results(&messages[i], messages.get(i + 1));
        out.push(messages[i].clone());
        i += 1;
        if dangling.is_empty() {
            continue;
        }
        let synthesized: Vec<Value> = dangling
            .iter()
            .map(|id| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": "interrupted",
                    "is_error": true,
                })
            })
            .collect();
        match messages.get(i) {
            // Fold the repairs into the front of the immediately-following
            // user message (the normal poisoned shape: a user prompt landed
            // right after the dangling tool_use).
            Some(next) if next["role"] == "user" && next["content"].is_array() => {
                let mut fixed = next.clone();
                let arr = fixed["content"].as_array_mut().expect("checked is_array");
                for (offset, block) in synthesized.into_iter().enumerate() {
                    arr.insert(offset, block);
                }
                out.push(fixed);
                i += 1; // the next message was consumed (repaired) here
            }
            // Tail of history (or a malformed neighbor): a standalone user
            // turn carrying only the repairs.
            _ => out.push(json!({ "role": "user", "content": synthesized })),
        }
    }
    out
}

/// The `tool_use` ids in `msg` (an assistant turn) that have NO matching
/// `tool_result` block in `next`.
fn missing_tool_results(msg: &Value, next: Option<&Value>) -> Vec<String> {
    if msg["role"] != "assistant" {
        return Vec::new();
    }
    let Some(blocks) = msg["content"].as_array() else {
        return Vec::new();
    };
    let uses: Vec<String> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| b["id"].as_str().map(str::to_string))
        .collect();
    if uses.is_empty() {
        return Vec::new();
    }
    let results: std::collections::HashSet<String> = next
        .filter(|n| n["role"] == "user")
        .and_then(|n| n["content"].as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "tool_result")
                .filter_map(|b| b["tool_use_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    uses.into_iter()
        .filter(|id| !results.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Arc<Store> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        Arc::new(Store::open(tmp.path()).await.unwrap())
    }

    #[tokio::test]
    async fn append_builds_messages_and_persists() {
        let store = store().await;
        let mut ledger = Ledger::load(store.clone(), "s1").await.unwrap();
        assert!(ledger.is_empty());
        ledger
            .append_user(json!([{"type": "text", "text": "hi"}]))
            .await
            .unwrap();
        ledger
            .append_assistant(json!([{"type": "text", "text": "hello"}]))
            .await
            .unwrap();
        assert_eq!(ledger.messages().len(), 2);
        assert_eq!(ledger.messages()[0]["role"], "user");
        assert_eq!(ledger.messages()[1]["content"][0]["text"], "hello");
    }

    #[tokio::test]
    async fn replace_all_checkpoints_and_reload_is_o_tail() {
        let store = store().await;
        let mut ledger = Ledger::load(store.clone(), "s1").await.unwrap();
        for i in 0..4 {
            ledger
                .append_user(json!([{"type":"text","text": format!("u{i}")}]))
                .await
                .unwrap();
            ledger
                .append_assistant(json!([{"type":"text","text": format!("a{i}")}]))
                .await
                .unwrap();
        }
        let replacement = vec![
            json!({"role":"user","content":[{"type":"text","text":"[Ryuzi context summary]\nS"}]}),
        ];
        let window = ledger.replace_all(replacement).await.unwrap();
        assert_eq!(window, 1);
        assert_eq!(ledger.len(), 1);
        // Post-compaction appends land after the checkpoint boundary.
        ledger
            .append_user(json!([{"type":"text","text":"after"}]))
            .await
            .unwrap();

        // Reload: replacement + tail only — NOT the 8 original turns.
        let reloaded = Ledger::load(store.clone(), "s1").await.unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.messages()[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("summary"));
        assert_eq!(reloaded.messages()[1]["content"][0]["text"], "after");
        assert_eq!(reloaded.window_number(), 1);

        // A second compaction increments the window.
        let mut reloaded = reloaded;
        let w2 = reloaded
            .replace_all(vec![
                json!({"role":"user","content":[{"type":"text","text":"S2"}]}),
            ])
            .await
            .unwrap();
        assert_eq!(w2, 2);
    }

    #[tokio::test]
    async fn replace_all_leaves_window_number_and_turns_unchanged_on_a_failed_insert() {
        let store = store().await;
        let mut ledger = Ledger::load(store.clone(), "s1").await.unwrap();
        ledger
            .append_user(json!([{"type":"text","text":"u0"}]))
            .await
            .unwrap();
        assert_eq!(ledger.window_number(), 0);
        let before_len = ledger.len();

        // Force the checkpoint insert to fail so we can assert the counter
        // and turns are left untouched rather than advancing/replacing ahead
        // of the (failed) durable write.
        store
            .with_conn(|c| c.execute("DROP TABLE context_checkpoints", []).map(|_| ()))
            .await
            .unwrap();

        let err = ledger
            .replace_all(vec![
                json!({"role":"user","content":[{"type":"text","text":"S"}]}),
            ])
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .to_lowercase()
            .contains("context_checkpoints"));
        assert_eq!(
            ledger.window_number(),
            0,
            "window_number must not advance on a failed insert"
        );
        assert_eq!(
            ledger.len(),
            before_len,
            "turns must not be replaced on a failed insert"
        );
    }

    #[tokio::test]
    async fn ephemeral_ledger_never_touches_the_store() {
        let mut ledger = Ledger::ephemeral("sub");
        ledger
            .append_user(json!([{"type":"text","text":"x"}]))
            .await
            .unwrap();
        let w = ledger
            .replace_all(vec![
                json!({"role":"user","content":[{"type":"text","text":"s"}]}),
            ])
            .await
            .unwrap();
        assert_eq!(w, 1);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn sanitize_appends_a_repair_turn_for_a_dangling_tail_tool_use() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "do it"}]}),
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "running"},
                {"type": "tool_use", "id": "tu-1", "name": "bash", "input": {"command": "ls"}},
            ]}),
        ];
        let fixed = sanitize_tool_pairing(&messages);
        assert_eq!(fixed.len(), 3, "a repair user turn is appended at the tail");
        assert_eq!(fixed[2]["role"], "user");
        assert_eq!(fixed[2]["content"][0]["type"], "tool_result");
        assert_eq!(fixed[2]["content"][0]["tool_use_id"], "tu-1");
        assert_eq!(fixed[2]["content"][0]["is_error"], true);
        assert_eq!(fixed[2]["content"][0]["content"], "interrupted");
    }

    #[test]
    fn sanitize_folds_missing_results_into_the_following_user_turn() {
        // Poisoned mid-history: a user text turn (next prompt / boot-resume
        // nudge) was appended right after the dangling tool_use.
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "start"}]}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu-1", "name": "bash", "input": {}},
            ]}),
            json!({"role": "user", "content": [{"type": "text", "text": "hello again"}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}),
        ];
        let fixed = sanitize_tool_pairing(&messages);
        assert_eq!(fixed.len(), 4, "repaired in place, no new message inserted");
        let content = fixed[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tu-1");
        assert_eq!(content[0]["is_error"], true);
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "hello again");
    }

    #[test]
    fn sanitize_synthesizes_only_the_missing_ids_of_a_partial_pairing() {
        let messages = vec![
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu-1", "name": "bash", "input": {}},
                {"type": "tool_use", "id": "tu-2", "name": "read", "input": {}},
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu-2", "content": "ok", "is_error": false},
            ]}),
        ];
        let fixed = sanitize_tool_pairing(&messages);
        let content = fixed[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "tu-1", "synthesized, prepended");
        assert_eq!(content[0]["is_error"], true);
        assert_eq!(
            content[1]["tool_use_id"], "tu-2",
            "original result untouched"
        );
        assert_eq!(content[1]["is_error"], false);
    }

    #[test]
    fn sanitize_leaves_a_healthy_history_untouched() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "go"}]}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu-1", "name": "bash", "input": {}},
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu-1", "content": "done", "is_error": false},
            ]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "done"}]}),
        ];
        assert_eq!(sanitize_tool_pairing(&messages), messages);
    }

    #[tokio::test]
    async fn messages_for_request_repairs_a_reloaded_poisoned_session() {
        let store = store().await;
        {
            let mut ledger = Ledger::load(store.clone(), "s1").await.unwrap();
            ledger
                .append_user(json!([{"type": "text", "text": "do it"}]))
                .await
                .unwrap();
            ledger
                .append_assistant(json!([
                    {"type": "tool_use", "id": "tu-9", "name": "bash", "input": {}}
                ]))
                .await
                .unwrap();
            // Interrupted here: no tool_result turn was ever appended.
        }
        let reloaded = Ledger::load(store.clone(), "s1").await.unwrap();
        // Durable rows replay verbatim ...
        assert_eq!(reloaded.messages().len(), 2);
        // ... but the request projection is repaired.
        let request = reloaded.messages_for_request();
        assert_eq!(request.len(), 3);
        assert_eq!(request[2]["role"], "user");
        assert_eq!(request[2]["content"][0]["tool_use_id"], "tu-9");
        assert_eq!(request[2]["content"][0]["is_error"], true);
    }
}
