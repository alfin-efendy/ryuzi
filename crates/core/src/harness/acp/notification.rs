//! Notification sink: persists ACP `SessionNotification` stream updates to the
//! message store and fans out `CoreEvent::Message` to broadcast subscribers.
//!
//! Task 3 / Spec 3A: receives `AgentMessageChunk`, `AgentThoughtChunk`,
//! `ToolCall`, and `ToolCallUpdate` notifications and writes them through
//! `Store::insert_message` / `Store::update_tool_call`. All other update
//! variants are silently skipped.

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCallStatus, ToolKind,
};
use tokio::sync::broadcast;

use crate::domain::{CoreEvent, NewMessage};
use crate::store::Store;

/// Maps a [`ToolKind`] to its canonical string label stored in the DB.
fn tool_kind_str(kind: &ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        _ => "other",
    }
}

/// Maps a [`ToolCallStatus`] to its canonical string label stored in the DB.
fn status_str(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "pending",
    }
}

/// Returns `true` for statuses that represent a terminal (final) state.
fn is_terminal(status: &ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
}

/// Sink that stores incoming ACP notifications into the [`Store`] and emits
/// the corresponding [`CoreEvent::Message`] on the broadcast channel.
pub struct NotificationSink {
    /// Ryuzi's own DB primary key for the session. All messages persisted by
    /// this sink are keyed under this value, NOT the ACP-assigned session id.
    pub session_pk: String,
    /// Persistent message store.
    pub store: Arc<Store>,
    /// Broadcast channel: new subscribers see future events only.
    pub events: broadcast::Sender<CoreEvent>,
}

/// Internal representation of a decoded, actionable ACP update.
///
/// Kept private; `handle` converts notifications to this before acting.
enum AcpUpdate {
    /// A text chunk for the assistant's visible message.
    AgentText { session_pk: String, text: String },
    /// A text chunk for the agent's internal thought.
    AgentThought { session_pk: String, text: String },
    /// A new tool call being announced.
    ToolCall {
        session_pk: String,
        id: String,
        title: String,
        kind_str: &'static str,
        initial_status: &'static str,
        raw_input: Option<serde_json::Value>,
    },
    /// A terminal-status update to an existing tool call row.
    ToolCallDone {
        session_pk: String,
        id: String,
        status: &'static str,
        output_payload: serde_json::Value,
    },
    /// Any variant we don't handle yet.
    Skip,
}

/// Decode a [`SessionNotification`] into an [`AcpUpdate`] without any I/O.
///
/// `session_pk` is ryuzi's own DB primary key; it must be provided by the
/// caller (from `NotificationSink::session_pk`) rather than derived from
/// `notification.session_id`, which is the ACP-assigned identifier and would
/// cause the frontend's `list_messages(session_pk)` query to miss all rows.
fn decode(notification: SessionNotification, session_pk: &str) -> AcpUpdate {
    let session_pk = session_pk.to_owned();

    match notification.update {
        // --- Agent message text chunk ---
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(tc) => AcpUpdate::AgentText {
                session_pk,
                text: tc.text,
            },
            _ => AcpUpdate::Skip,
        },

        // --- Agent thought text chunk ---
        SessionUpdate::AgentThoughtChunk(chunk) => match chunk.content {
            ContentBlock::Text(tc) => AcpUpdate::AgentThought {
                session_pk,
                text: tc.text,
            },
            _ => AcpUpdate::Skip,
        },

        // --- New tool call ---
        SessionUpdate::ToolCall(tc) => {
            let id = tc.tool_call_id.0.to_string();
            let title = tc.title.clone();
            let kind = tool_kind_str(&tc.kind);
            let st = status_str(&tc.status);
            AcpUpdate::ToolCall {
                session_pk,
                id,
                title,
                kind_str: kind,
                initial_status: st,
                raw_input: tc.raw_input,
            }
        }

        // --- Tool call update ---
        SessionUpdate::ToolCallUpdate(update) => {
            let id = update.tool_call_id.0.to_string();
            match update.fields.status {
                Some(ref s) if is_terminal(s) => {
                    let st = status_str(s);
                    let output_payload = match update.fields.raw_output {
                        Some(v) => serde_json::json!({ "output": v }),
                        None => serde_json::json!({}),
                    };
                    AcpUpdate::ToolCallDone {
                        session_pk,
                        id,
                        status: st,
                        output_payload,
                    }
                }
                _ => AcpUpdate::Skip,
            }
        }

        _ => AcpUpdate::Skip,
    }
}

impl NotificationSink {
    /// Persist a `(role="system", block_type="status")` row and emit a real
    /// [`CoreEvent::Message`] with the returned DB seq.
    ///
    /// Used to record observable fs-write events (and any other client-side
    /// status events) through the same persist-then-emit path that ACP
    /// notifications use, so the frontend sees a real seq ≥ 1 (never −1).
    ///
    /// On store error → `tracing::warn!` + skip (same as the sink's rule).
    pub async fn record_status(&self, summary: String) {
        let payload = serde_json::json!({ "summary": summary });
        let msg = NewMessage::block(
            &self.session_pk,
            "system",
            "status",
            payload.clone(),
        );
        match self.store.insert_message(msg).await {
            Ok(seq) => {
                let _ = self.events.send(CoreEvent::Message {
                    session_pk: self.session_pk.clone(),
                    seq,
                    role: "system".into(),
                    block_type: "status".into(),
                    payload,
                    tool_call_id: None,
                    status: None,
                    tool_kind: None,
                });
            }
            Err(e) => {
                tracing::warn!("notification: failed to insert status message: {e}");
            }
        }
    }
}

/// Process one [`SessionNotification`], persisting to the store and emitting
/// a [`CoreEvent::Message`] on success.
///
/// - The `session_pk` key comes from `sink.session_pk` (ryuzi's DB primary
///   key), NOT from `notification.session_id` (the ACP-assigned identifier).
/// - Store errors are logged with [`tracing::warn!`] and swallowed; the
///   broadcast send failure (no subscribers) is also silently ignored.
pub async fn handle(notification: SessionNotification, sink: &NotificationSink) {
    match decode(notification, &sink.session_pk) {
        AcpUpdate::AgentText { session_pk, text } => {
            let msg = NewMessage::block(
                &session_pk,
                "assistant",
                "text",
                serde_json::json!({ "text": text }),
            );
            match sink.store.insert_message(msg).await {
                Ok(seq) => {
                    let _ = sink.events.send(CoreEvent::Message {
                        session_pk: session_pk.clone(),
                        seq,
                        role: "assistant".into(),
                        block_type: "text".into(),
                        payload: serde_json::json!({ "text": text }),
                        tool_call_id: None,
                        status: None,
                        tool_kind: None,
                    });
                }
                Err(e) => {
                    tracing::warn!("notification: failed to insert agent text message: {e}");
                }
            }
        }

        AcpUpdate::AgentThought { session_pk, text } => {
            let msg = NewMessage::block(
                &session_pk,
                "assistant",
                "thought",
                serde_json::json!({ "text": text }),
            );
            match sink.store.insert_message(msg).await {
                Ok(seq) => {
                    let _ = sink.events.send(CoreEvent::Message {
                        session_pk: session_pk.clone(),
                        seq,
                        role: "assistant".into(),
                        block_type: "thought".into(),
                        payload: serde_json::json!({ "text": text }),
                        tool_call_id: None,
                        status: None,
                        tool_kind: None,
                    });
                }
                Err(e) => {
                    tracing::warn!("notification: failed to insert agent thought message: {e}");
                }
            }
        }

        AcpUpdate::ToolCall {
            session_pk,
            id,
            title,
            kind_str,
            initial_status,
            raw_input,
        } => {
            let payload = serde_json::json!({ "name": title, "input": raw_input });
            let msg = NewMessage {
                session_pk: session_pk.clone(),
                role: "assistant".into(),
                block_type: "tool_call".into(),
                payload: payload.clone(),
                tool_call_id: Some(id.clone()),
                status: Some(initial_status.into()),
                tool_kind: Some(kind_str.into()),
            };
            match sink.store.insert_message(msg).await {
                Ok(seq) => {
                    let _ = sink.events.send(CoreEvent::Message {
                        session_pk: session_pk.clone(),
                        seq,
                        role: "assistant".into(),
                        block_type: "tool_call".into(),
                        payload,
                        tool_call_id: Some(id),
                        status: Some(initial_status.into()),
                        tool_kind: Some(kind_str.into()),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "notification: failed to insert tool_call row (id={id}): {e}"
                    );
                }
            }
        }

        AcpUpdate::ToolCallDone {
            session_pk,
            id,
            status,
            output_payload,
        } => {
            match sink
                .store
                .update_tool_call(&session_pk, &id, Some(status), &output_payload)
                .await
            {
                // Re-emit with the ORIGINAL row seq (identity-correct: the
                // frontend upserts by tool_call_id, not seq) and the MERGED
                // payload + persisted kind, so live completion renders with
                // name + input + output intact.
                Ok((seq, merged_payload, tool_kind)) => {
                    let _ = sink.events.send(CoreEvent::Message {
                        session_pk: session_pk.clone(),
                        seq,
                        role: "assistant".into(),
                        block_type: "tool_call".into(),
                        payload: merged_payload,
                        tool_call_id: Some(id),
                        status: Some(status.into()),
                        tool_kind,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "notification: failed to update tool_call row (id={id}): {e}"
                    );
                }
            }
        }

        AcpUpdate::Skip => {}
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn streamed_updates_persist_to_messages() {
        let (store, session_pk) =
            crate::harness::acp::testkit::run_prompt_and_collect().await;
        let msgs = store.list_messages(&session_pk).await.unwrap();

        // assistant text row
        assert!(
            msgs.iter().any(|m| m.role == "assistant"
                && m.block_type == "text"
                && m.payload["text"] == "working"),
            "expected assistant text row with text='working', got: {msgs:?}"
        );

        // tool_call row upserted to completed with correct id
        let tc = msgs
            .iter()
            .find(|m| m.block_type == "tool_call")
            .expect("tool_call row");
        assert_eq!(tc.status.as_deref(), Some("completed"));
        assert_eq!(tc.tool_call_id.as_deref(), Some("tc-1"));
    }

    #[tokio::test]
    async fn tool_completion_reemits_merged_payload_and_kind() {
        use crate::domain::CoreEvent;

        let (store, session_pk, events) =
            crate::harness::acp::testkit::run_via_harness_trait_collecting_events("hi").await;

        // The live completion event carries the MERGED payload and the
        // persisted kind (not just {output} / None as before).
        let done = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::Message {
                    block_type,
                    status: Some(s),
                    payload,
                    tool_kind,
                    ..
                } if block_type == "tool_call" && s == "completed" => {
                    Some((payload.clone(), tool_kind.clone()))
                }
                _ => None,
            })
            .expect("expected a completed tool_call Message event");
        assert_eq!(done.0["name"], "Bash");
        assert_eq!(done.0["output"], "output text");
        assert_eq!(done.1.as_deref(), Some("execute"));

        // And the persisted row kept name + input alongside the output.
        let msgs = store.list_messages(&session_pk).await.unwrap();
        let tc = msgs.iter().find(|m| m.block_type == "tool_call").expect("tool_call row");
        assert_eq!(tc.payload["name"], "Bash");
        assert_eq!(tc.payload["output"], "output text");
        assert_eq!(tc.tool_kind.as_deref(), Some("execute"));
    }

    #[tokio::test]
    async fn user_turn_is_broadcast_live() {
        use crate::domain::CoreEvent;

        let (_store, _session_pk, events) =
            crate::harness::acp::testkit::run_via_harness_trait_collecting_events("hello there")
                .await;

        let user = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::Message { role, block_type, payload, seq, .. }
                    if role == "user" && block_type == "text" =>
                {
                    Some((payload.clone(), *seq))
                }
                _ => None,
            })
            .expect("expected a live user-turn Message event");
        assert_eq!(user.0["text"], "hello there");
        assert!(user.1 >= 1, "user turn must carry a real DB seq");
    }
}
