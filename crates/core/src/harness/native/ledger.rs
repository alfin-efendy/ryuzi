//! The provider-turn ledger: the model-faithful Anthropic `messages` array,
//! persisted to the `provider_turns` table so history survives restarts and
//! can be replayed on resume.

use crate::domain::NewProviderTurn;
use crate::store::Store;
use serde_json::{json, Value};
use std::sync::Arc;

/// An in-memory conversation history for one session, optionally persisted to
/// the `provider_turns` table. Sub-agents use an ephemeral (unpersisted)
/// ledger so their internal turns don't pollute the parent session's history.
pub struct Ledger {
    session_pk: String,
    /// `None` for an ephemeral (sub-agent) ledger.
    store: Option<Arc<Store>>,
    /// Anthropic messages: `{role, content:[...]}` objects.
    turns: Vec<Value>,
}

impl Ledger {
    /// Load a session's history from the store (empty for a new session).
    pub async fn load(store: Arc<Store>, session_pk: &str) -> anyhow::Result<Ledger> {
        let rows = store.list_provider_turns(session_pk).await?;
        let turns = rows
            .into_iter()
            .map(|t| json!({ "role": t.role, "content": t.payload }))
            .collect();
        Ok(Ledger {
            session_pk: session_pk.to_string(),
            store: Some(store),
            turns,
        })
    }

    /// A fresh, unpersisted ledger (for sub-agent runs).
    pub fn ephemeral(session_pk: &str) -> Ledger {
        Ledger {
            session_pk: session_pk.to_string(),
            store: None,
            turns: Vec::new(),
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
        if let Some(store) = &self.store {
            store
                .insert_provider_turn(NewProviderTurn::new(
                    self.session_pk.clone(),
                    role,
                    content.clone(),
                ))
                .await?;
        }
        self.turns.push(json!({ "role": role, "content": content }));
        Ok(())
    }

    /// The Anthropic `messages` array for a provider request.
    pub fn messages(&self) -> &[Value] {
        &self.turns
    }

    /// Whether any turns have been recorded.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
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
    async fn reload_replays_identically() {
        let store = store().await;
        {
            let mut ledger = Ledger::load(store.clone(), "s1").await.unwrap();
            ledger
                .append_user(json!([{"type": "text", "text": "one"}]))
                .await
                .unwrap();
            ledger
                .append_assistant(json!([{"type": "text", "text": "two"}]))
                .await
                .unwrap();
        }
        let reloaded = Ledger::load(store.clone(), "s1").await.unwrap();
        assert_eq!(reloaded.messages().len(), 2);
        assert_eq!(reloaded.messages()[0]["content"][0]["text"], "one");
        assert_eq!(reloaded.messages()[1]["role"], "assistant");
    }
}
