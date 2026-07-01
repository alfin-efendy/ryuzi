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
fn decode(notification: SessionNotification) -> AcpUpdate {
    let session_pk = notification.session_id.0.to_string();

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

/// Process one [`SessionNotification`], persisting to the store and emitting
/// a [`CoreEvent::Message`] on success.
///
/// - The `session_pk` key is derived from `notification.session_id`.
/// - Store errors are logged with [`tracing::warn!`] and swallowed; the
///   broadcast send failure (no subscribers) is also silently ignored.
pub async fn handle(notification: SessionNotification, sink: &NotificationSink) {
    match decode(notification) {
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
                Ok(()) => {
                    let _ = sink.events.send(CoreEvent::Message {
                        session_pk: session_pk.clone(),
                        seq: -1, // synthetic — the row already has a real seq
                        role: "assistant".into(),
                        block_type: "tool_call".into(),
                        payload: output_payload,
                        tool_call_id: Some(id),
                        status: Some(status.into()),
                        tool_kind: None,
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
}
