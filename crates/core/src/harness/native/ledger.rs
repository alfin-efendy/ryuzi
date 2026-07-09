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

    /// The Anthropic `messages` array for a provider request, with any
    /// dangling `tool_use` repaired — see [`sanitize_tool_pairing`]. Always
    /// use THIS (not [`Ledger::messages`]) when assembling a provider request:
    /// a turn interrupted between the assistant `tool_use` append and the
    /// `tool_result` append (cancel, crash, parked approval) leaves durable
    /// rows that would otherwise 400 every later Anthropic request. The
    /// durable rows are untouched — this is a per-request projection.
    pub fn messages_for_request(&self) -> Vec<Value> {
        sanitize_tool_pairing(&self.turns)
    }

    /// Whether any turns have been recorded.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Number of turns (messages) currently projected.
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Compact the in-memory projection: replace `turns[0..=boundary]` with a
    /// single user message carrying `summary` plus the boundary turn's own
    /// text, keeping every turn after `boundary`. This bounds the request size
    /// while leaving the durable `provider_turns` history untouched (a resume
    /// reloads the full history and re-compacts as needed). The boundary must
    /// be a real user-text turn so the resulting history stays provider-valid
    /// (user, assistant, …).
    pub fn compact_at(&mut self, boundary: usize, summary: &str) {
        if boundary >= self.turns.len() {
            return;
        }
        let boundary_text = self.turns[boundary]["content"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find_map(|b| b.get("text").and_then(|t| t.as_str()))
            })
            .unwrap_or_default()
            .to_string();
        let merged = json!({
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!(
                    "[Summary of earlier conversation]\n{summary}\n\n[Continuing request]\n{boundary_text}"
                )
            }]
        });
        let rest: Vec<Value> = self.turns.split_off(boundary + 1);
        self.turns = vec![merged];
        self.turns.extend(rest);
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
