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

    /// Drop the oldest message, then keep dropping until the front is a valid
    /// history start: a user turn whose first block is not a tool_result.
    /// (Compaction overflow-recovery — spec §7.2.)
    pub fn drop_oldest(&mut self) {
        if self.turns.is_empty() {
            return;
        }
        self.turns.remove(0);
        while let Some(front) = self.turns.first() {
            let role_ok = front.msg["role"] == "user";
            let first_block_type = front.msg["content"][0]["type"].as_str().unwrap_or("");
            if role_ok && first_block_type != "tool_result" {
                break;
            }
            self.turns.remove(0);
        }
    }

    /// Install a compacted replacement history. Persistent ledgers write a
    /// durable checkpoint so `load` never replays the replaced turns again;
    /// the provider_turns rows stay untouched (append-only audit record).
    pub async fn replace_all(&mut self, replacement: Vec<Value>) -> anyhow::Result<u32> {
        self.window_number += 1;
        let boundary_seq = self.turns.last().map(|t| t.seq).unwrap_or(0);
        if let Some(store) = &self.store {
            store
                .insert_context_checkpoint(
                    &self.session_pk,
                    boundary_seq,
                    self.window_number as i64,
                    &Value::Array(replacement.clone()),
                )
                .await?;
        }
        self.turns = replacement
            .into_iter()
            .map(|msg| Turn {
                seq: boundary_seq,
                msg,
            })
            .collect();
        Ok(self.window_number)
    }

    /// Replace the in-memory projection without touching persistence — used
    /// by compaction's pre-trim, which is immediately followed by
    /// `replace_all` (the durable step). Seqs are preserved positionally.
    pub(super) fn overwrite_in_memory(&mut self, msgs: Vec<Value>) {
        for (turn, msg) in self.turns.iter_mut().zip(msgs) {
            turn.msg = msg;
        }
    }
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

    #[tokio::test]
    async fn drop_oldest_preserves_a_valid_history_start() {
        let mut ledger = Ledger::ephemeral("s");
        ledger
            .append_user(json!([{"type":"text","text":"u0"}]))
            .await
            .unwrap();
        ledger
            .append_assistant(json!([{"type":"tool_use","id":"t1","name":"bash","input":{}}]))
            .await
            .unwrap();
        ledger
            .append_user(
                json!([{"type":"tool_result","tool_use_id":"t1","content":"out","is_error":false}]),
            )
            .await
            .unwrap();
        ledger
            .append_assistant(json!([{"type":"text","text":"a1"}]))
            .await
            .unwrap();
        ledger
            .append_user(json!([{"type":"text","text":"u1"}]))
            .await
            .unwrap();
        // Dropping u0 must also drop the now-orphaned assistant tool_use AND its
        // tool_result AND the trailing assistant, landing on the next real user turn.
        ledger.drop_oldest();
        assert_eq!(ledger.messages()[0]["role"], "user");
        assert_eq!(ledger.messages()[0]["content"][0]["type"], "text");
        assert_eq!(ledger.messages()[0]["content"][0]["text"], "u1");
    }
}
